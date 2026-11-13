use openrussh_cli_config::Session;
use openrussh_cli_terminal::{self as terminal, Event, Events, Input, InputMode, Terminal};

use openrussh_client::config::RequestTty;
use openrussh_client::connect::Connection;
use openrussh_client::process::{Child, ChildIn, ChildOut, Chunk, cmd};
use openrussh_client::russh::client::Handler;

use anyhow::{Context as _, Result};

pub(crate) async fn run<H: Handler>(
    conn: &Connection<H>,
    session: Session,
    stdin_null: bool,
    escape_char: Option<u8>,
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
    let mut child = match session {
        Session::Shell => {
            tracing::debug!("Requesting shell.");
            conn.shell().await?
        }
        Session::Exec { command } => {
            tracing::debug!("Sending command: {command}");
            cmd::exec(conn.handle(), command, conn.request_tty()).await?
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
    use super::{Escape, Escapes, escape_help, escape_key, input_closed};

    use openrussh_client::russh::Error;

    use std::io::{self, ErrorKind};

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
