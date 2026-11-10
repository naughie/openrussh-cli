fn main() {
    let mut rt = t::runtime();

    rt.register("stdout, stderr and exit status", async || {
        let client = t::Client::new("")?;
        let output = client
            .run(
                &[
                    "target",
                    "sh",
                    "-c",
                    "printf stdout; printf stderr >&2; exit 7",
                ],
                b"",
            )
            .await?;
        t::expect(&output, 7, b"stdout", b"stderr");

        Ok(())
    });

    rt.register("command arguments are passed literally", async || {
        let client = t::Client::new("")?;
        let output = client
            .run(
                &[
                    "target", "printf", "<%s>\\n", "", "a b", "$HOME",
                    "$(printf expanded)", "a'b\"c", "x; exit 7", "|", "*", "line\nbreak",
                ],
                b"",
            )
            .await?;
        t::expect(
            &output, 0,
            b"<>\n<a b>\n<$HOME>\n<$(printf expanded)>\n<a'b\"c>\n<x; exit 7>\n<|>\n<*>\n<line\nbreak>\n",
            b"",
        );

        Ok(())
    });

    rt.register("shell stdin", async || {
        let client = t::Client::new("")?;
        let output = client.run(&["target"], b"printf shell; exit 3\n").await?;
        t::expect(&output, 3, b"shell", b"");

        Ok(())
    });

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
                    "sh",
                    "-c",
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
            let output = client
                .run(&["target", "sh", "-c", &cmd], &vec![b'x'; 8388608])
                .await?;
            t::expect(&output, code & 255, b"", b"");
        }

        Ok(())
    });

    rt.run();
}
