use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use futures::StreamExt;
use serde::Deserialize;
use std::collections::HashMap;
use tokio_stream::wrappers::BroadcastStream;
use uuid::Uuid;

use crate::daemon::buffer::{LogEntry, Stream};
use crate::daemon::error::AppError;
use crate::daemon::process::run_supervisor;
use crate::daemon::session::{Session, SessionCommand, SessionState, BROADCAST_CAPACITY};
use crate::daemon::session_mgr::SessionManager;
use crate::daemon::watcher::start_watcher;

pub type AppState = SessionManager;

// ── Healthz ──────────────────────────────────────────────────────────────────

pub async fn healthz() -> impl IntoResponse {
    Json(serde_json::json!({
        "ok": true,
        "service": "korun",
        "time": Utc::now().to_rfc3339(),
    }))
}

// ── Sessions list ─────────────────────────────────────────────────────────────

pub async fn list_sessions(State(mgr): State<AppState>) -> impl IntoResponse {
    let ids = mgr.list();
    let sessions: Vec<_> = ids
        .iter()
        .filter_map(|id| {
            mgr.with(id, |s| {
                serde_json::json!({
                    "id": s.id,
                    "state": s.state,
                    "command": s.command,
                    "cwd": s.cwd,
                    "pid": s.pid,
                    "started_at": s.started_at.to_rfc3339(),
                    "restart_count": s.restart_count,
                })
            })
        })
        .collect();
    Json(serde_json::json!({ "sessions": sessions }))
}

// ── Create session ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub command: Vec<String>,
    pub cwd: Option<String>,
    pub watch: Option<Vec<String>>,
    pub env: Option<HashMap<String, String>>,
}

pub async fn create_session(
    State(mgr): State<AppState>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<impl IntoResponse, AppError> {
    if req.command.is_empty() {
        return Err(AppError::BadRequest("command must not be empty".into()));
    }
    let cwd = req.cwd.unwrap_or_else(|| "/tmp".to_string());
    let watch = req.watch.unwrap_or_default();
    let env = req.env.unwrap_or_default();
    let id = Uuid::new_v4();

    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(8);
    let (log_tx, _) = tokio::sync::broadcast::channel(BROADCAST_CAPACITY);

    // Resolve watch paths relative to cwd
    let resolved_watch: Vec<String> = watch
        .iter()
        .map(|p| {
            if std::path::Path::new(p).is_absolute() {
                p.clone()
            } else {
                format!("{cwd}/{p}")
            }
        })
        .collect();

    let session = Session::new(
        id,
        req.command,
        cwd,
        env,
        resolved_watch.clone(),
        cmd_tx.clone(),
        log_tx.clone(),
    );
    mgr.insert(session);

    // Start watcher if paths given
    if !resolved_watch.is_empty() {
        let cmd_tx2 = cmd_tx.clone();
        let mgr2 = mgr.clone();
        let id2 = id;
        let (change_tx, mut change_rx) = tokio::sync::mpsc::channel::<String>(32);

        // Bridge change events to restart commands
        tokio::spawn(async move {
            while let Some(path) = change_rx.recv().await {
                // Update file change metadata
                mgr2.with_mut(&id2, |s| {
                    s.file_change_count += 1;
                    s.last_change_at = Some(Utc::now());
                    s.last_change_path = Some(path.clone());
                });
                let _ = cmd_tx2
                    .send(SessionCommand::Restart { is_watch: true })
                    .await;
            }
        });

        let watch_paths = resolved_watch.clone();
        if let Err(e) = start_watcher(watch_paths, change_tx) {
            tracing::warn!("failed to start watcher: {e}");
        }
    }

    // Launch supervisor
    let mgr3 = mgr.clone();
    tokio::spawn(async move {
        run_supervisor(id, mgr3, cmd_rx).await;
    });

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": id, "state": "starting" })),
    ))
}

// ── Get session ───────────────────────────────────────────────────────────────

pub async fn get_session(
    State(mgr): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    mgr.with(&id, |s| {
        // Read all stats under a single lock acquisition per buffer to avoid races
        let (stdout_lines, stdout_dropped, stdout_bytes) = {
            let b = s.stdout_buf.lock().unwrap();
            (b.len(), b.dropped, b.total_bytes)
        };
        let (stderr_lines, stderr_dropped, stderr_bytes) = {
            let b = s.stderr_buf.lock().unwrap();
            (b.len(), b.dropped, b.total_bytes)
        };
        let (blended_lines, blended_dropped) = {
            let b = s.blended_buf.lock().unwrap();
            (b.len(), b.dropped)
        };

        Json(serde_json::json!({
            "id": s.id,
            "state": s.state,
            "command": s.command,
            "cwd": s.cwd,
            "env_overrides": s.env_overrides,
            "watch": s.watch,
            "pid": s.pid,
            "started_at": s.started_at.to_rfc3339(),
            "last_started_at": s.last_started_at.map(|t| t.to_rfc3339()),
            "last_stopped_at": s.last_stopped_at.map(|t| t.to_rfc3339()),
            "uptime_ms": s.uptime_ms(),
            "restart_count": s.restart_count,
            "manual_restart_count": s.manual_restart_count,
            "watch_restart_count": s.watch_restart_count,
            "file_change_count": s.file_change_count,
            "last_change_at": s.last_change_at.map(|t| t.to_rfc3339()),
            "last_change_path": s.last_change_path,
            "exit_code": s.exit_code,
            "term_signal": s.term_signal,
            "stdout_lines": stdout_lines,
            "stderr_lines": stderr_lines,
            "blended_lines": blended_lines,
            "stdout_dropped_lines": stdout_dropped,
            "stderr_dropped_lines": stderr_dropped,
            "blended_dropped_lines": blended_dropped,
            "stdout_bytes": stdout_bytes,
            "stderr_bytes": stderr_bytes,
        }))
    })
    .ok_or_else(|| AppError::NotFound("session not found".into()))
}

// ── Restart ───────────────────────────────────────────────────────────────────

pub async fn restart_session(
    State(mgr): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let (state, cmd_tx) = mgr
        .with(&id, |s| (s.state, s.cmd_tx.clone()))
        .ok_or_else(|| AppError::NotFound("session not found".into()))?;

    // If already stopping, return current state without queuing
    if state == SessionState::Stopping {
        return Ok(Json(
            serde_json::json!({ "ok": true, "id": id, "state": state }),
        ));
    }

    cmd_tx
        .send(SessionCommand::Restart { is_watch: false })
        .await
        .map_err(|_| AppError::Internal("supervisor task gone".into()))?;

    Ok(Json(
        serde_json::json!({ "ok": true, "id": id, "state": "starting" }),
    ))
}

// ── Stop ─────────────────────────────────────────────────────────────────────

pub async fn stop_session(
    State(mgr): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let (state, cmd_tx) = mgr
        .with(&id, |s| (s.state, s.cmd_tx.clone()))
        .ok_or_else(|| AppError::NotFound("session not found".into()))?;

    if state == SessionState::Stopping {
        return Ok(Json(
            serde_json::json!({ "ok": true, "id": id, "state": state }),
        ));
    }

    cmd_tx
        .send(SessionCommand::Stop)
        .await
        .map_err(|_| AppError::Internal("supervisor task gone".into()))?;

    Ok(Json(
        serde_json::json!({ "ok": true, "id": id, "state": "stopping" }),
    ))
}

// ── Logs ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    pub stream: Option<String>,
    pub limit: Option<usize>,
    pub since_seq: Option<u64>,
    pub follow: Option<u8>,
    pub format: Option<String>,
}

pub async fn get_logs(
    State(mgr): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<LogsQuery>,
) -> Result<Response, AppError> {
    // Clone to owned String to avoid lifetime issues in the async streaming closure
    let stream_name = q.stream.clone().unwrap_or_else(|| "blended".to_string());
    let limit = q.limit.unwrap_or(100);
    let follow = q.follow.unwrap_or(0) == 1;
    let format = q.format.clone().unwrap_or_else(|| "json".to_string());

    let (entries, next_seq, log_tx) = mgr
        .with(&id, |s| {
            let buf: std::sync::MutexGuard<'_, crate::daemon::buffer::LogBuffer> =
                match stream_name.as_str() {
                    "stdout" => s.stdout_buf.lock().unwrap(),
                    "stderr" => s.stderr_buf.lock().unwrap(),
                    _ => s.blended_buf.lock().unwrap(),
                };
            let entries = if let Some(seq) = q.since_seq {
                buf.since_seq(seq, limit)
            } else {
                buf.tail(limit)
            };
            let next_seq = buf.peek_next_seq();
            drop(buf);
            (entries, next_seq, s.log_tx.clone())
        })
        .ok_or_else(|| AppError::NotFound("session not found".into()))?;

    if !follow {
        return Ok(render_snapshot(id, &stream_name, entries, next_seq, &format).into_response());
    }

    // Streaming follow mode
    let rx = log_tx.subscribe();
    // Both stream_name and format are owned Strings — safe to move into the closure
    let stream_name_clone = stream_name.clone();
    let format_clone = format.clone();
    let stream = BroadcastStream::new(rx).flat_map(move |msg| {
        let line = match msg {
            Ok(entry) => {
                // Filter: for blended, show all; for stdout/stderr, show only that stream; for unknown, show nothing
                let should_include = match stream_name_clone.as_str() {
                    "blended" => true,
                    "stdout" => entry.stream == Stream::Stdout,
                    "stderr" => entry.stream == Stream::Stderr,
                    _ => false, // unknown stream name → no entries
                };
                if !should_include {
                    return futures::stream::empty().left_stream();
                }
                format_entry(&entry, &format_clone)
            }
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                if format_clone == "text" {
                    format!("[korun: dropped {n} lines]\n")
                } else {
                    let e = serde_json::json!({
                        "seq": 0u64,
                        "ts": Utc::now().to_rfc3339(),
                        "stream": "system",
                        "line": format!("dropped {n} lines"),
                    });
                    format!("{e}\n")
                }
            }
        };
        futures::stream::once(async move { Ok::<_, std::convert::Infallible>(line) }).right_stream()
    });

    // First send buffered entries, then stream new ones.
    // Use into_iter() + move to avoid capturing &format (which would prevent 'static bound).
    let format2 = format.clone();
    let initial = entries
        .into_iter()
        .map(move |e| Ok::<_, std::convert::Infallible>(format_entry(&e, &format2)));
    let combined = futures::stream::iter(initial).chain(stream);

    Ok(Body::from_stream(combined).into_response())
}

pub async fn get_head(
    State(mgr): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<LogsQuery>,
) -> Result<impl IntoResponse, AppError> {
    let stream_name = q.stream.clone().unwrap_or_else(|| "blended".to_string());
    let limit = q.limit.unwrap_or(100);
    let format = q.format.clone().unwrap_or_else(|| "json".to_string());

    mgr.with(&id, |s| {
        let buf: std::sync::MutexGuard<'_, crate::daemon::buffer::LogBuffer> =
            match stream_name.as_str() {
                "stdout" => s.stdout_buf.lock().unwrap(),
                "stderr" => s.stderr_buf.lock().unwrap(),
                _ => s.blended_buf.lock().unwrap(),
            };
        let entries = buf.head(limit);
        let next_seq = buf.peek_next_seq();
        drop(buf);
        render_snapshot(id, &stream_name, entries, next_seq, &format)
    })
    .ok_or_else(|| AppError::NotFound("session not found".into()))
}

pub async fn get_tail(
    State(mgr): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<LogsQuery>,
) -> Result<Response, AppError> {
    // Tail reuses the same logic as logs but defaults to tail semantics
    get_logs(State(mgr), Path(id), Query(q)).await
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn render_snapshot(
    session_id: Uuid,
    stream: &str,
    entries: Vec<LogEntry>,
    next_seq: u64,
    format: &str,
) -> Response {
    if format == "text" {
        let text: String = entries
            .iter()
            .map(|e| {
                if stream == "blended" {
                    format!("[{}] {}\n", stream_label(e.stream), e.line)
                } else {
                    format!("{}\n", e.line)
                }
            })
            .collect();
        return text.into_response();
    }
    Json(serde_json::json!({
        "session_id": session_id,
        "stream": stream,
        "entries": entries,
        "next_seq": next_seq,
    }))
    .into_response()
}

fn format_entry(entry: &LogEntry, format: &str) -> String {
    if format == "text" {
        format!("[{}] {}\n", stream_label(entry.stream), entry.line)
    } else {
        format!("{}\n", serde_json::to_string(entry).unwrap_or_default())
    }
}

fn stream_label(s: Stream) -> &'static str {
    match s {
        Stream::Stdout => "stdout",
        Stream::Stderr => "stderr",
        Stream::System => "system",
    }
}
