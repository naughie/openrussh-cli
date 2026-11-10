use crate::config::Session;
use crate::terminal::{self, Event, Events, Input, InputMode, Terminal};

use openrussh_client::config::RequestTty;
use openrussh_client::connect::Connection;
use openrussh_client::process::{Child, ChildIn, ChildOut, Chunk, Command};
use openrussh_client::russh::client::Handler;

use anyhow::{Context as _, Result};

pub(crate) async fn run<H: Handler>(
    conn: &Connection<H>,
    session: Session,
    stdin_null: bool,
    escape_char: Option<u8>,
    events: &mut Events,
) -> Result<u8> {
    if matches!(session, Session::None) {
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

    use std::os::fd::AsFd as _;

    let tty = match conn.request_tty() {
        RequestTty::Never => false,
        RequestTty::Always => true,
        RequestTty::IfTty => {
            use std::io::IsTerminal as _;
            std::io::stdin().is_terminal()
        }
        RequestTty::ForShell => matches!(session, Session::Shell),
    };

    let forwarding = matches!(session, Session::Forward { .. });
    let mut child = match session {
        Session::Shell => conn.shell().await?,
        Session::Exec { prog, args } => {
            let mut cmd = Command::new().prog(&prog);

            for arg in args.iter().flatten() {
                cmd = cmd.arg(arg);
            }

            conn.exec(cmd.complete(false)).await?
        }
        Session::Forward { host, port } => {
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

    let interactive = tty && !forwarding;
    let term = Terminal::new(fd, interactive && !stdin_null)?;
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

                    let close = match res {
                        Ok(close) => close,
                        Err(e) if input_closed(&e) => false,
                        Err(e) => return Err(e),
                    };

                    if close {
                        let _ = writer.channel().close().await;

                        return Ok(255);
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
                        if let Some(mode) = mode { mode.pause()?; }
                        term.suspend()?;
                        if let Some(mode) = mode { mode.resume()?; }
                        if interactive {
                            let (w, h) = term.size();
                            writer.channel().window_change(w, h, 0, 0).await?;
                        }
                    }
                }
            }
        }
    }

    Ok(child.status().code().unwrap_or(255) as u8)
}

async fn send_input(
    input: Option<Input<'_>>,
    writer: ChildIn<'_>,
    term: &Terminal<'_>,
    mode: Option<InputMode<'_>>,
    escape_char: Option<u8>,
) -> Result<bool> {
    let Some(mut input) = input else {
        writer.eof().await?;

        return Ok(false);
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

            return Ok(false);
        }

        if escapes.write(&buf[..n], writer, term, mode).await? {
            return Ok(true);
        }
    }
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

struct Escapes {
    key: Option<u8>,
    pending: bool,
    line_start: bool,
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
        Self {
            key,
            pending: false,
            line_start: true,
        }
    }

    fn next<'buf>(&mut self, buf: &mut &'buf [u8]) -> Option<Escape<'buf>> {
        let (&first, rest) = buf.split_first()?;
        let Some(key) = self.key else {
            return Some(Escape::Data(std::mem::take(buf)));
        };

        if self.pending {
            self.pending = false;

            if first != key && !matches!(first, b'.' | 26 | b'?') {
                self.line_start = false;

                return Some(Escape::Literal(key));
            }
            *buf = rest;

            return Some(match first {
                b'.' => Escape::Close,
                26 => Escape::Suspend,
                b'?' => Escape::Help(key),
                _ => {
                    self.line_start = matches!(key, b'\r' | b'\n');
                    Escape::Literal(key)
                }
            });
        }

        for (idx, &c) in buf.iter().enumerate() {
            if self.line_start && c == key {
                if idx != 0 {
                    let (data, rest) = buf.split_at(idx);
                    *buf = rest;

                    return Some(Escape::Data(data));
                }
                self.pending = true;
                *buf = rest;

                return self.next(buf);
            }
            self.line_start = matches!(c, b'\r' | b'\n');
        }
        Some(Escape::Data(std::mem::take(buf)))
    }

    fn finish(&mut self) -> Option<u8> {
        std::mem::take(&mut self.pending)
            .then_some(self.key)
            .flatten()
    }

    async fn write(
        &mut self,
        mut buf: &[u8],
        writer: ChildIn<'_>,
        term: &Terminal<'_>,
        mode: Option<InputMode<'_>>,
    ) -> Result<bool> {
        while let Some(escape) = self.next(&mut buf) {
            match escape {
                Escape::Data(data) => writer.write_stdin_io(data).await?,
                Escape::Literal(c) => writer.write_stdin_io(&[c][..]).await?,
                Escape::Close => return Ok(true),
                Escape::Suspend => {
                    if let Some(mode) = mode {
                        mode.pause()?;
                    }
                    term.suspend()?;

                    if let Some(mode) = mode {
                        mode.resume()?;
                    }

                    let (w, h) = term.size();
                    writer.channel().window_change(w, h, 0, 0).await?;
                }
                Escape::Help(key) => {
                    let key = escape_key(key);
                    eprintln!(
                        "\r\nSupported escape sequences:\r\n  {key}.   Close connection\r\n  {key}^Z  Suspend ssh\r\n  {key}{key}   Send the escape character\r\n  {key}?   Display this message\r"
                    );
                }
            }
        }

        Ok(false)
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

#[cfg(test)]
mod tests {
    use super::{Escape, Escapes, escape_key, input_closed};

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
