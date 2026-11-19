use std::process::ExitCode;

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("ssh: Failed building the runtime: {e}");
            return ExitCode::from(255);
        }
    };
    let status = runtime.block_on(openrussh_cli::run_ssh());

    // A signal can end the invocation while a host-key prompt is still reading.
    // Application cleanup is complete; do not wait for that read before returning.
    runtime.shutdown_background();
    status
}
