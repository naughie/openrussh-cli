pub use anyhow::Result;
pub use nix::sys::signal::Signal;
pub use tempfile::tempdir;
pub use tokio;

mod runtime;
pub use runtime::Runtime;

mod pty;
pub use pty::PtyClient;

use anyhow::{Context as _, ensure};
use tempfile::NamedTempFile;
use tokio::process::{Child, Command};
use tokio::time::{Duration, timeout};

use std::process::{Output, Stdio};

const TIMEOUT: Duration = Duration::from_secs(15);

pub fn runtime() -> Runtime {
    Runtime::new()
}

pub struct Client {
    conf: Option<NamedTempFile>,
}

fn fixture_home() -> Result<std::path::PathBuf> {
    let home = std::env::home_dir().context("Find fixture home")?;
    ensure!(
        home == std::path::Path::new("/home/alice"),
        "Run tests through tests/x.sh"
    );

    Ok(home)
}

impl Client {
    pub fn new(settings: &str) -> Result<Self> {
        Self::settings(&format!("IdentityFile ~/.ssh/id_ed25519\n{settings}"))
    }

    pub fn settings(settings: &str) -> Result<Self> {
        use std::io::Write as _;

        let home = fixture_home()?;

        let mut conf = NamedTempFile::new()?;
        writeln!(conf, "Host target\n RequestTTY no\n{settings}")?;
        conf.write_all(&std::fs::read(home.join(".ssh/config"))?)?;
        conf.flush()?;

        Ok(Self { conf: Some(conf) })
    }

    pub fn toml(text: &str) -> Result<Self> {
        Self::file(text, ".toml")
    }

    pub fn lua(text: &str) -> Result<Self> {
        Self::file(text, ".lua")
    }

    fn file(text: &str, suffix: &str) -> Result<Self> {
        use std::io::Write as _;

        fixture_home()?;

        let mut conf = tempfile::Builder::new().suffix(suffix).tempfile()?;
        conf.write_all(text.as_bytes())?;
        conf.flush()?;

        Ok(Self { conf: Some(conf) })
    }

    pub fn default_config() -> Result<Self> {
        fixture_home()?;

        Ok(Self { conf: None })
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("/opt/tests/ssh");

        if let Some(conf) = &self.conf {
            cmd.arg("-F").arg(conf.path());
        }

        cmd.args(args);
        cmd.kill_on_drop(true);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        cmd
    }

    pub async fn run(&self, args: &[&str], data: &[u8]) -> Result<Output> {
        use tokio::io::AsyncWriteExt as _;

        let mut child = self.command(args).spawn()?;
        let mut stdin = child.stdin.take().context("Capture stdin")?;

        let input = async {
            match stdin.write_all(data).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
                Err(e) => return Err(e),
            }

            drop(stdin);

            Ok(())
        };

        let (_, output) = timeout(TIMEOUT, async {
            tokio::try_join!(input, child.wait_with_output())
        })
        .await??;

        Ok(output)
    }

    pub fn pty(&self, args: &[&str]) -> Result<PtyClient> {
        use nix::fcntl::{FcntlArg, FdFlag, fcntl};
        use nix::pty::openpty;
        use nix::sys::termios::tcgetattr;
        use nix::unistd::setsid;

        use std::fs::File;

        let pty = openpty(None, None)?;
        fcntl(&pty.master, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
        fcntl(&pty.slave, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;

        let slave = File::from(pty.slave);
        let saved = tcgetattr(&slave)?;
        let flags = fcntl(&slave, FcntlArg::F_GETFL)?;

        let mut cmd = self.command(args);
        cmd.stdin(slave.try_clone()?);
        cmd.stdout(slave.try_clone()?);
        cmd.stderr(slave.try_clone()?);

        unsafe {
            cmd.pre_exec(|| {
                setsid()?;

                if nix::libc::ioctl(0, nix::libc::TIOCSCTTY, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }

                Ok(())
            });
        }

        Ok(PtyClient {
            child: cmd.spawn()?,
            master: tokio::fs::File::from_std(File::from(pty.master)),
            slave,
            saved,
            flags,
        })
    }
}

pub fn expect(output: &Output, code: i32, stdout: &[u8], stderr: &[u8]) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(output.stdout, stdout);

    assert_eq!(output.stderr, stderr);
}

pub async fn terminate(child: Child) -> Result<()> {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    let pid = child.id().context("Client already exited")?;
    kill(Pid::from_raw(pid as i32), Signal::SIGTERM)?;

    let output = timeout(TIMEOUT, child.wait_with_output()).await??;

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(())
}

pub struct KnownHosts {
    path: std::path::PathBuf,
    original: Vec<u8>,
}

impl KnownHosts {
    pub fn replace(content: &[u8]) -> Result<Self> {
        let home = std::env::home_dir().context("Find fixture home")?;
        ensure!(
            home == std::path::Path::new("/home/alice"),
            "Run tests through tests/x.sh"
        );

        let path = home.join(".ssh/known_hosts");
        let original = std::fs::read(&path)?;
        std::fs::write(&path, content)?;

        Ok(Self { path, original })
    }
}

impl Drop for KnownHosts {
    fn drop(&mut self) {
        std::fs::write(&self.path, &self.original).expect("restore fixture known_hosts");
    }
}
