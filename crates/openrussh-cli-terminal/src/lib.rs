//! Unix terminal modes, asynchronous input, and process signal streams.
//!
//! [`Terminal`] borrows a descriptor, enters raw mode when requested, and restores
//! the saved terminal attributes when dropped. [`Input`] duplicates a borrowed
//! descriptor for asynchronous reads and restores shared file-status flags when
//! its polling guard is dropped. Neither takes ownership of the caller's source
//! descriptor. [`Signals`] separates shutdown requests from terminal events so a
//! caller can await both while performing session work.
//!
//! # Reading input
//!
//! Pollable descriptors, including pipes, terminals, and sockets, use Tokio's I/O
//! driver. Descriptors rejected by polling with `EPERM` use Tokio's file reader.
//! [`Input`] implements [`tokio::io::AsyncRead`]. Construct it inside a Tokio
//! runtime with I/O enabled:
//!
//! ```
//! use openrussh_cli_terminal::Input;
//! use std::io::{self, Write};
//! use std::os::fd::AsFd;
//! use std::os::unix::net::UnixStream;
//!
//! #[tokio::main(flavor = "current_thread")]
//! async fn main() -> io::Result<()> {
//!     let (mut writer, reader) = UnixStream::pair()?;
//!     let mut input = Input::new(reader.as_fd())?;
//!     writer.write_all(b"hello")?;
//!     let mut buffer = [0; 16];
//!     let count = input.read(&mut buffer).await?;
//!     assert_eq!(&buffer[..count], b"hello");
//!     drop(input); // Restores the original shared file-status flags.
//!     Ok(())
//! }
//! ```

use nix::sys::termios::{self, SetArg, Termios};

use tokio::io::{AsyncRead, ReadBuf};
use tokio::signal::unix::{Signal, SignalKind};

use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::pin::Pin;
use std::task::{Context, Poll, ready};

/// A terminal-related process signal, received through [`Events::recv()`].
pub enum Event {
    /// `SIGWINCH`: the terminal dimensions may have changed.
    Resize,
    /// `SIGTSTP`: the caller should arrange terminal restoration and suspension.
    Suspend,
    /// `SIGCONT`: the process has continued after being stopped.
    Continue,
}

/// Shutdown and terminal-event signal streams for one session.
///
/// Create with [`Self::new()`], then use [`Self::split()`] to await the two signal
/// groups independently. Receiving an event does not itself stop or exit the
/// process; the caller chooses the appropriate session action.
///
/// ```no_run
/// use openrussh_cli_terminal::{Event, Signals};
///
/// #[tokio::main(flavor = "current_thread")]
/// async fn main() -> std::io::Result<()> {
///     let mut signals = Signals::new()?;
///     let (shutdown, events) = signals.split();
///     tokio::select! {
///         signal = shutdown.recv() => println!("Shutdown requested by signal {signal}"),
///         event = events.recv() => match event {
///             Event::Resize => println!("Refresh the terminal dimensions"),
///             Event::Suspend => println!("Restore terminal modes before stopping"),
///             Event::Continue => println!("Resume session work"),
///         },
///     }
///     Ok(())
/// }
/// ```
pub struct Signals {
    shutdown: Shutdown,
    events: Events,
}

/// A receiver for `SIGHUP`, `SIGINT`, `SIGQUIT`, and `SIGTERM`.
///
/// Obtain a mutable reference through [`Signals::split()`].
pub struct Shutdown {
    hup: Signal,
    int: Signal,
    quit: Signal,
    term: Signal,
}

/// A receiver for terminal resize, suspension, and continuation signals.
///
/// Obtain a mutable reference through [`Signals::split()`].
pub struct Events {
    winch: Signal,
    tstp: Signal,
    cont: Signal,
}

impl Signals {
    /// Registers the shutdown and terminal-event streams with Tokio.
    ///
    /// Signal registration has Tokio's process-wide signal-handler behavior;
    /// dropping these streams does not restore the original signal disposition.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if any signal stream cannot be registered.
    ///
    /// # Panics
    ///
    /// Panics outside a Tokio runtime with its signal driver enabled.
    pub fn new() -> io::Result<Self> {
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

    /// Borrows both receivers so they can be awaited independently.
    ///
    /// The receivers remain owned by this [`Signals`] value.
    pub fn split(&mut self) -> (&mut Shutdown, &mut Events) {
        (&mut self.shutdown, &mut self.events)
    }
}

impl Shutdown {
    /// Waits for a shutdown signal and returns its Unix signal number.
    ///
    /// The result is one of `SIGHUP`, `SIGINT`, `SIGQUIT`, or `SIGTERM`.
    /// Cancelling this wait does not consume a signal from any of the streams.
    pub async fn recv(&mut self) -> i32 {
        tokio::select! {
            _ = self.hup.recv() => nix::libc::SIGHUP,
            _ = self.int.recv() => nix::libc::SIGINT,
            _ = self.quit.recv() => nix::libc::SIGQUIT,
            _ = self.term.recv() => nix::libc::SIGTERM,
        }
    }
}

impl Events {
    /// Waits for the next resize, suspend, or continue event.
    ///
    /// Cancelling this wait does not consume an event from any of the streams.
    pub async fn recv(&mut self) -> Event {
        tokio::select! {
            _ = self.winch.recv() => Event::Resize,
            _ = self.tstp.recv() => Event::Suspend,
            _ = self.cont.recv() => Event::Continue,
        }
    }
}

/// A borrowed terminal's saved attributes and raw-mode restoration guard.
///
/// [`Self::new()`] immediately enters raw mode when enabled on a terminal.
/// Dropping the guard attempts to restore the original attributes, including
/// during error unwinding. Restoration uses drain semantics and does not flush
/// pending input. Drop cannot report restoration failures.
///
/// The descriptor must remain alive for `'fd`; the guard never closes it.
/// Disabled guards and nonterminal descriptors leave terminal attributes alone,
/// while [`Self::size()`] remains available.
///
/// ```no_run
/// use openrussh_cli_terminal::Terminal;
/// use std::io;
/// use std::os::fd::AsFd;
///
/// let stdin = io::stdin();
/// let terminal = Terminal::new(stdin.as_fd(), true)?;
/// let (columns, rows) = terminal.size();
/// println!("Terminal dimensions: {columns} columns, {rows} rows");
/// drop(terminal); // Restores the attributes saved before raw mode.
/// # Ok::<(), io::Error>(())
/// ```
#[must_use = "dropping the terminal guard restores its saved attributes"]
pub struct Terminal<'fd> {
    fd: BorrowedFd<'fd>,
    saved: Option<Termios>,
}

impl<'fd> Terminal<'fd> {
    /// Borrows `fd`, saves its attributes when enabled, and enters raw mode.
    ///
    /// If `enabled` is `false` or `fd` is not a terminal, no attributes are saved
    /// or changed. Otherwise the saved attributes are the baseline used by
    /// [`Self::enter()`], [`Self::suspend()`], and drop restoration.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if terminal detection, attribute reading, or entering
    /// raw mode fails.
    pub fn new(fd: BorrowedFd<'fd>, enabled: bool) -> io::Result<Self> {
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

    /// Enters raw mode using the attributes saved by [`Self::new()`].
    ///
    /// Repeated calls use the same original baseline. Disabled and nonterminal
    /// guards do nothing. Mode changes wait for pending output to drain, without
    /// flushing input.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the terminal attributes cannot be changed.
    pub fn enter(&self) -> io::Result<()> {
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

    /// Restores saved attributes, stops the process, and reenters raw mode.
    ///
    /// The process stops itself with `SIGSTOP`. After an external `SIGCONT`
    /// resumes it, this method calls [`Self::enter()`]. This restoration sequence
    /// also preserves pending terminal input.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if restoring attributes, stopping the process, or
    /// reentering raw mode fails. On error, later steps are not attempted.
    pub fn suspend(&self) -> io::Result<()> {
        self.restore()?;
        suspend()?;
        self.enter()
    }

    /// Returns the borrowed descriptor's `(columns, rows)` dimensions.
    ///
    /// Returns `(80, 24)` if the dimensions cannot be read, including for a
    /// nonterminal descriptor. This method also works when raw mode is disabled.
    pub fn size(&self) -> (u32, u32) {
        size(self.fd)
    }
}

impl Drop for Terminal<'_> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Stops the current process with `SIGSTOP` until another process resumes it.
///
/// This function changes no terminal attributes or input flags. Call
/// [`Terminal::suspend()`] when saved terminal modes need to be restored around
/// the stop, and coordinate [`InputMode::pause()`] / [`InputMode::resume()`] when
/// another process needs the original descriptor flags.
///
/// # Errors
///
/// Returns an I/O error if the signal cannot be sent to the current process.
pub fn suspend() -> io::Result<()> {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::getpid;

    kill(getpid(), Signal::SIGSTOP)?;

    Ok(())
}

fn size(fd: BorrowedFd<'_>) -> (u32, u32) {
    use terminal_size::{Height, Width, terminal_size_of};

    terminal_size_of(fd)
        .map(|(Width(w), Height(h))| (u32::from(w), u32::from(h)))
        .unwrap_or((80, 24))
}

fn attrs(fd: BorrowedFd<'_>) -> io::Result<Termios> {
    use std::mem::MaybeUninit;
    use std::os::fd::AsRawFd as _;

    let mut raw = MaybeUninit::<nix::libc::termios>::zeroed();

    // SAFETY: `fd` is borrowed for this call and `raw` is writable. Zeroing also
    // initializes fields the platform may leave untouched, such as unused
    // control-character slots.
    if unsafe { nix::libc::tcgetattr(fd.as_raw_fd(), raw.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: Every field started initialized, and tcgetattr succeeded.
    Ok(unsafe { raw.assume_init() }.into())
}

/// An asynchronous reader backed by a duplicate of a borrowed Unix descriptor.
///
/// Pollable descriptors are registered with Tokio and switched to `O_NONBLOCK`.
/// File-status flags and any file offset are shared with the source descriptor;
/// duplication does not create an independent cursor. The saved flags are
/// restored on drop, including after EOF or cancellation of a pending read.
/// Preexisting `O_NONBLOCK` is preserved.
///
/// When the platform refuses polling with `EPERM`, as for regular files on Linux,
/// reads use [`tokio::fs::File`] instead. [`Self::mode()`] returns `None` for that
/// fallback. The source descriptor must remain alive for `'fd` in either case.
#[must_use = "dropping the input reader restores its saved file-status flags"]
pub struct Input<'fd> {
    kind: InputKind<'fd>,
}

enum InputKind<'fd> {
    Poll(PollInput<'fd>),
    File(tokio::fs::File),
}

struct PollInput<'fd> {
    fd: tokio::io::unix::AsyncFd<OwnedFd>,
    mode: InputMode<'fd>,
}

impl Drop for PollInput<'_> {
    fn drop(&mut self) {
        let _ = self.mode.pause();
    }
}

impl<'fd> Input<'fd> {
    /// Duplicates and prepares `source` for asynchronous reads.
    ///
    /// The original descriptor remains owned by the caller. For polling input,
    /// this changes the flags shared by both descriptors until the guard is
    /// dropped or [`InputMode::pause()`] is called.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if duplication, reading/changing flags, or registering
    /// the descriptor fails. An `EPERM` registration error selects the file
    /// fallback instead.
    ///
    /// # Panics
    ///
    /// Panics outside a Tokio runtime with its I/O driver enabled.
    pub fn new(source: BorrowedFd<'fd>) -> io::Result<Self> {
        use nix::fcntl::{FcntlArg, OFlag, fcntl};
        use tokio::io::unix::AsyncFd;

        let fd = source.try_clone_to_owned()?;
        let flags = OFlag::from_bits_retain(fcntl(&fd, FcntlArg::F_GETFL)?);

        // SAFETY: OwnedFd keeps this descriptor open and unchanged until
        // AsyncFd deregisters it. No operation replaces the owned descriptor.
        match unsafe { AsyncFd::register_with_interest(fd, tokio::io::Interest::READABLE) } {
            Ok(fd) => {
                let input = PollInput {
                    fd,
                    mode: InputMode { fd: source, flags },
                };
                input.mode.resume()?;

                Ok(Self {
                    kind: InputKind::Poll(input),
                })
            }
            Err(e) => {
                let (fd, e) = e.into_parts();

                if e.raw_os_error() != Some(nix::libc::EPERM) {
                    return Err(e);
                }

                Ok(Self {
                    kind: InputKind::File(tokio::fs::File::from_std(fd.into())),
                })
            }
        }
    }

    /// Returns a flag-control handle for polling input, or `None` for file input.
    ///
    /// The handle borrows the original descriptor, rather than this [`Input`].
    /// Use it while the input guard remains alive to restore blocking flags before
    /// suspension, then reenable nonblocking flags after continuation.
    pub fn mode(&self) -> Option<InputMode<'fd>> {
        match &self.kind {
            InputKind::Poll(input) => Some(input.mode),
            InputKind::File(_) => None,
        }
    }

    /// Reads bytes into `buf`, returning the number read.
    ///
    /// An empty buffer returns zero immediately without waiting for input. For a
    /// nonempty buffer, zero indicates EOF.
    ///
    /// Polling input waits for readability, retries interrupted reads, and clears
    /// stale readiness before retrying. Cancelling a pending polling read does
    /// not consume future input; a subsequent call can read it normally. File
    /// input delegates to Tokio's asynchronous file reader.
    ///
    /// # Errors
    ///
    /// Returns an I/O error from readiness registration or the underlying read.
    pub async fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        tokio::io::AsyncReadExt::read(self, buf).await
    }
}

impl AsyncRead for Input<'_> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }

        match &mut self.get_mut().kind {
            InputKind::Poll(input) => loop {
                let mut ready = ready!(input.fd.poll_read_ready(cx))?;

                match ready.try_io(|fd| {
                    nix::unistd::read(fd.get_ref(), buf.initialize_unfilled())
                        .map_err(io::Error::from)
                }) {
                    Ok(Ok(n)) => {
                        buf.advance(n);

                        return Poll::Ready(Ok(()));
                    }
                    Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => {}
                    Ok(Err(e)) => return Poll::Ready(Err(e)),
                    Err(_) => {}
                }
            },
            InputKind::File(f) => Pin::new(f).poll_read(cx, buf),
        }
    }
}

#[derive(Clone, Copy)]
/// Explicit control of the shared file-status flags for polling input.
///
/// Obtain this handle through [`Input::mode()`]. It borrows the original source
/// descriptor and can be copied; dropping a handle does not change flags. The
/// owning [`Input`] guard restores its saved flags on drop.
///
/// [`Self::pause()`] and [`Self::resume()`] change flags only. They do not cancel
/// reads or stop the process. Pause when no read is in progress, then resume
/// before polling input again.
pub struct InputMode<'fd> {
    fd: BorrowedFd<'fd>,
    flags: nix::fcntl::OFlag,
}

impl InputMode<'_> {
    /// Restores the file-status flags captured by [`Input::new()`].
    ///
    /// This removes the added `O_NONBLOCK` flag unless it was already present
    /// when the input was constructed.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the descriptor's flags cannot be changed.
    pub fn pause(self) -> io::Result<()> {
        use nix::fcntl::{FcntlArg, fcntl};

        fcntl(self.fd, FcntlArg::F_SETFL(self.flags))?;

        Ok(())
    }

    /// Sets the captured flags together with `O_NONBLOCK` for polling reads.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the descriptor's flags cannot be changed.
    pub fn resume(self) -> io::Result<()> {
        use nix::fcntl::{FcntlArg, OFlag, fcntl};

        fcntl(self.fd, FcntlArg::F_SETFL(self.flags | OFlag::O_NONBLOCK))?;

        Ok(())
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
    async fn empty_input_read_does_not_wait_for_readiness_or_consume_input() {
        let (r, w) = pipe().unwrap();
        let mut input = Input::new(r.as_fd()).unwrap();

        assert_eq!(
            tokio::time::timeout(Duration::from_millis(20), input.read(&mut []))
                .await
                .unwrap()
                .unwrap(),
            0
        );

        write(&w, b"remaining").unwrap();
        let mut buf = [0; 16];
        let n = input.read(&mut buf).await.unwrap();

        assert_eq!(&buf[..n], b"remaining");
    }

    #[tokio::test]
    async fn async_read_appends_to_an_uninitialized_read_buffer() {
        use std::future::poll_fn;
        use std::mem::MaybeUninit;
        use std::pin::Pin;
        use tokio::io::{AsyncRead as _, ReadBuf};

        let (r, w) = pipe().unwrap();
        let mut input = Input::new(r.as_fd()).unwrap();
        write(&w, b"suffix").unwrap();

        let mut storage = [MaybeUninit::uninit(); 32];
        let mut buf = ReadBuf::uninit(&mut storage);
        buf.put_slice(b"prefix");
        poll_fn(|cx| Pin::new(&mut input).poll_read(cx, &mut buf))
            .await
            .unwrap();

        assert_eq!(buf.filled(), b"prefixsuffix");
    }

    #[tokio::test]
    async fn owned_input_descriptor_is_close_on_exec() {
        use super::InputKind;
        use nix::fcntl::FdFlag;

        let (r, _w) = pipe().unwrap();
        let saved = fcntl(&r, FcntlArg::F_GETFD).unwrap();
        let input = Input::new(r.as_fd()).unwrap();
        let InputKind::Poll(input) = &input.kind else {
            panic!("pipes must use readiness polling");
        };
        let flags = FdFlag::from_bits_retain(fcntl(input.fd.get_ref(), FcntlArg::F_GETFD).unwrap());

        assert!(flags.contains(FdFlag::FD_CLOEXEC));
        assert_eq!(fcntl(&r, FcntlArg::F_GETFD).unwrap(), saved);
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
