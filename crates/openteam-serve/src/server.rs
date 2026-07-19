//! The stream server's axum app: `build_router` + `serve`, on the ADR 0019
//! `openteam-mock` precedent (ADR 0030).
//!
//! The server is a **pure reader** — a deterministic function of run-dir
//! bytes, with no RNG (ADR 0030). `AppState` is the shared discovery root plus
//! the injected [`ServeConfig`]; routes are mounted under `/v1/` (the single
//! version marker, ADR 0029) with the debug page at `/` outside it. Both the
//! `openteam serve` CLI and the crate's integration tests mount the identical
//! router; they differ only in who owns the listener's lifetime.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::config::ServeConfig;

/// Immutable shared server state (ADR 0030): the one discovery root and the
/// injected timing config. Cheap to clone — everything behind an `Arc`.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) root: Arc<PathBuf>,
    pub(crate) config: Arc<ServeConfig>,
}

impl AppState {
    pub(crate) fn new(root: PathBuf, config: ServeConfig) -> Self {
        Self {
            root: Arc::new(root),
            config: Arc::new(config),
        }
    }
}

/// The one axum app (ADR 0019/0030): contract routes under `/v1/`, the debug
/// page at `/`. `root` is the single discovery root (`--dir`, ADR 0027);
/// `config` carries the injected timing knobs.
pub fn build_router(root: PathBuf, config: ServeConfig) -> Router {
    let state = AppState::new(root, config);
    tracing::debug!(
        root = %state.root.display(),
        poll_ms = state.config.poll_interval.as_millis(),
        "stream server router built"
    );
    Router::new().with_state(state)
}

/// A graceful-shutdown handle for a served stream server: signals the listener
/// and awaits the serve task (ADR 0019).
pub struct ShutdownHandle {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl ShutdownHandle {
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// Bind `127.0.0.1:<port>` (`0` = OS-assigned ephemeral, ADR 0027), serve the
/// router on a background task, and hand back the bound address plus a
/// graceful-shutdown handle — the mock's `serve` pattern (ADR 0019/0030).
pub async fn serve(
    root: PathBuf,
    config: ServeConfig,
    port: u16,
) -> std::io::Result<(SocketAddr, ShutdownHandle)> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    let addr = listener.local_addr()?;
    let (shutdown, rx) = oneshot::channel::<()>();
    let router = build_router(root, config);
    let task = tokio::spawn(async move {
        let served = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
        if let Err(fault) = served {
            tracing::error!(%fault, "stream server terminated abnormally");
        }
    });
    tracing::info!(%addr, "stream server listening");
    Ok((addr, ShutdownHandle { shutdown, task }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn serve_binds_loopback_and_shuts_down_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        // A test-fast config, overriding the pinned production knobs — no CLI
        // surface involved (ADR 0030).
        let config = ServeConfig {
            poll_interval: std::time::Duration::from_millis(5),
            keep_alive: std::time::Duration::from_millis(50),
            retry_ms: 10,
            broadcast_capacity: 4,
        };
        let (addr, handle) = serve(dir.path().to_path_buf(), config, 0).await.unwrap();
        assert!(addr.ip().is_loopback());
        assert_ne!(
            addr.port(),
            0,
            "0 resolves to an OS-assigned ephemeral port"
        );
        handle.shutdown().await;
    }
}
