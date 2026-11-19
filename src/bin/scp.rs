use std::process::ExitCode;

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("scp: Failed building the runtime: {e}");
            return ExitCode::from(255);
        }
    };
    let status = runtime.block_on(openrussh_cli::run_scp());

    // A signal can interrupt a blocking host-key prompt before it receives input.
    runtime.shutdown_background();
    status
}
