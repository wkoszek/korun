// tests/process_test.rs
use korun::daemon::buffer::{LogBuffer, Stream};
use korun::daemon::process::spawn_child;
use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::time::sleep;

#[tokio::test]
async fn spawn_echo_captures_stdout() {
    let stdout_buf = Arc::new(Mutex::new(LogBuffer::new(100)));
    let stderr_buf = Arc::new(Mutex::new(LogBuffer::new(100)));
    let blended_buf = Arc::new(Mutex::new(LogBuffer::new(100)));
    let (log_tx, _) = broadcast::channel(64);

    let mut spawned = spawn_child(
        &["echo".to_string(), "hello from korun".to_string()],
        "/tmp",
        &HashMap::new(),
        Arc::new(AtomicU64::new(0)),
        Arc::clone(&stdout_buf),
        Arc::clone(&stderr_buf),
        Arc::clone(&blended_buf),
        log_tx,
    )
    .unwrap();

    spawned.child.wait().await.unwrap();
    sleep(Duration::from_millis(50)).await;

    let buf = stdout_buf.lock().unwrap();
    let entries = buf.tail(10);
    assert!(!entries.is_empty());
    assert_eq!(entries[0].line, "hello from korun");
    assert_eq!(entries[0].stream, Stream::Stdout);
}

use korun::daemon::process::run_supervisor;
use korun::daemon::session::{Session, SessionCommand, SessionState};
use korun::daemon::session_mgr::SessionManager;
use tokio::sync::mpsc;

#[tokio::test]
async fn supervisor_stop_transitions_to_exited() {
    let mgr = SessionManager::new();
    let id = uuid::Uuid::new_v4();
    let (cmd_tx, cmd_rx) = mpsc::channel(8);
    let (log_tx, _) = broadcast::channel(64);

    let session = Session::new(
        id,
        vec!["sleep".to_string(), "10".to_string()],
        "/tmp".to_string(),
        Default::default(),
        vec![],
        cmd_tx.clone(),
        log_tx,
        None,
    );
    mgr.insert(session);

    let mgr2 = mgr.clone();
    tokio::spawn(async move {
        run_supervisor(id, mgr2, cmd_rx).await;
    });

    // Wait up to 2s for the supervisor to reach Running state
    let state = {
        let mut final_state = SessionState::Starting;
        for _ in 0..40 {
            sleep(Duration::from_millis(50)).await;
            final_state = mgr.with(&id, |s| s.state).unwrap();
            if final_state == SessionState::Running {
                break;
            }
        }
        final_state
    };
    assert_eq!(state, SessionState::Running);

    // Send stop — session remains available for inspection in exited state
    cmd_tx.send(SessionCommand::Stop).await.unwrap();
    let mut state = SessionState::Stopping;
    for _ in 0..40 {
        sleep(Duration::from_millis(50)).await;
        state = mgr.with(&id, |s| s.state).unwrap();
        if state == SessionState::Exited {
            break;
        }
    }

    assert_eq!(state, SessionState::Exited);
    assert_eq!(mgr.with(&id, |s| s.pid).unwrap(), None);
    assert!(mgr.with(&id, |s| s.last_stopped_at).unwrap().is_some());
}

#[tokio::test]
async fn supervisor_persists_natural_exit_metadata() {
    let mgr = SessionManager::new();
    let id = uuid::Uuid::new_v4();
    let (cmd_tx, cmd_rx) = mpsc::channel(8);
    let (log_tx, _) = broadcast::channel(64);

    let session = Session::new(
        id,
        vec!["sh".to_string(), "-c".to_string(), "exit 7".to_string()],
        "/tmp".to_string(),
        Default::default(),
        vec![],
        cmd_tx,
        log_tx,
        None,
    );
    mgr.insert(session);

    let mgr2 = mgr.clone();
    tokio::spawn(async move {
        run_supervisor(id, mgr2, cmd_rx).await;
    });

    let mut state = SessionState::Starting;
    for _ in 0..40 {
        sleep(Duration::from_millis(50)).await;
        state = mgr.with(&id, |s| s.state).unwrap();
        if state == SessionState::Exited {
            break;
        }
    }

    assert_eq!(state, SessionState::Exited);
    assert_eq!(mgr.with(&id, |s| s.exit_code).unwrap(), Some(7));
    assert_eq!(mgr.with(&id, |s| s.pid).unwrap(), None);
}

#[tokio::test]
async fn supervisor_restart_from_exited_returns_to_running() {
    let mgr = SessionManager::new();
    let id = uuid::Uuid::new_v4();
    let (cmd_tx, cmd_rx) = mpsc::channel(8);
    let (log_tx, _) = broadcast::channel(64);

    let session = Session::new(
        id,
        vec!["sleep".to_string(), "10".to_string()],
        "/tmp".to_string(),
        Default::default(),
        vec![],
        cmd_tx.clone(),
        log_tx,
        None,
    );
    mgr.insert(session);

    let mgr2 = mgr.clone();
    tokio::spawn(async move {
        run_supervisor(id, mgr2, cmd_rx).await;
    });

    for _ in 0..40 {
        sleep(Duration::from_millis(50)).await;
        if mgr.with(&id, |s| s.state).unwrap() == SessionState::Running {
            break;
        }
    }

    cmd_tx.send(SessionCommand::Stop).await.unwrap();
    for _ in 0..40 {
        sleep(Duration::from_millis(50)).await;
        if mgr.with(&id, |s| s.state).unwrap() == SessionState::Exited {
            break;
        }
    }
    assert_eq!(mgr.with(&id, |s| s.state).unwrap(), SessionState::Exited);

    cmd_tx
        .send(SessionCommand::Restart { is_watch: false })
        .await
        .unwrap();
    for _ in 0..40 {
        sleep(Duration::from_millis(50)).await;
        if mgr.with(&id, |s| s.state).unwrap() == SessionState::Running {
            break;
        }
    }
    assert_eq!(mgr.with(&id, |s| s.state).unwrap(), SessionState::Running);
    assert_eq!(mgr.with(&id, |s| s.manual_restart_count).unwrap(), 1);

    cmd_tx.send(SessionCommand::Stop).await.unwrap();
}

#[tokio::test]
async fn spawn_child_uses_global_log_sequence_across_streams() {
    let stdout_buf = Arc::new(Mutex::new(LogBuffer::new(100)));
    let stderr_buf = Arc::new(Mutex::new(LogBuffer::new(100)));
    let blended_buf = Arc::new(Mutex::new(LogBuffer::new(100)));
    let (log_tx, _) = broadcast::channel(64);

    let mut spawned = spawn_child(
        &[
            "sh".to_string(),
            "-c".to_string(),
            "printf 'out\\n'; printf 'err\\n' 1>&2".to_string(),
        ],
        "/tmp",
        &HashMap::new(),
        Arc::new(AtomicU64::new(0)),
        Arc::clone(&stdout_buf),
        Arc::clone(&stderr_buf),
        Arc::clone(&blended_buf),
        log_tx,
    )
    .unwrap();

    spawned.child.wait().await.unwrap();
    sleep(Duration::from_millis(50)).await;

    let blended = blended_buf.lock().unwrap().tail(10);
    assert_eq!(blended.len(), 2);
    assert_ne!(blended[0].seq, blended[1].seq);
    assert!(blended[0].seq < blended[1].seq);
}
