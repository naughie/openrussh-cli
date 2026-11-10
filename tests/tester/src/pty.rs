use anyhow::{Context as _, Result, ensure};
use tokio::process::Child;
use tokio::time::{Duration, timeout};

const TIMEOUT: Duration = Duration::from_secs(15);

pub struct PtyClient {
    pub(crate) child: Child,
    pub(crate) master: tokio::fs::File,
    pub(crate) slave: std::fs::File,
    pub(crate) saved: nix::sys::termios::Termios,
    pub(crate) flags: i32,
}

impl PtyClient {
    pub async fn read_until(&mut self, needle: &[u8]) -> Result<Vec<u8>> {
        use tokio::io::AsyncReadExt as _;

        timeout(TIMEOUT, async {
            let mut data = Vec::new();
            let mut buf = [0; 4096];
            while !data.windows(needle.len()).any(|part| part == needle) {
                let n = self.master.read(&mut buf).await?;
                ensure!(n != 0, "PTY closed before {needle:?}: {data:?}");
                data.extend_from_slice(&buf[..n]);
            }

            Ok(data)
        })
        .await?
    }

    pub async fn write(&mut self, data: &[u8]) -> Result<()> {
        use tokio::io::AsyncWriteExt as _;
        timeout(TIMEOUT, self.master.write_all(data)).await??;
        self.master.flush().await?;

        Ok(())
    }

    pub fn signal(&self, signal: nix::sys::signal::Signal) -> Result<()> {
        use nix::sys::signal::kill;
        use nix::unistd::Pid;

        let pid = self.child.id().context("PTY client already exited")?;
        kill(Pid::from_raw(pid as i32), signal)?;

        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        use std::os::fd::AsRawFd as _;

        let size = nix::libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        if unsafe { nix::libc::ioctl(self.slave.as_raw_fd(), nix::libc::TIOCSWINSZ, &size) } == -1 {
            return Err(std::io::Error::last_os_error().into());
        }
        self.signal(nix::sys::signal::Signal::SIGWINCH)
    }

    pub fn assert_restored(&self) -> Result<()> {
        use nix::fcntl::{FcntlArg, fcntl};
        use nix::sys::termios::tcgetattr;

        assert_eq!(tcgetattr(&self.slave)?, self.saved);

        assert_eq!(fcntl(&self.slave, FcntlArg::F_GETFL)?, self.flags);

        Ok(())
    }

    pub async fn wait_raw(&self) -> Result<()> {
        use nix::sys::termios::{cfmakeraw, tcgetattr};

        let mut raw = self.saved.clone();
        cfmakeraw(&mut raw);

        timeout(TIMEOUT, async {
            while tcgetattr(&self.slave)? != raw {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            Result::Ok(())
        })
        .await?
    }

    pub async fn wait_stopped(&mut self) -> Result<()> {
        use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
        use nix::unistd::Pid;

        let pid = Pid::from_raw(self.child.id().context("PTY client already exited")? as i32);
        timeout(TIMEOUT, async {
            loop {
                match waitpid(pid, Some(WaitPidFlag::WUNTRACED | WaitPidFlag::WNOHANG))? {
                    WaitStatus::Stopped(_, _) => return Ok(()),
                    WaitStatus::StillAlive => tokio::time::sleep(Duration::from_millis(10)).await,
                    status => anyhow::bail!("Expected stopped client, received {status:?}"),
                }
            }
        })
        .await?
    }

    pub async fn wait(mut self, code: i32) -> Result<()> {
        let status = timeout(TIMEOUT, self.child.wait()).await??;

        assert_eq!(status.code(), Some(code));
        self.assert_restored()
    }
}
