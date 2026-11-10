use nix::sys::termios::{self, SetArg, Termios};

use tokio::signal::unix::{Signal, SignalKind};

use std::io;
use std::os::fd::BorrowedFd;

pub(crate) enum Event {
    Resize,
    Suspend,
    Continue,
}

pub(crate) struct Signals {
    shutdown: Shutdown,
    events: Events,
}

pub(crate) struct Shutdown {
    hup: Signal,
    int: Signal,
    quit: Signal,
    term: Signal,
}

pub(crate) struct Events {
    winch: Signal,
    tstp: Signal,
    cont: Signal,
}

impl Signals {
    pub(crate) fn new() -> io::Result<Self> {
        use tokio::signal::unix::signal;

        Ok(Self {
            shutdown: Shutdown {
                hup: signal(SignalKind::hangup())?,
                int: signal(SignalKind::interrupt())?,
                quit: signal(SignalKind::quit())?,
                term: signal(SignalKind::terminate())?,
            },
            events: Events {
                winch: signal(SignalKind::window_change())?,
                tstp: signal(SignalKind::from_raw(nix::libc::SIGTSTP))?,
                cont: signal(SignalKind::from_raw(nix::libc::SIGCONT))?,
            },
        })
    }

    pub(crate) fn split(&mut self) -> (&mut Shutdown, &mut Events) {
        (&mut self.shutdown, &mut self.events)
    }
}

impl Shutdown {
    pub(crate) async fn recv(&mut self) -> i32 {
        tokio::select! {
            _ = self.hup.recv() => nix::libc::SIGHUP,
            _ = self.int.recv() => nix::libc::SIGINT,
            _ = self.quit.recv() => nix::libc::SIGQUIT,
            _ = self.term.recv() => nix::libc::SIGTERM,
        }
    }
}

impl Events {
    pub(crate) async fn recv(&mut self) -> Event {
        tokio::select! {
            _ = self.winch.recv() => Event::Resize,
            _ = self.tstp.recv() => Event::Suspend,
            _ = self.cont.recv() => Event::Continue,
        }
    }
}

pub(crate) struct Terminal<'fd> {
    fd: BorrowedFd<'fd>,
    saved: Option<Termios>,
}

impl<'fd> Terminal<'fd> {
    pub(crate) fn new(fd: BorrowedFd<'fd>, enabled: bool) -> io::Result<Self> {
        use nix::unistd::isatty;

        let saved = if enabled && isatty(fd)? {
            Some(attrs(fd)?)
        } else {
            None
        };

        let term = Self { fd, saved };
        term.enter()?;

        Ok(term)
    }

    pub(crate) fn enter(&self) -> io::Result<()> {
        if let Some(saved) = &self.saved {
            let mut raw = saved.clone();
            termios::cfmakeraw(&mut raw);
            termios::tcsetattr(self.fd, SetArg::TCSADRAIN, &raw)?;
        }

        Ok(())
    }

    fn restore(&self) -> io::Result<()> {
        if let Some(saved) = &self.saved {
            termios::tcsetattr(self.fd, SetArg::TCSADRAIN, saved)?;
        }

        Ok(())
    }

    pub(crate) fn suspend(&self) -> io::Result<()> {
        self.restore()?;
        suspend()?;
        self.enter()
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        size(self.fd)
    }
}

impl Drop for Terminal<'_> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

pub(crate) fn suspend() -> io::Result<()> {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::getpid;

    kill(getpid(), Signal::SIGSTOP)?;

    Ok(())
}

pub(crate) fn size(fd: BorrowedFd<'_>) -> (u32, u32) {
    use terminal_size::{Height, Width, terminal_size_of};

    terminal_size_of(fd)
        .map(|(Width(w), Height(h))| (u32::from(w), u32::from(h)))
        .unwrap_or((80, 24))
}

pub(crate) fn attrs(fd: BorrowedFd<'_>) -> io::Result<Termios> {
    use std::mem::MaybeUninit;
    use std::os::fd::AsRawFd as _;

    let mut raw = MaybeUninit::<nix::libc::termios>::zeroed();

    if unsafe { nix::libc::tcgetattr(fd.as_raw_fd(), raw.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(unsafe { raw.assume_init() }.into())
}

pub(crate) enum Input<'fd> {
    Poll {
        fd: tokio::io::unix::AsyncFd<Nonblocking>,
        source: BorrowedFd<'fd>,
    },
    File(tokio::fs::File),
}

impl<'fd> Input<'fd> {
    pub(crate) fn new(source: BorrowedFd<'fd>) -> io::Result<Self> {
        use nix::fcntl::{FcntlArg, OFlag, fcntl};
        use nix::unistd::dup;
        use tokio::io::unix::AsyncFd;

        let fd = dup(source)?;
        let flags = OFlag::from_bits_truncate(fcntl(&fd, FcntlArg::F_GETFL)?);

        match unsafe {
            AsyncFd::register_with_interest(
                Nonblocking { fd, flags },
                tokio::io::Interest::READABLE,
            )
        }
        .map_err(io::Error::from)
        {
            Ok(fd) => {
                fcntl(
                    &fd.get_ref().fd,
                    FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK),
                )?;

                Ok(Self::Poll { fd, source })
            }
            Err(e) if e.raw_os_error() == Some(nix::libc::EPERM) => {
                let f = std::fs::File::from(dup(source)?);

                Ok(Self::File(tokio::fs::File::from_std(f)))
            }
            Err(e) => Err(e),
        }
    }

    pub(crate) fn mode(&self) -> Option<InputMode<'fd>> {
        match self {
            Self::Poll { fd, source } => Some(InputMode {
                fd: *source,
                flags: fd.get_ref().flags,
            }),
            Self::File(_) => None,
        }
    }

    pub(crate) async fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use tokio::io::AsyncReadExt as _;

        match self {
            Self::Poll { fd, .. } => loop {
                let mut ready = fd.readable().await?;

                match ready
                    .try_io(|fd| nix::unistd::read(&fd.get_ref().fd, buf).map_err(io::Error::from))
                {
                    Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Ok(res) => return res,
                    Err(_) => continue,
                }
            },
            Self::File(f) => f.read(buf).await,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct InputMode<'fd> {
    fd: BorrowedFd<'fd>,
    flags: nix::fcntl::OFlag,
}

impl InputMode<'_> {
    pub(crate) fn pause(self) -> io::Result<()> {
        use nix::fcntl::{FcntlArg, fcntl};

        fcntl(self.fd, FcntlArg::F_SETFL(self.flags))?;

        Ok(())
    }

    pub(crate) fn resume(self) -> io::Result<()> {
        use nix::fcntl::{FcntlArg, OFlag, fcntl};

        fcntl(self.fd, FcntlArg::F_SETFL(self.flags | OFlag::O_NONBLOCK))?;

        Ok(())
    }
}

pub(crate) struct Nonblocking {
    fd: std::os::fd::OwnedFd,
    flags: nix::fcntl::OFlag,
}

impl std::os::fd::AsRawFd for Nonblocking {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.fd.as_raw_fd()
    }
}

impl Drop for Nonblocking {
    fn drop(&mut self) {
        use nix::fcntl::{FcntlArg, fcntl};

        let _ = fcntl(&self.fd, FcntlArg::F_SETFL(self.flags));
    }
}

#[cfg(test)]
mod tests {
    use super::{Input, Terminal, attrs, size};

    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    use nix::pty::{OpenptyResult, Winsize, openpty};
    use nix::sys::termios::{self, LocalFlags, SetArg, Termios};
    use nix::unistd::{pipe, write};

    use std::io;
    use std::os::fd::{AsFd as _, BorrowedFd};
    use std::time::Duration;

    fn pty() -> OpenptyResult {
        let pty = openpty(None, None).unwrap();
        let mut modes = attrs(pty.slave.as_fd()).unwrap();
        modes
            .local_flags
            .insert(LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ISIG);
        termios::tcsetattr(&pty.slave, SetArg::TCSANOW, &modes).unwrap();
        pty
    }

    fn flags(fd: BorrowedFd<'_>) -> OFlag {
        OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL).unwrap())
    }

    fn assert_restored(fd: BorrowedFd<'_>, saved: &Termios) {
        let actual = attrs(fd).unwrap();

        #[cfg(target_os = "macos")]
        let actual = {
            let mut raw = nix::libc::termios::from(actual);
            raw.c_lflag = (raw.c_lflag & !nix::libc::PENDIN)
                | (saved.local_flags & LocalFlags::PENDIN).bits();
            Termios::from(raw)
        };

        assert_eq!(&actual, saved);
    }

    #[test]
    fn raw_mode_is_restored_on_drop() {
        let pty = pty();
        let saved = attrs(pty.slave.as_fd()).unwrap();
        {
            let _term = Terminal::new(pty.slave.as_fd(), true).unwrap();
            let raw = attrs(pty.slave.as_fd()).unwrap();

            assert!(
                !raw.local_flags
                    .intersects(LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::ISIG)
            );
        }

        assert_restored(pty.slave.as_fd(), &saved);
    }

    #[test]
    fn terminal_modes_are_restored_on_error() {
        fn fail(fd: BorrowedFd<'_>) -> io::Result<()> {
            let _term = Terminal::new(fd, true)?;
            Err(io::Error::other("session failed"))
        }

        let pty = pty();
        let saved = attrs(pty.slave.as_fd()).unwrap();

        assert!(fail(pty.slave.as_fd()).is_err());

        assert_restored(pty.slave.as_fd(), &saved);
    }

    #[test]
    fn disabled_terminal_preserves_modes() {
        let pty = pty();
        let saved = attrs(pty.slave.as_fd()).unwrap();
        let term = Terminal::new(pty.slave.as_fd(), false).unwrap();
        term.enter().unwrap();

        assert_eq!(attrs(pty.slave.as_fd()).unwrap(), saved);
        drop(term);

        assert_eq!(attrs(pty.slave.as_fd()).unwrap(), saved);
    }

    #[test]
    fn terminal_can_restore_and_reenter_raw_mode() {
        let pty = pty();
        let saved = attrs(pty.slave.as_fd()).unwrap();
        let term = Terminal::new(pty.slave.as_fd(), true).unwrap();
        term.restore().unwrap();

        assert_restored(pty.slave.as_fd(), &saved);
        term.enter().unwrap();

        assert!(
            !termios::tcgetattr(&pty.slave)
                .unwrap()
                .local_flags
                .contains(LocalFlags::ICANON)
        );
        drop(term);

        assert_restored(pty.slave.as_fd(), &saved);
    }

    #[tokio::test]
    async fn restoring_canonical_mode_preserves_pending_input() {
        let pty = pty();
        let saved = attrs(pty.slave.as_fd()).unwrap();
        {
            let _term = Terminal::new(pty.slave.as_fd(), true).unwrap();
            write(&pty.master, b"queued\n").unwrap();
        }

        assert_restored(pty.slave.as_fd(), &saved);

        let mut input = Input::new(pty.slave.as_fd()).unwrap();
        let mut buf = [0; 32];
        let n = tokio::time::timeout(Duration::from_secs(3), input.read(&mut buf))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(&buf[..n], b"queued\n");
        assert_restored(pty.slave.as_fd(), &saved);
    }

    #[test]
    fn nonterminal_input_keeps_default_size() {
        let (r, _w) = pipe().unwrap();
        let _term = Terminal::new(r.as_fd(), true).unwrap();

        assert_eq!(size(r.as_fd()), (80, 24));
    }

    #[test]
    fn terminal_size_comes_from_the_borrowed_terminal() {
        let size = Winsize {
            ws_row: 43,
            ws_col: 121,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        let pty = openpty(Some(&size), None).unwrap();
        let term = Terminal::new(pty.slave.as_fd(), false).unwrap();

        assert_eq!(term.size(), (121, 43));
    }

    #[tokio::test]
    async fn input_restores_shared_flags_after_eof() {
        let (r, w) = pipe().unwrap();
        let saved = flags(r.as_fd());
        {
            let mut input = Input::new(r.as_fd()).unwrap();

            assert!(flags(r.as_fd()).contains(OFlag::O_NONBLOCK));
            write(&w, b"input").unwrap();
            drop(w);

            let mut buf = [0; 16];

            assert_eq!(input.read(&mut buf).await.unwrap(), 5);

            assert_eq!(&buf[..5], b"input");

            assert_eq!(input.read(&mut buf).await.unwrap(), 0);
        }

        assert_eq!(flags(r.as_fd()), saved);
    }

    #[tokio::test]
    async fn input_pause_and_resume_use_the_original_fd() {
        let (r, _w) = pipe().unwrap();
        let saved = flags(r.as_fd());
        let input = Input::new(r.as_fd()).unwrap();
        let mode = input.mode().unwrap();
        mode.pause().unwrap();

        assert_eq!(flags(r.as_fd()), saved);
        mode.resume().unwrap();

        assert_eq!(flags(r.as_fd()), saved | OFlag::O_NONBLOCK);
        drop(input);

        assert_eq!(flags(r.as_fd()), saved);
    }

    #[tokio::test]
    async fn input_cancellation_does_not_consume_future_data() {
        let (r, w) = pipe().unwrap();
        let saved = flags(r.as_fd());
        let mut input = Input::new(r.as_fd()).unwrap();
        let mut buf = [0; 16];

        assert!(
            tokio::time::timeout(Duration::from_millis(20), input.read(&mut buf))
                .await
                .is_err()
        );
        write(&w, b"later").unwrap();

        let n = tokio::time::timeout(Duration::from_secs(1), input.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..n], b"later");
        drop(input);

        assert_eq!(flags(r.as_fd()), saved);
    }

    #[tokio::test]
    async fn input_preserves_preexisting_nonblocking_flags() {
        let (r, _w) = pipe().unwrap();
        let saved = flags(r.as_fd()) | OFlag::O_NONBLOCK;
        fcntl(&r, FcntlArg::F_SETFL(saved)).unwrap();
        drop(Input::new(r.as_fd()).unwrap());

        assert_eq!(flags(r.as_fd()), saved);
    }

    #[tokio::test]
    async fn regular_file_input_reads_to_eof() {
        use std::io::{Seek as _, SeekFrom, Write as _};

        let mut f = tempfile::tempfile().unwrap();
        f.write_all(b"file input").unwrap();
        f.seek(SeekFrom::Start(0)).unwrap();

        let mut input = Input::new(f.as_fd()).unwrap();
        let mut buf = [0; 16];

        assert_eq!(input.read(&mut buf).await.unwrap(), 10);

        assert_eq!(&buf[..10], b"file input");

        assert_eq!(input.read(&mut buf).await.unwrap(), 0);
    }
}
