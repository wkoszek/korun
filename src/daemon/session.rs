use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

use crate::daemon::buffer::LogBuffer;
use crate::daemon::watcher::WatcherHandle;

pub const STDOUT_BUF_CAPACITY: usize = 10_000;
pub const STDERR_BUF_CAPACITY: usize = 10_000;
pub const BLENDED_BUF_CAPACITY: usize = 20_000;
pub const BROADCAST_CAPACITY: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    Starting,
    Running,
    Stopping,
    Exited,
    Failed,
}

/// Commands sent to the supervisor task.
#[derive(Debug)]
pub enum SessionCommand {
    Stop,
    /// `is_watch` distinguishes file-watch-triggered from manual restart.
    Restart {
        is_watch: bool,
    },
}

/// The full session record stored in SessionManager.
pub struct Session {
    pub id: Uuid,
    pub state: SessionState,
    pub command: Vec<String>,
    pub cwd: String,
    pub env_overrides: std::collections::HashMap<String, String>,
    pub watch: Vec<String>,

    // child info
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub term_signal: Option<i32>,

    // timestamps
    pub started_at: DateTime<Utc>,
    pub last_started_at: Option<DateTime<Utc>>,
    pub last_stopped_at: Option<DateTime<Utc>>,

    // counters
    pub restart_count: u32,
    pub manual_restart_count: u32,
    pub watch_restart_count: u32,
    pub file_change_count: u64,
    pub last_change_at: Option<DateTime<Utc>>,
    pub last_change_path: Option<String>,
    pub next_log_seq: Arc<AtomicU64>,

    // log buffers
    pub stdout_buf: Arc<Mutex<LogBuffer>>,
    pub stderr_buf: Arc<Mutex<LogBuffer>>,
    pub blended_buf: Arc<Mutex<LogBuffer>>,
    #[allow(dead_code)]
    pub watcher: Option<WatcherHandle>,

    // inter-task handles
    pub cmd_tx: mpsc::Sender<SessionCommand>,
    pub log_tx: broadcast::Sender<crate::daemon::buffer::LogEntry>,
}

impl Session {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: Uuid,
        command: Vec<String>,
        cwd: String,
        env_overrides: std::collections::HashMap<String, String>,
        watch: Vec<String>,
        cmd_tx: mpsc::Sender<SessionCommand>,
        log_tx: broadcast::Sender<crate::daemon::buffer::LogEntry>,
        watcher: Option<WatcherHandle>,
    ) -> Self {
        Self {
            id,
            state: SessionState::Starting,
            command,
            cwd,
            env_overrides,
            watch,
            pid: None,
            exit_code: None,
            term_signal: None,
            started_at: Utc::now(),
            last_started_at: None,
            last_stopped_at: None,
            restart_count: 0,
            manual_restart_count: 0,
            watch_restart_count: 0,
            file_change_count: 0,
            last_change_at: None,
            last_change_path: None,
            next_log_seq: Arc::new(AtomicU64::new(0)),
            stdout_buf: Arc::new(Mutex::new(LogBuffer::new(STDOUT_BUF_CAPACITY))),
            stderr_buf: Arc::new(Mutex::new(LogBuffer::new(STDERR_BUF_CAPACITY))),
            blended_buf: Arc::new(Mutex::new(LogBuffer::new(BLENDED_BUF_CAPACITY))),
            watcher,
            cmd_tx,
            log_tx,
        }
    }

    /// Uptime in milliseconds since last_started_at, or 0.
    pub fn uptime_ms(&self) -> u64 {
        if !matches!(self.state, SessionState::Running | SessionState::Stopping) {
            return 0;
        }
        self.last_started_at.map_or(0, |t| {
            Utc::now()
                .signed_duration_since(t)
                .num_milliseconds()
                .max(0) as u64
        })
    }

    pub fn clear_exit_metadata(&mut self) {
        self.exit_code = None;
        self.term_signal = None;
    }

    pub fn next_seq(&self) -> u64 {
        self.next_log_seq.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_state_serializes() {
        assert_eq!(
            serde_json::to_string(&SessionState::Running).unwrap(),
            "\"running\""
        );
        assert_eq!(
            serde_json::to_string(&SessionState::Exited).unwrap(),
            "\"exited\""
        );
        assert_eq!(
            serde_json::to_string(&SessionState::Failed).unwrap(),
            "\"failed\""
        );
    }

    #[test]
    fn session_command_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<SessionCommand>();
    }
}
