use openrussh_cli_config::LogLevel;

use tracing::level_filters::LevelFilter;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::{FilterFn, filter_fn};
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

use std::fmt;

pub(crate) fn init(verbose: bool, level: Option<&LogLevel>) {
    tracing_subscriber::fmt()
        .with_env_filter(filter(verbose, level))
        .with_ansi(false)
        .event_format(OpenSshFormat)
        .with_writer(std::io::stderr)
        .finish()
        .with(verbosity(verbose))
        .init();
}

fn filter(verbose: bool, level: Option<&LogLevel>) -> EnvFilter {
    match level {
        Some(LogLevel::None) => EnvFilter::default().add_directive(LevelFilter::OFF.into()),
        Some(LogLevel::Filter(filter)) => filter.as_ref().clone(),
        None => {
            let default = if verbose {
                LevelFilter::DEBUG
            } else {
                LevelFilter::ERROR
            };
            EnvFilter::default().add_directive(default.into())
        }
    }
}

fn verbosity(verbose: bool) -> FilterFn<impl Fn(&tracing::Metadata<'_>) -> bool> {
    // Keep span context available to EnvFilter while capping emitted events.
    filter_fn(move |meta| verbose || meta.is_span() || *meta.level() == Level::ERROR)
}

struct OpenSshFormat;

impl<S, N> FormatEvent<S, N> for OpenSshFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        match *event.metadata().level() {
            Level::ERROR => writer.write_str("ssh: ")?,
            Level::DEBUG => writer.write_str("debug1: ")?,
            Level::TRACE => writer.write_str("debug2: ")?,
            Level::WARN | Level::INFO => {}
        }

        ctx.field_format().format_fields(writer.by_ref(), event)?;
        // A remote PTY session can put the local terminal in raw mode.
        writer.write_str("\r\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Output(Arc<Mutex<Vec<u8>>>);

    impl Write for Output {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Output {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    fn capture(verbose: bool, level: Option<LogLevel>, emit: impl FnOnce()) -> String {
        let output = Output::default();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(filter(verbose, level.as_ref()))
            .with_ansi(false)
            .event_format(OpenSshFormat)
            .with_writer(output.clone())
            .finish()
            .with(verbosity(verbose));

        tracing::subscriber::with_default(subscriber, emit);

        String::from_utf8(output.0.lock().unwrap().clone()).unwrap()
    }

    fn messages(verbose: bool, level: Option<LogLevel>) -> String {
        capture(verbose, level, || {
            tracing::error!("Connection failed");
            tracing::warn!("Warning: Host key added");
            tracing::info!("Authenticated");
            tracing::debug!("Entering session");
            tracing::trace!("Packet data");
        })
    }

    fn directives(value: &str) -> LogLevel {
        LogLevel::try_from_filter(value).unwrap()
    }

    #[test]
    fn verbosity_and_configured_level_filter_actual_events() {
        let errors = "ssh: Connection failed\r\n";
        let warnings = format!("{errors}Warning: Host key added\r\n");
        let info = format!("{warnings}Authenticated\r\n");
        let debug = format!("{info}debug1: Entering session\r\n");
        let trace = format!("{debug}debug2: Packet data\r\n");

        for verbose in [false, true] {
            assert_eq!(messages(verbose, Some(LogLevel::None)), "");
            assert_eq!(messages(verbose, Some(directives("off"))), "");
            assert_eq!(messages(verbose, Some(directives("error"))), errors);
        }

        assert_eq!(messages(false, None), errors);
        assert_eq!(messages(true, None), debug);

        for level in ["warn", "info", "debug", "trace"] {
            assert_eq!(messages(false, Some(directives(level))), errors);
        }

        assert_eq!(messages(true, Some(directives("warn"))), warnings);
        assert_eq!(messages(true, Some(directives("info"))), info);
        assert_eq!(messages(true, Some(directives("debug"))), debug);
        assert_eq!(messages(true, Some(directives("trace"))), trace);
    }

    #[test]
    fn target_directives_filter_events_without_bypassing_the_nonverbose_cap() {
        let emit = || {
            tracing::error!(target: "openrussh_cli", "Client error");
            tracing::debug!(target: "openrussh_cli::session", "Client session");
            tracing::error!(target: "russh::client", "Transport error");
            tracing::info!(target: "russh::client", "Transport connected");
            tracing::debug!(target: "russh::client", "Transport details");
            tracing::trace!(target: "russh::client::kex", "Key exchange details");
            tracing::error!(target: "unrelated", "Other error");
        };
        let errors = "ssh: Client error\r\nssh: Transport error\r\nssh: Other error\r\n";

        assert_eq!(
            capture(true, Some(directives("debug,russh=info")), emit),
            "ssh: Client error\r\ndebug1: Client session\r\nssh: Transport error\r\nTransport connected\r\nssh: Other error\r\n"
        );
        for spec in ["debug,russh=info", "error,openrussh_cli=trace", "trace"] {
            assert_eq!(
                capture(false, Some(directives(spec)), emit),
                errors,
                "{spec}"
            );
        }

        assert_eq!(
            capture(
                true,
                Some(directives(
                    "off,russh::client=debug,russh::client::kex=trace"
                )),
                emit
            ),
            "ssh: Transport error\r\nTransport connected\r\ndebug1: Transport details\r\ndebug2: Key exchange details\r\n"
        );
        assert_eq!(
            capture(false, Some(directives("debug,russh=off")), emit),
            "ssh: Client error\r\nssh: Other error\r\n"
        );
        assert_eq!(
            capture(true, Some(directives("russh=info")), emit),
            "ssh: Transport error\r\nTransport connected\r\n"
        );
    }

    #[test]
    fn nonverbose_filtering_preserves_span_context() {
        let emit = || {
            tracing::error!(target: "russh::client", "Outside connection");
            let span = tracing::debug_span!(target: "russh::client", "connection");
            let _entered = span.enter();
            tracing::error!(target: "russh::client", "Connection failed");
            tracing::debug!(target: "russh::client", "Connection details");
        };

        assert_eq!(
            capture(false, Some(directives("off,russh[connection]=debug")), emit),
            "ssh: Connection failed\r\n"
        );
    }
}
