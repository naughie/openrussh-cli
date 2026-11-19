use t::tokio::time::{Duration, timeout};

use std::fs;
use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _, symlink};
use std::path::Path;
use std::process::Output;

async fn copy(client: &t::Client, args: &[&str]) -> t::Result<Output> {
    Ok(timeout(Duration::from_secs(15), client.command_scp(args).output()).await??)
}

fn shared_directory(path: &Path) -> t::Result<()> {
    fs::create_dir(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o777))?;

    Ok(())
}

async fn cleanup(client: &t::Client, path: &Path) -> t::Result<()> {
    let path = path.to_str().expect("UTF-8 temporary path");
    let command = format!("rm -rf -- '{}'", path.replace('\'', "'\\''"));
    let output = client.run(&["target", &command], b"").await?;
    t::expect(&output, 0, b"", b"");

    Ok(())
}

fn main() {
    let mut rt = t::runtime();

    rt.register("scp help describes recursive copying", async || {
        let client = t::Client::new("")?;
        let output = copy(&client, &["--help"]).await?;
        assert!(output.status.success());
        let help = String::from_utf8_lossy(&output.stdout);
        assert!(help.contains("Usage: scp"));
        assert!(help.contains("-r"));

        Ok(())
    });

    rt.register(
        "file uploads and downloads preserve bytes and literal names",
        async || {
            let client = t::Client::new("")?;
            let dir = t::tempdir()?;
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777))?;
            let source = dir.path().join("source");
            let remote = dir.path().join("remote");
            let downloaded = dir.path().join("downloaded");
            shared_directory(&remote)?;

            for data in [
                Vec::new(),
                b"\0binary\xff\r\n".to_vec(),
                vec![b'x'; 2_097_153],
            ] {
                fs::write(&source, &data)?;
                let path = remote.join("literal '字' ;$(printf injected)");
                let destination = format!("target:{}", path.display());
                let output = copy(&client, &[source.to_str().unwrap(), &destination]).await?;
                t::expect(&output, 0, b"", b"");
                assert_eq!(fs::read(&path)?, data);

                let output = copy(&client, &[&destination, downloaded.to_str().unwrap()]).await?;
                t::expect(&output, 0, b"", b"");
                assert_eq!(fs::read(&downloaded)?, data);
            }

            cleanup(&client, &remote).await?;

            Ok(())
        },
    );

    rt.register(
        "recursive uploads and downloads retain nested and empty directories",
        async || {
            let client = t::Client::new("")?;
            let dir = t::tempdir()?;
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777))?;
            let source = dir.path().join("source tree");
            fs::create_dir_all(source.join("nested/empty"))?;
            fs::write(source.join("root file"), b"root\n")?;
            fs::write(source.join("nested/子 'file'"), b"nested\0\xff")?;
            let remote = dir.path().join("remote tree");
            let destination = format!("target:{}", remote.display());

            let output = copy(&client, &["-r", source.to_str().unwrap(), &destination]).await?;
            t::expect(&output, 0, b"", b"");
            assert_eq!(fs::read(remote.join("root file"))?, b"root\n");
            assert_eq!(fs::read(remote.join("nested/子 'file'"))?, b"nested\0\xff");
            assert!(remote.join("nested/empty").is_dir());

            let downloaded = dir.path().join("downloaded tree");
            let output = copy(&client, &["-r", &destination, downloaded.to_str().unwrap()]).await?;
            t::expect(&output, 0, b"", b"");
            assert_eq!(fs::read(downloaded.join("root file"))?, b"root\n");
            assert_eq!(
                fs::read(downloaded.join("nested/子 'file'"))?,
                b"nested\0\xff"
            );
            assert!(downloaded.join("nested/empty").is_dir());
            assert_eq!(fs::read_dir(downloaded.join("nested/empty"))?.count(), 0);

            let output = copy(&client, &["-r", source.to_str().unwrap(), &destination]).await?;
            t::expect(&output, 0, b"", b"");
            assert_eq!(
                fs::read(remote.join("source tree/nested/子 'file'"))?,
                b"nested\0\xff"
            );
            assert!(remote.join("source tree/nested/empty").is_dir());

            let output = copy(&client, &["-r", &destination, downloaded.to_str().unwrap()]).await?;
            t::expect(&output, 0, b"", b"");
            assert_eq!(
                fs::read(downloaded.join("remote tree/root file"))?,
                b"root\n"
            );
            assert!(downloaded.join("remote tree/nested/empty").is_dir());

            cleanup(&client, &remote).await?;

            Ok(())
        },
    );

    rt.register(
        "multiple files copy into an existing destination directory",
        async || {
            let client = t::Client::new("")?;
            let dir = t::tempdir()?;
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777))?;
            let first = dir.path().join("first");
            let second = dir.path().join("second file");
            fs::write(&first, b"first")?;
            fs::write(&second, b"second")?;
            let remote = dir.path().join("remote");
            shared_directory(&remote)?;
            let destination = format!("target:{}", remote.display());

            let output = copy(
                &client,
                &[
                    first.to_str().unwrap(),
                    second.to_str().unwrap(),
                    &destination,
                ],
            )
            .await?;
            t::expect(&output, 0, b"", b"");
            assert_eq!(fs::read(remote.join("first"))?, b"first");
            assert_eq!(fs::read(remote.join("second file"))?, b"second");

            let downloaded = dir.path().join("downloaded");
            fs::create_dir(&downloaded)?;
            let first = format!("target:{}/first", remote.display());
            let second = format!("target:{}/second file", remote.display());
            let output = copy(&client, &[&first, &second, downloaded.to_str().unwrap()]).await?;
            t::expect(&output, 0, b"", b"");
            assert_eq!(fs::read(downloaded.join("first"))?, b"first");
            assert_eq!(fs::read(downloaded.join("second file"))?, b"second");

            cleanup(&client, &remote).await?;

            Ok(())
        },
    );

    rt.register(
        "file symlinks are followed and directory cycles fail",
        async || {
            let client = t::Client::new("")?;
            let dir = t::tempdir()?;
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777))?;
            let source = dir.path().join("source");
            fs::write(&source, b"symlink target")?;
            let link = dir.path().join("source link");
            symlink("source", &link)?;
            let remote = dir.path().join("remote");
            shared_directory(&remote)?;
            let uploaded = remote.join("uploaded");
            let destination = format!("target:{}", uploaded.display());

            let output = copy(&client, &[link.to_str().unwrap(), &destination]).await?;
            t::expect(&output, 0, b"", b"");
            assert_eq!(fs::read(&uploaded)?, b"symlink target");
            assert!(fs::symlink_metadata(&uploaded)?.is_file());

            let remote_link = remote.join("remote link");
            symlink("uploaded", &remote_link)?;
            let source = format!("target:{}", remote_link.display());
            let downloaded = dir.path().join("downloaded");
            let output = copy(&client, &[&source, downloaded.to_str().unwrap()]).await?;
            t::expect(&output, 0, b"", b"");
            assert_eq!(fs::read(&downloaded)?, b"symlink target");
            assert!(fs::symlink_metadata(&downloaded)?.is_file());

            let cycle = dir.path().join("cycle");
            fs::create_dir(&cycle)?;
            symlink(".", cycle.join("again"))?;
            let destination = format!("target:{}/cycle upload", remote.display());
            let output = copy(&client, &["-r", cycle.to_str().unwrap(), &destination]).await?;
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8_lossy(&output.stderr).contains("Directory cycle"));

            let remote_cycle = remote.join("cycle");
            shared_directory(&remote_cycle)?;
            symlink(".", remote_cycle.join("again"))?;
            let source = format!("target:{}", remote_cycle.display());
            let downloaded = dir.path().join("cycle download");
            let output = copy(&client, &["-r", &source, downloaded.to_str().unwrap()]).await?;
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8_lossy(&output.stderr).contains("Directory cycle"));

            cleanup(&client, &remote).await?;

            Ok(())
        },
    );

    rt.register(
        "directories require -r and missing sources fail",
        async || {
            let client = t::Client::new("")?;
            let dir = t::tempdir()?;
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777))?;
            let source = dir.path().join("source");
            fs::create_dir(&source)?;
            let destination = dir.path().join("destination");
            let remote_source = format!("target:{}", source.display());
            let remote_missing = format!("target:{}/missing", dir.path().display());
            let remote_destination = format!("target:{}", destination.display());
            let missing = dir.path().join("missing");

            for (args, diagnostic) in [
                (
                    vec![source.to_str().unwrap(), remote_destination.as_str()],
                    "is a directory (use -r)",
                ),
                (
                    vec![remote_source.as_str(), destination.to_str().unwrap()],
                    "is a directory (use -r)",
                ),
                (
                    vec![missing.to_str().unwrap(), remote_destination.as_str()],
                    "Read local source",
                ),
                (
                    vec![remote_missing.as_str(), destination.to_str().unwrap()],
                    "Read remote source",
                ),
            ] {
                let output = copy(&client, &args).await?;
                assert!(!output.status.success(), "{args:?}");
                assert!(output.stdout.is_empty(), "{args:?}");
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains(diagnostic),
                    "{args:?}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            assert!(!destination.exists());

            Ok(())
        },
    );

    rt.register(
        "recursive dot and parent sources use their directory names",
        async || {
            let client = t::Client::new("")?;
            let dir = t::tempdir()?;
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777))?;
            let source = dir.path().join("source");
            fs::create_dir_all(source.join("child"))?;
            fs::write(source.join("file"), b"directory content")?;
            let remote = dir.path().join("remote");
            shared_directory(&remote)?;
            let destination = format!("target:{}", remote.display());
            let dot = source.join(".");

            let output = copy(&client, &["-r", dot.to_str().unwrap(), &destination]).await?;
            t::expect(&output, 0, b"", b"");
            assert_eq!(fs::read(remote.join("file"))?, b"directory content");
            assert!(remote.join("child").is_dir());

            let downloaded = dir.path().join("downloaded");
            fs::create_dir(&downloaded)?;
            let dot = format!("target:{}/.", remote.display());
            let output = copy(&client, &["-r", &dot, downloaded.to_str().unwrap()]).await?;
            t::expect(&output, 0, b"", b"");
            assert_eq!(fs::read(downloaded.join("file"))?, b"directory content");
            assert!(downloaded.join("child").is_dir());

            let parent = source.join("child/..");
            let output = copy(&client, &["-r", parent.to_str().unwrap(), &destination]).await?;
            t::expect(&output, 0, b"", b"");
            assert_eq!(fs::read(remote.join("source/file"))?, b"directory content");

            let parent = format!("target:{}/source/child/..", remote.display());
            let output = copy(&client, &["-r", &parent, downloaded.to_str().unwrap()]).await?;
            t::expect(&output, 0, b"", b"");
            assert_eq!(
                fs::read(downloaded.join("source/file"))?,
                b"directory content"
            );

            cleanup(&client, &remote).await?;

            Ok(())
        },
    );

    rt.register(
        "special-file destinations fail before opening them",
        async || {
            let client = t::Client::new("")?;
            let dir = t::tempdir()?;
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777))?;
            let source = dir.path().join("source");
            fs::write(&source, b"regular file")?;
            let remote_fifo = dir.path().join("remote-fifo");
            let local_fifo = dir.path().join("local-fifo");
            let command = format!(
                "mkfifo -m 666 -- '{}' '{}'",
                remote_fifo.display(),
                local_fifo.display()
            );
            let output = client.run(&["target", &command], b"").await?;
            t::expect(&output, 0, b"", b"");

            let destination = format!("target:{}", remote_fifo.display());
            let output = copy(&client, &[source.to_str().unwrap(), &destination]).await?;
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("Remote destination is not a regular file")
            );
            assert!(fs::symlink_metadata(&remote_fifo)?.file_type().is_fifo());

            let source = format!("target:{}", source.display());
            let output = copy(&client, &[&source, local_fifo.to_str().unwrap()]).await?;
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("Local destination is not a regular file")
            );
            assert!(fs::symlink_metadata(&local_fifo)?.file_type().is_fifo());

            Ok(())
        },
    );

    rt.register("scp reuses configuration formats and SSH overrides", async || {
        assert!(std::env::var_os("PATH").is_some_and(|path| !path.is_empty()));
        let dir = t::tempdir()?;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777))?;
        let source = dir.path().join("source");
        fs::write(&source, b"configuration")?;
        let remote = dir.path().join("remote");
        shared_directory(&remote)?;
        let destination = format!("target:{}/copied", remote.display());
        let clients = [
            t::Client::settings(" HostName 127.0.0.2\n User nobody\n Port 1\n")?,
            t::Client::toml("[hosts.target]\nhost-name = '127.0.0.2'\nuser = 'nobody'\nport = 1\n")?,
            t::Client::lua("return {hosts = {target = {host_name = '127.0.0.2', user = 'nobody', port = 1}}}")?,
        ];

        for (client, port_flag) in clients.iter().zip(["-p", "-P", "-p"]) {
            let output = copy(client, &[
                "-4", "-o", "HostName=127.0.0.1", "-o", "User=bob", "-o", "Port=2222",
                port_flag, "1",
                "-i", "/home/alice/.ssh/id_ed25519", "-o", "IdentityAgent=none",
                "-o", "UserKnownHostsFile=/home/alice/.ssh/known_hosts",
                "-o", "SendEnv=PATH",
                source.to_str().unwrap(), &destination,
            ]).await?;
            t::expect(&output, 0, b"", b"");
            assert_eq!(fs::read(remote.join("copied"))?, b"configuration");
        }

        let client = t::Client::new("")?;
        cleanup(&client, &remote).await?;

        Ok(())
    });

    rt.run();
}
