use anyhow::Error;

use tokio::runtime::Runtime as TokioRuntime;

use std::sync::Arc;

use libtest_mimic::Arguments;
use libtest_mimic::{Failed, Trial};

pub struct Runtime {
    args: Arguments,
    rt: Arc<TokioRuntime>,
    tests: Vec<Trial>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

fn run_test_impl(
    rt: &Arc<TokioRuntime>,
    test: impl AsyncFnOnce() -> Result<(), Error> + Send + 'static,
) -> impl FnOnce() -> Result<(), Failed> + Send + 'static {
    let rt = rt.clone();
    move || {
        rt.block_on(test())
            .map_err(|e| Failed::from(format_args!("test failed: {e}")))
    }
}

impl Runtime {
    pub fn new() -> Self {
        use tokio::runtime::Builder;

        Self {
            args: {
                let mut args = Arguments::from_args();
                args.test_threads.get_or_insert(1);
                args
            },
            rt: Arc::new(
                Builder::new_multi_thread()
                    .enable_all()
                    .thread_name("my-tokio-worker")
                    .build()
                    .expect("failed to create Tokio runtime"),
            ),
            tests: Vec::new(),
        }
    }

    pub fn register(
        &mut self,
        desc: &str,
        test: impl AsyncFnOnce() -> Result<(), Error> + Send + 'static,
    ) {
        self.tests
            .push(Trial::test(desc, run_test_impl(&self.rt, test)));
    }

    pub fn run(self) -> ! {
        libtest_mimic::run(&self.args, self.tests).exit()
    }
}
