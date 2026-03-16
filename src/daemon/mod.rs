use std::net::SocketAddr;

use crate::daemon::router::build_router;
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
    let app = build_router(mgr);

    tracing::info!("korun daemon listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
