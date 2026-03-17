use std::net::SocketAddr;

use crate::daemon::router::build_router;
use crate::daemon::session::SessionCommand;
use crate::daemon::session_mgr::SessionManager;

pub mod buffer;
pub mod error;
pub mod handlers;
pub mod process;
pub mod router;
pub mod session;
pub mod session_mgr;
pub mod watcher;

pub async fn run_daemon(addr: SocketAddr) -> anyhow::Result<()> {
    let mgr = SessionManager::new();
    let app = build_router(mgr.clone());

    tracing::info!("korun daemon listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;

    let shutdown = build_shutdown(mgr);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

async fn build_shutdown(mgr: SessionManager) {
    // Wait for Ctrl-C or SIGTERM
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
    }

    tracing::info!("shutdown: killing all sessions");
    for (_, cmd_tx) in mgr.all_cmd_txs() {
        let _ = cmd_tx.try_send(SessionCommand::Stop);
    }
    // Brief pause for supervisors to SIGKILL their children
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
}
