async fn openssh(args: &[&str], data: &[u8]) -> t::Result<std::process::Output> {
    use t::tokio::io::AsyncWriteExt as _;

    let mut cmd = t::tokio::process::Command::new("/usr/bin/ssh");
    cmd.args(["-F", "/home/alice/.ssh/config", "-T"])
        .args(args)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn()?;
    let mut stdin = child.stdin.take().expect("Capture stdin");

    let input = async {
        match stdin.write_all(data).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(e) => return Err(e),
        }

        drop(stdin);

        Ok(())
    };

    let (_, output) = t::tokio::time::timeout(std::time::Duration::from_secs(15), async {
        t::tokio::try_join!(input, child.wait_with_output())
    })
    .await??;

    Ok(output)
}

struct CommandCase {
    args: &'static [&'static str],
    code: i32,
    stdout: &'static [u8],
    stderr: &'static [u8],
}

impl CommandCase {
    async fn compare(&self, client: &t::Client, data: &[u8]) -> t::Result<()> {
        let reference = openssh(self.args, data).await?;
        t::expect(&reference, self.code, self.stdout, self.stderr);

        let output = client.run(self.args, data).await?;
        let args = self.args;
        assert_eq!(output.status.code(), reference.status.code(), "{args:?}");
        assert_eq!(output.stdout, reference.stdout, "{args:?}");
        assert_eq!(output.stderr, reference.stderr, "{args:?}");

        Ok(())
    }
}

fn main() {
    let mut rt = t::runtime();

    rt.register("stdout, stderr and exit status", async || {
        let client = t::Client::new("")?;
        let output = client
            .run(&["target", "printf stdout; printf stderr >&2; exit 7"], b"")
            .await?;
        t::expect(&output, 7, b"stdout", b"stderr");

        Ok(())
    });

    rt.register("command arguments match OpenSSH shell semantics", async || {
        let client = t::Client::new("")?;
        let cases = [
            CommandCase {
                args: &["target", "echo hello"],
                code: 0,
                stdout: b"hello\n",
                stderr: b"",
            },
            CommandCase {
                args: &["target", "echo", "hello"],
                code: 0,
                stdout: b"hello\n",
                stderr: b"",
            },
            CommandCase {
                args: &["target", "printf", "'<%s>\\n'", "", "a b"],
                code: 0,
                stdout: b"<a>\n<b>\n",
                stderr: b"",
            },
            CommandCase {
                args: &["target", "printf '<%s>\\n' 'left", "", "right'"],
                code: 0,
                stdout: b"<left  right>\n",
                stderr: b"",
            },
            CommandCase {
                args: &[
                    "target",
                    "value='remote value'; printf '<%s>\\n' \"$value\" \"$(printf expanded)\" \"$HOME\"",
                ],
                code: 0,
                stdout: b"<remote value>\n<expanded>\n</home/bob>\n",
                stderr: b"",
            },
            CommandCase {
                args: &[
                    "target",
                    "printf pipe | cat; false && printf skipped; true && printf success; false || printf fallback; printf error >&2; exit 7",
                ],
                code: 7,
                stdout: b"pipesuccessfallback",
                stderr: b"error",
            },
            CommandCase {
                args: &[
                    "target",
                    "tmp=$(mktemp); printf redirected >\"$tmp\"; cat <\"$tmp\"; rm \"$tmp\"",
                ],
                code: 0,
                stdout: b"redirected",
                stderr: b"",
            },
            CommandCase {
                args: &[
                    "target",
                    "printf",
                    "'<%s>\\n'",
                    "''",
                    "'a b'",
                    "'$HOME'",
                    "'$(printf expanded)'",
                    "'a'\\''b\"c'",
                    "'x; exit 7'",
                    "'|'",
                    "'*'",
                    "'line\nbreak'",
                ],
                code: 0,
                stdout: b"<>\n<a b>\n<$HOME>\n<$(printf expanded)>\n<a'b\"c>\n<x; exit 7>\n<|>\n<*>\n<line\nbreak>\n",
                stderr: b"",
            },
            CommandCase {
                args: &["target", "printf before # trailing comment"],
                code: 0,
                stdout: b"before",
                stderr: b"",
            },
        ];

        for case in cases {
            case.compare(&client, b"").await?;
        }

        Ok(())
    });

    rt.register("shell stdin", async || {
        let client = t::Client::new("")?;
        let output = client.run(&["target"], b"printf shell; exit 3\n").await?;
        t::expect(&output, 3, b"shell", b"");

        Ok(())
    });

    rt.register(
        "empty joined command matches OpenSSH shell selection",
        async || {
            let client = t::Client::new("")?;
            let script = b"printf shell; printf error >&2; exit 3\n";
            let cases = [
                CommandCase {
                    args: &["-T", "target", ""],
                    code: 3,
                    stdout: b"shell",
                    stderr: b"error",
                },
                CommandCase {
                    args: &["-T", "target", "", ""],
                    code: 0,
                    stdout: b"",
                    stderr: b"",
                },
            ];

            for case in cases {
                case.compare(&client, script).await?;
            }

            Ok(())
        },
    );

    rt.register("stdin and null stdin", async || {
        let client = t::Client::new("")?;
        let output = client.run(&["target", "cat"], b"input\n").await?;
        t::expect(&output, 0, b"input\n", b"");

        let output = client.run(&["-n", "target", "cat"], b"discarded").await?;
        t::expect(&output, 0, b"", b"");

        Ok(())
    });

    rt.register("large duplex transfer", async || {
        let client = t::Client::new("")?;
        let output = client
            .run(
                &[
                    "target",
                    "head -c 8388608 /dev/zero; cat >/dev/null; printf done",
                ],
                &vec![b'x'; 8388608],
            )
            .await?;
        assert_eq!(output.status.code(), Some(0));

        assert_eq!(output.stdout.len(), 8388612);

        assert!(output.stdout[..8388608].iter().all(|&c| c == 0));

        assert_eq!(&output.stdout[8388608..], b"done");

        assert!(output.stderr.is_empty());

        Ok(())
    });

    rt.register("early remote exit", async || {
        let client = t::Client::new("")?;

        for code in [0, 7] {
            let cmd = format!("exit {code}");
            let output = client.run(&["target", &cmd], &vec![b'x'; 8388608]).await?;
            t::expect(&output, code & 255, b"", b"");
        }

        Ok(())
    });

    rt.run();
}
