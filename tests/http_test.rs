use axum::body::Body;
use axum::http::{Request, StatusCode};
use korun::daemon::router::build_router;
use korun::daemon::session_mgr::SessionManager;
use serde_json::Value;
use tempfile::tempdir;
use tower::util::ServiceExt;

#[tokio::test]
async fn healthz_returns_200() {
    let mgr = SessionManager::new();
    let app = build_router(mgr);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn list_sessions_empty() {
    let mgr = SessionManager::new();
    let app = build_router(mgr);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/sessions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["sessions"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn get_unknown_session_returns_404() {
    let mgr = SessionManager::new();
    let app = build_router(mgr);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/sessions/00000000-0000-0000-0000-000000000000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_session_returns_201() {
    let mgr = SessionManager::new();
    let app = build_router(mgr);

    let body = serde_json::json!({
        "command": ["echo", "hello"],
        "cwd": "/tmp"
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/sessions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(json["id"].as_str().is_some());
    assert_eq!(json["state"].as_str().unwrap(), "starting");
}

#[tokio::test]
async fn create_session_rejects_invalid_cwd() {
    let mgr = SessionManager::new();
    let app = build_router(mgr);

    let body = serde_json::json!({
        "command": ["echo", "hello"],
        "cwd": "/definitely/not/a/real/path"
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/sessions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_session_retains_watcher_handle() {
    let mgr = SessionManager::new();
    let app = build_router(mgr.clone());
    let tmp = tempdir().unwrap();
    let watch_path = tmp.path().join("watched.txt");
    std::fs::write(&watch_path, "before").unwrap();

    let body = serde_json::json!({
        "command": ["sleep", "10"],
        "cwd": tmp.path(),
        "watch": ["watched.txt"]
    });

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/sessions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    let id: uuid::Uuid = json["id"].as_str().unwrap().parse().unwrap();
    assert!(mgr.with(&id, |s| s.watcher.is_some()).unwrap());
}
