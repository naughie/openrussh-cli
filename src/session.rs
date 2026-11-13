use openrussh_cli_config::Session;
use openrussh_cli_terminal::{self as terminal, Event, Events, Input, InputMode, Terminal};

use crate::connection::Connection;
use openrussh_client::config::RequestTty;
use openrussh_client::process::{Child, ChildIn, ChildOut, Chunk, cmd, shell};
use openrussh_client::russh::client::Handler;

use anyhow::{Context as _, Result};

pub(crate) async fn run<H: Handler>(
    conn: &Connection<H>,
    session: Session,
    stdin_null: bool,
    escape_char: Option<u8>,
    send_env: Option<&[String]>,
    events: &mut Events,
) -> Result<u8> {
    use std::os::fd::AsFd as _;

    if matches!(session, Session::None) {
        tracing::debug!("Entering forwarding-only session.");
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(200));

        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if conn.handle().is_closed() {
                        return Ok(255);
                    }
                }
                event = events.recv() => match event {
                    Event::Suspend => terminal::suspend()?,
                    Event::Resize | Event::Continue => {},
                }
            }
        }
    }

    let request_pty = match conn.request_tty() {
        RequestTty::Never => false,
        RequestTty::Always => true,
        RequestTty::IfTty => {
            use std::io::IsTerminal as _;
            std::io::stdin().is_terminal()
        }
        RequestTty::ForShell => matches!(session, Session::Shell),
    };

    let stdio_forwarding = matches!(session, Session::Forward { .. });
    let environment = send_env
        .filter(|patterns| !patterns.is_empty() && !stdio_forwarding)
        .map(|patterns| selected_environment(patterns, std::env::vars_os()))
        .unwrap_or_default();
    let mut child = match session {
        Session::Shell => {
            tracing::debug!("Requesting shell.");
            start_process(conn, None, request_pty, &environment).await?
        }
        Session::Exec { command } => {
            tracing::debug!("Sending command: {command}");
            start_process(conn, Some(command), request_pty, &environment).await?
        }
        Session::Forward { host, port } => {
            tracing::debug!("Opening stdio forwarding to {host} port {port}.");
            let channel = conn
                .handle()
                .channel_open_direct_tcpip(host, u32::from(port), "127.0.0.1", 0)
                .await?;
            Child::new(channel)
        }
        Session::None => unreachable!(),
    };

    let stdin = std::io::stdin();
    let fd = stdin.as_fd();

    let interactive = request_pty && !stdio_forwarding;
    let term = Terminal::new(fd, interactive && !stdin_null)?;
    tracing::debug!("Entering session.");
    {
        let (writer, reader) = child.channel();
        let input = if stdin_null {
            None
        } else {
            Some(Input::new(fd)?)
        };

        let mut mode = input.as_ref().and_then(Input::mode);

        let input = send_input(
            input,
            writer,
            &term,
            mode,
            if interactive { escape_char } else { None },
        );

        let output = read_output(reader);
        tokio::pin!(input, output);
        let mut input_done = false;

        loop {
            tokio::select! {
                res = &mut output => {
                    res?;
                    break;
                }
                res = &mut input, if !input_done => {
                    input_done = true;
                    mode = None;

                    match res {
                        Ok(InputEnd::Eof) => {},
                        Ok(InputEnd::Disconnect) => {
                            let _ = writer.channel().close().await;

                            return Ok(255);
                        }
                        Err(e) if input_closed(&e) => {},
                        Err(e) => return Err(e),
                    }
                }
                event = events.recv() => match event {
                    Event::Resize | Event::Continue => {
                        term.enter()?;

                        if interactive {
                            let (w, h) = term.size();
                            writer.channel().window_change(w, h, 0, 0).await?;
                        }
                    }
                    Event::Suspend => {
                        suspend_local(&term, mode)?;
                        if interactive {
                            let (w, h) = term.size();
                            writer.channel().window_change(w, h, 0, 0).await?;
                        }
                    }
                }
            }
        }
    }

    Ok(u8::try_from(child.status().code().unwrap_or(255)).unwrap_or(255))
}

async fn start_process<H: Handler>(
    conn: &Connection<H>,
    command: Option<String>,
    request_pty: bool,
    environment: &[(String, String)],
) -> Result<Child> {
    if environment.is_empty() {
        return match command {
            Some(command) => Ok(cmd::exec(conn.handle(), command, conn.request_tty()).await?),
            None => Ok(shell::shell(conn.handle(), conn.request_tty()).await?),
        };
    }

    // The dependency's process helpers open and execute a channel in one step.
    // SendEnv needs protocol requests on that channel before shell/exec starts.
    let channel = conn.handle().channel_open_session().await?;
    if request_pty {
        use std::io::IsTerminal as _;
        use std::os::fd::AsFd as _;

        let stdin = std::io::stdin();
        let ((columns, rows), modes) = if stdin.is_terminal() {
            let terminal = Terminal::new(stdin.as_fd(), false)?;
            (
                terminal.size(),
                terminal.attributes().ok().map(terminal_modes),
            )
        } else {
            ((80, 24), None)
        };
        let term = std::env::var("TERM").unwrap_or_else(|_| "xterm".to_owned());
        let modes = modes.as_ref().map_or(&[][..], |modes| modes.as_slice());
        channel
            .request_pty(true, &term, columns, rows, 0, 0, modes)
            .await?;
    }
    for (name, value) in environment {
        // Like OpenSSH, let server AcceptEnv policy silently refuse variables.
        channel.set_env(false, name, value).await?;
    }
    match command {
        Some(command) => channel.exec(true, command).await?,
        None => channel.request_shell(true).await?,
    }
    Ok(Child::new(channel))
}

fn terminal_modes(attributes: terminal::Attributes) -> [(openrussh_client::russh::Pty, u32); 11] {
    use openrussh_client::russh::Pty;
    [
        (Pty::VINTR, u32::from(attributes.interrupt)),
        (Pty::VQUIT, u32::from(attributes.quit)),
        (Pty::VERASE, u32::from(attributes.erase)),
        (Pty::VKILL, u32::from(attributes.kill_line)),
        (Pty::VEOF, u32::from(attributes.end_of_file)),
        (Pty::ECHO, u32::from(attributes.echo)),
        (Pty::ICANON, u32::from(attributes.canonical_input)),
        (Pty::ISIG, u32::from(attributes.signals)),
        (Pty::ICRNL, u32::from(attributes.map_carriage_return)),
        (Pty::IXON, u32::from(attributes.software_flow_control)),
        (Pty::OPOST, u32::from(attributes.postprocess_output)),
    ]
}

fn selected_environment(
    patterns: &[String],
    variables: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(String, String)> {
    let mut selected: Vec<&str> = Vec::new();
    for pattern in patterns {
        if let Some(remove) = pattern.strip_prefix('-') {
            selected.retain(|pattern| !wildcard_matches(remove.as_bytes(), pattern.as_bytes()));
        } else {
            selected.push(pattern);
        }
    }
    if selected.is_empty() {
        return Vec::new();
    }
    variables
        .into_iter()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(name, _)| {
            selected
                .iter()
                .any(|pattern| wildcard_matches(pattern.as_bytes(), name.as_bytes()))
        })
        .collect()
}

fn wildcard_matches(pattern: &[u8], name: &[u8]) -> bool {
    let (mut pattern_index, mut name_index) = (0, 0);
    let mut star = None;
    while name_index < name.len() {
        match pattern.get(pattern_index) {
            Some(b'?') => {
                pattern_index += 1;
                name_index += 1;
            }
            Some(b'*') => {
                star = Some((pattern_index, name_index));
                pattern_index += 1;
            }
            Some(byte) if *byte == name[name_index] => {
                pattern_index += 1;
                name_index += 1;
            }
            _ => {
                let Some((star_index, consumed)) = star else {
                    return false;
                };
                name_index = consumed + 1;
                pattern_index = star_index + 1;
                star = Some((star_index, name_index));
            }
        }
    }
    pattern[pattern_index..].iter().all(|byte| *byte == b'*')
}

enum InputEnd {
    Eof,
    Disconnect,
}

async fn send_input(
    input: Option<Input<'_>>,
    writer: ChildIn<'_>,
    term: &Terminal<'_>,
    mode: Option<InputMode<'_>>,
    escape_char: Option<u8>,
) -> Result<InputEnd> {
    let Some(mut input) = input else {
        writer.eof().await?;

        return Ok(InputEnd::Eof);
    };

    let mut buf = [0; 8192];
    let mut escapes = Escapes::new(escape_char);

    loop {
        let n = input.read(&mut buf).await.context("Could not read stdin")?;

        if n == 0 {
            if let Some(key) = escapes.finish() {
                writer.write_stdin_io(&[key][..]).await?;
            }
            writer.eof().await?;

            return Ok(InputEnd::Eof);
        }

        if escapes
            .write(&buf[..n], writer, term, mode)
            .await?
            .is_break()
        {
            return Ok(InputEnd::Disconnect);
        }
    }
}

fn suspend_local(term: &Terminal<'_>, mode: Option<InputMode<'_>>) -> std::io::Result<()> {
    if let Some(mode) = mode {
        mode.pause()?;
    }
    term.suspend()?;

    if let Some(mode) = mode {
        mode.resume()?;
    }

    Ok(())
}

fn input_closed(e: &anyhow::Error) -> bool {
    use openrussh_client::russh::Error;
    use std::io::ErrorKind;

    match e.downcast_ref::<Error>() {
        Some(Error::SendError) => true,
        Some(Error::IO(e)) => e.kind() == ErrorKind::BrokenPipe,
        _ => false,
    }
}

async fn read_output(mut reader: ChildOut<'_>) -> Result<()> {
    use tokio::io::AsyncWriteExt as _;

    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    while let Some(chunk) = reader.read_next().await {
        match chunk {
            Chunk::Stdout(data) => {
                stdout.write_all(&data).await?;
                stdout.flush().await?;
            }
            Chunk::Stderr(data) => {
                stderr.write_all(&data).await?;
                stderr.flush().await?;
            }
        }
    }

    Ok(())
}

enum Escapes {
    Disabled,
    Enabled { key: u8, state: EscapeState },
}

enum EscapeState {
    LineStart,
    MidLine,
    Prefix,
}

#[derive(Debug, PartialEq)]
enum Escape<'buf> {
    Data(&'buf [u8]),
    Literal(u8),
    Close,
    Suspend,
    Help(u8),
}

impl Escapes {
    fn new(key: Option<u8>) -> Self {
        match key {
            Some(key) => Self::Enabled {
                key,
                state: EscapeState::LineStart,
            },
            None => Self::Disabled,
        }
    }

    fn next<'buf>(&mut self, buf: &mut &'buf [u8]) -> Option<Escape<'buf>> {
        let (&first, rest) = buf.split_first()?;
        let Self::Enabled { key, state } = self else {
            return Some(Escape::Data(std::mem::take(buf)));
        };
        let key = *key;

        if matches!(state, EscapeState::Prefix) {
            *state = EscapeState::LineStart;

            if first == key {
                *buf = rest;
                *state = EscapeState::after(key);

                return Some(Escape::Literal(key));
            }

            let escape = match first {
                b'.' => Escape::Close,
                26 => Escape::Suspend,
                b'?' => Escape::Help(key),
                _ => {
                    *state = EscapeState::after(key);

                    return Some(Escape::Literal(key));
                }
            };
            *buf = rest;

            return Some(escape);
        }

        for (idx, &c) in buf.iter().enumerate() {
            if matches!(state, EscapeState::LineStart) && c == key {
                if idx != 0 {
                    let (data, rest) = buf.split_at(idx);
                    *buf = rest;

                    return Some(Escape::Data(data));
                }
                *state = EscapeState::Prefix;
                *buf = rest;

                return self.next(buf);
            }
            *state = EscapeState::after(c);
        }
        Some(Escape::Data(std::mem::take(buf)))
    }

    fn finish(&mut self) -> Option<u8> {
        match self {
            Self::Enabled {
                key,
                state: state @ EscapeState::Prefix,
            } => {
                *state = EscapeState::after(*key);
                Some(*key)
            }
            _ => None,
        }
    }

    async fn write(
        &mut self,
        mut buf: &[u8],
        writer: ChildIn<'_>,
        term: &Terminal<'_>,
        mode: Option<InputMode<'_>>,
    ) -> Result<std::ops::ControlFlow<()>> {
        use std::ops::ControlFlow;

        while let Some(escape) = self.next(&mut buf) {
            match escape {
                Escape::Data(data) => writer.write_stdin_io(data).await?,
                Escape::Literal(c) => writer.write_stdin_io(&[c][..]).await?,
                Escape::Close => return Ok(ControlFlow::Break(())),
                Escape::Suspend => {
                    suspend_local(term, mode)?;

                    let (w, h) = term.size();
                    writer.channel().window_change(w, h, 0, 0).await?;
                }
                Escape::Help(key) => {
                    eprintln!("{}", escape_help(key));
                }
            }
        }

        Ok(ControlFlow::Continue(()))
    }
}

impl EscapeState {
    fn after(byte: u8) -> Self {
        if matches!(byte, b'\r' | b'\n') {
            Self::LineStart
        } else {
            Self::MidLine
        }
    }
}

fn escape_key(key: u8) -> impl std::fmt::Display {
    use std::fmt;

    struct Key(u8);

    impl fmt::Display for Key {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self.0 {
                0..=31 => write!(f, "^{}", char::from(self.0 + b'@')),
                127 => f.write_str("^?"),
                c => write!(f, "{}", char::from(c)),
            }
        }
    }

    Key(key)
}

fn escape_help(key: u8) -> impl std::fmt::Display {
    use std::fmt;

    struct Help(u8);

    impl fmt::Display for Help {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let key = escape_key(self.0);
            f.write_str("\r\nSupported escape sequences:\r\n")?;

            if self.0 != b'.' {
                write!(f, "  {key}.   Close connection\r\n")?;
            }
            if self.0 != 26 {
                write!(f, "  {key}^Z  Suspend ssh\r\n")?;
            }
            write!(f, "  {key}{key}   Send the escape character\r\n")?;
            if self.0 != b'?' {
                write!(f, "  {key}?   Display this message\r\n")?;
            }

            f.write_str("\r")
        }
    }

    Help(key)
}

#[cfg(test)]
mod tests {
    use super::{Escape, Escapes, escape_help, escape_key, input_closed, selected_environment};

    use openrussh_client::russh::Error;

    use std::io::{self, ErrorKind};

    #[test]
    fn send_env_matches_names_and_removes_previously_configured_patterns() {
        let variables = || {
            [
                ("LANG", "ja_JP.UTF-8"),
                ("LC_TIME", "C"),
                ("LC_X", "one"),
                ("LC_XX", "two"),
                ("SECRET", "private"),
            ]
            .map(|(name, value)| (name.into(), value.into()))
        };
        let patterns = |values: &[&str]| {
            values
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            selected_environment(&[], variables()),
            Vec::<(String, String)>::new()
        );
        assert_eq!(
            selected_environment(&patterns(&["LANG", "LC_*", "-LC_*", "LC_?"]), variables()),
            [
                ("LANG".into(), "ja_JP.UTF-8".into()),
                ("LC_X".into(), "one".into())
            ]
        );
        // Removal matches stored patterns rather than subtracting variables
        // covered by an earlier wildcard, matching OpenSSH's SendEnv parsing.
        assert_eq!(
            selected_environment(&patterns(&["LC_*", "-LC_TIME"]), variables()).len(),
            3
        );
        assert_eq!(
            selected_environment(&patterns(&["LANG", "LANG"]), variables()),
            [("LANG".into(), "ja_JP.UTF-8".into())]
        );
    }

    #[test]
    fn send_env_preserves_values_and_skips_non_utf8_without_mutating_the_environment() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;
        let value = "quotes ' \" $(touch /tmp/never)\nsecond line";
        let variables = [
            (OsString::from("SELECTED"), OsString::from(value)),
            (OsString::from_vec(b"BAD\xff".to_vec()), "name".into()),
            ("BAD_VALUE".into(), OsString::from_vec(b"\xff".to_vec())),
        ];
        assert_eq!(
            selected_environment(&["*".into()], variables),
            [("SELECTED".into(), value.into())]
        );
    }

    #[tokio::test]
    async fn environment_requests_precede_shell_and_raw_exec_without_requiring_acceptance()
    -> anyhow::Result<()> {
        use super::start_process;
        use crate::connection::Connection;
        use openrussh_client::config::RequestTty;
        use openrussh_client::russh::{self, Channel, ChannelId, client, server};
        use std::sync::Arc;
        use tokio::sync::mpsc;
        use tokio::time::{Duration, timeout};

        struct Client;
        impl client::Handler for Client {
            type Error = russh::Error;
            async fn check_server_key(
                &mut self,
                _: &russh::keys::PublicKeyOrCertificate,
            ) -> Result<bool, Self::Error> {
                Ok(true)
            }
        }
        struct Server(mpsc::UnboundedSender<Vec<String>>);
        impl server::Handler for Server {
            type Error = russh::Error;

            async fn auth_none(&mut self, _: &str) -> Result<server::Auth, Self::Error> {
                Ok(server::Auth::Accept)
            }

            async fn channel_open_session(
                &mut self,
                _: Channel<server::Msg>,
                reply: server::ChannelOpenHandle,
                _: &mut server::Session,
            ) -> Result<(), Self::Error> {
                reply.accept().await;
                Ok(())
            }

            async fn env_request(
                &mut self,
                channel: ChannelId,
                name: &str,
                value: &str,
                session: &mut server::Session,
            ) -> Result<(), Self::Error> {
                self.0
                    .send(vec!["env".into(), name.into(), value.into()])
                    .unwrap();
                session.channel_failure(channel)
            }

            async fn shell_request(
                &mut self,
                channel: ChannelId,
                session: &mut server::Session,
            ) -> Result<(), Self::Error> {
                self.0.send(vec!["shell".into()]).unwrap();
                session.channel_success(channel)
            }

            async fn exec_request(
                &mut self,
                channel: ChannelId,
                command: &[u8],
                session: &mut server::Session,
            ) -> Result<(), Self::Error> {
                self.0
                    .send(vec![
                        "exec".into(),
                        String::from_utf8(command.to_vec()).unwrap(),
                    ])
                    .unwrap();
                session.channel_success(channel)
            }
        }

        let (client_stream, server_stream) = tokio::io::duplex(8192);
        let (sender, mut requests) = mpsc::unbounded_channel();
        let key =
            russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)?;
        let server = tokio::spawn(async move {
            server::run_stream(
                Arc::new(server::Config {
                    keys: vec![key],
                    ..Default::default()
                }),
                server_stream,
                Server(sender),
            )
            .await?
            .await
        });
        let mut handle =
            client::connect_stream(Arc::new(client::Config::default()), client_stream, Client)
                .await?;
        assert!(handle.authenticate_none("test").await?.success());
        let connection = Connection::Direct {
            handle,
            tty: RequestTty::Never,
        };
        let environment = [(
            "SELECTED".to_owned(),
            "quotes ' \"\n$(no shell expansion)".to_owned(),
        )];
        let command = "printf '%s\\n' \"$SELECTED\"; printf raw";
        for operation in [Some(command.to_owned()), None] {
            let expected = match &operation {
                Some(command) => vec!["exec".to_owned(), command.clone()],
                None => vec!["shell".to_owned()],
            };
            let _child = start_process(&connection, operation, false, &environment).await?;
            assert_eq!(
                timeout(Duration::from_secs(5), requests.recv())
                    .await?
                    .unwrap(),
                vec![
                    "env".to_owned(),
                    environment[0].0.clone(),
                    environment[0].1.clone()
                ]
            );
            assert_eq!(
                timeout(Duration::from_secs(5), requests.recv())
                    .await?
                    .unwrap(),
                expected
            );
        }
        connection
            .disconnect(russh::Disconnect::ByApplication, "", "")
            .await?;
        timeout(Duration::from_secs(5), server).await???;
        Ok(())
    }

    fn decode(chunks: &[&[u8]], key: Option<u8>) -> (Vec<u8>, Vec<&'static str>) {
        let mut parser = Escapes::new(key);
        let mut data = Vec::new();
        let mut actions = Vec::new();

        for &chunk in chunks {
            let mut chunk = chunk;
            while let Some(escape) = parser.next(&mut chunk) {
                match escape {
                    Escape::Data(bytes) => data.extend_from_slice(bytes),
                    Escape::Literal(c) => data.push(c),
                    Escape::Close => {
                        actions.push("close");

                        return (data, actions);
                    }
                    Escape::Suspend => actions.push("suspend"),
                    Escape::Help(_) => actions.push("help"),
                }
            }
        }

        if let Some(key) = parser.finish() {
            data.push(key);
        }
        (data, actions)
    }

    #[test]
    fn ordinary_input_is_unchanged() {
        for data in [b"ordinary input".as_slice(), b"abc~.def", b"\xff\0bytes"] {
            assert_eq!(decode(&[data], Some(b'~')), (data.to_vec(), vec![]));
        }
    }

    #[test]
    fn escapes_require_a_line_start() {
        let (data, actions) = decode(&[b"abc~.\nline\r~.ignored"], Some(b'~'));

        assert_eq!(data, b"abc~.\nline\r");

        assert_eq!(actions, ["close"]);
    }

    #[test]
    fn split_escape_sequences_keep_their_meaning() {
        let data = b"line\n~?~\x1a~~literal\n~.ignored";
        let expected = (
            b"line\n~literal\n".to_vec(),
            vec!["help", "suspend", "close"],
        );

        for split in 0..=data.len() {
            assert_eq!(
                decode(&[&data[..split], &data[split..]], Some(b'~')),
                expected
            );
        }

        let bytes: Vec<_> = data.chunks(1).collect();

        assert_eq!(decode(&bytes, Some(b'~')), expected);
    }

    #[test]
    fn doubled_escape_is_literal() {
        assert_eq!(decode(&[b"~~.~."], Some(b'~')), (b"~.~.".to_vec(), vec![]));
    }

    #[test]
    fn doubled_action_character_prefixes_are_literal() {
        for key in [b'.', b'?', 26] {
            let input = [key, key, b'\n', key, b'x'];

            for split in 0..=input.len() {
                assert_eq!(
                    decode(&[&input[..split], &input[split..]], Some(key)),
                    (vec![key, b'\n', key, b'x'], vec![])
                );
            }
        }
    }

    #[test]
    fn unknown_and_unfinished_escapes_are_preserved() {
        for chunks in [vec![b"~x\n~".as_slice()], vec![b"~", b"x\n", b"~"]] {
            assert_eq!(decode(&chunks, Some(b'~')), (b"~x\n~".to_vec(), vec![]));
        }
    }

    #[test]
    fn finishing_an_escape_is_idempotent() {
        let mut parser = Escapes::new(Some(b'~'));
        let mut buf = b"~".as_slice();

        assert_eq!(parser.next(&mut buf), None);

        assert_eq!(parser.finish(), Some(b'~'));

        assert_eq!(parser.finish(), None);
    }

    #[test]
    fn disabled_escapes_forward_every_byte() {
        let data = b"~.\n~?~\x1a~~";

        assert_eq!(decode(&[data], None), (data.to_vec(), vec![]));
    }

    #[test]
    fn custom_escapes_keep_default_character_literal() {
        assert_eq!(
            decode(&[b"~.\n!.ignored"], Some(b'!')),
            (b"~.\n".to_vec(), vec!["close"])
        );

        assert_eq!(decode(&[b"\x01."], Some(1)), (vec![], vec!["close"]));
    }

    #[test]
    fn literal_newline_escape_starts_a_new_line() {
        for key in *b"\r\n" {
            let input = [key, key, key, b'.'];

            for split in 0..=input.len() {
                assert_eq!(
                    decode(&[&input[..split], &input[split..]], Some(key)),
                    (vec![key], vec!["close"])
                );
            }
        }
    }

    #[test]
    fn help_displays_printable_and_control_keys() {
        for (key, expected) in [
            (b'~', "~"),
            (b'!', "!"),
            (0, "^@"),
            (1, "^A"),
            (31, "^_"),
            (127, "^?"),
        ] {
            assert_eq!(escape_key(key).to_string(), expected);
        }
    }

    #[test]
    fn help_omits_actions_shadowed_by_a_doubled_prefix() {
        for (key, shadowed) in [(b'.', "Close"), (b'?', "Display"), (26, "Suspend")] {
            let help = escape_help(key).to_string();

            assert!(!help.contains(shadowed));
            assert!(help.contains("Send the escape character"));
        }
        let help = escape_help(b'~').to_string();

        for action in ["Close", "Suspend", "Send", "Display"] {
            assert!(help.contains(action));
        }
    }

    #[test]
    fn remote_input_closure_can_preserve_exit_status() {
        assert!(input_closed(&Error::SendError.into()));

        assert!(input_closed(
            &Error::IO(io::Error::from(ErrorKind::BrokenPipe)).into()
        ));
    }

    #[test]
    fn local_input_and_other_ssh_errors_remain_failures() {
        for e in [
            anyhow::Error::from(io::Error::from(ErrorKind::BrokenPipe)),
            Error::IO(io::Error::from(ErrorKind::PermissionDenied)).into(),
            Error::Inconsistent.into(),
        ] {
            assert!(!input_closed(&e));
        }
    }
}
