use crate::daemon::handlers::*;
use crate::daemon::session_mgr::SessionManager;
use axum::{
    routing::{get, post},
    Router,
};

pub fn build_router(mgr: SessionManager) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/sessions", get(list_sessions).post(create_session))
        .route("/v1/sessions/:id", get(get_session))
        .route("/v1/sessions/:id/restart", post(restart_session))
        .route("/v1/sessions/:id/stop", post(stop_session))
        .route("/v1/sessions/:id/logs", get(get_logs))
        .route("/v1/sessions/:id/head", get(get_head))
        .route("/v1/sessions/:id/tail", get(get_tail))
        .with_state(mgr)
}
