use std::process::ExitCode;

fn main() -> ExitCode {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed building the Runtime");
    let status = runtime.block_on(openrussh_cli::run());

    // A signal can end the invocation while a host-key prompt is still reading.
    // Application cleanup is complete; do not wait for that read before returning.
    runtime.shutdown_background();
    status
}
