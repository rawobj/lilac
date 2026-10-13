//! The I/O runtime.
//!
//! GPUI drives the UI on its own executor. Networking and the audio pipeline
//! live on a small tokio runtime; results are handed back to the UI by awaiting
//! the `JoinHandle` from a GPUI task.

use std::future::Future;
use std::sync::OnceLock;

use tokio::runtime::{Builder, Runtime};
use tokio::task::JoinHandle;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub fn rt() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        // reqwest and librespot share one rustls; with no default provider
        // rustls cannot choose and librespot's HTTP client panics. Every TLS
        // connection starts on this runtime, so installing it here is early enough.
        let _ = rustls::crypto::ring::default_provider().install_default();
        Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(4)
            .thread_name("lilac-io")
            .enable_all()
            .build()
            .expect("failed to start I/O runtime")
    })
}

pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    rt().spawn(future)
}

/// Run `future` on the I/O runtime and resolve with its output. A panicked or
/// cancelled task surfaces as an error instead of poisoning the caller.
pub async fn run<F, T>(future: F) -> anyhow::Result<T>
where
    F: Future<Output = anyhow::Result<T>> + Send + 'static,
    T: Send + 'static,
{
    match spawn(future).await {
        Ok(result) => result,
        Err(err) => Err(anyhow::anyhow!("background task failed: {err}")),
    }
}
