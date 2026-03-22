use chrono::Utc;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

use crate::daemon::buffer::{LogBuffer, LogEntry, Stream};
use crate::daemon::session::{SessionCommand, SessionState};
use crate::daemon::session_mgr::SessionManager;

pub struct SpawnedChild {
    pub pid: u32,
    pub child: tokio::process::Child,
}

/// Spawn the child process and start stdout/stderr reader tasks.
#[allow(clippy::too_many_arguments)]
pub fn spawn_child(
    command: &[String],
    cwd: &str,
    env_overrides: &HashMap<String, String>,
    next_seq: Arc<AtomicU64>,
    stdout_buf: Arc<Mutex<LogBuffer>>,
    stderr_buf: Arc<Mutex<LogBuffer>>,
    blended_buf: Arc<Mutex<LogBuffer>>,
    log_tx: broadcast::Sender<LogEntry>,
) -> anyhow::Result<SpawnedChild> {
    let (prog, args) = command
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("empty command"))?;

    let mut cmd = Command::new(prog);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true); // fallback: kills direct child if supervisor drops

    // Put child in its own process group so killing -pgid kills the whole tree
    // (e.g. `cargo run` → `coreapp-server` both die)
    #[cfg(unix)]
    cmd.process_group(0);

    for (k, v) in env_overrides {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn()?;
    let pid = child
        .id()
        .ok_or_else(|| anyhow::anyhow!("child has no pid"))?;

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    // stdout reader task
    {
        let stdout_buf = Arc::clone(&stdout_buf);
        let blended_buf = Arc::clone(&blended_buf);
        let log_tx = log_tx.clone();
        let next_seq_stdout = next_seq.clone();
        tokio::spawn(async move {
            read_stream(
                BufReader::new(stdout),
                Stream::Stdout,
                next_seq_stdout,
                stdout_buf,
                blended_buf,
                log_tx,
            )
            .await;
        });
    }

    // stderr reader task
    {
        let stderr_buf = Arc::clone(&stderr_buf);
        let blended_buf = Arc::clone(&blended_buf);
        let log_tx = log_tx.clone();
        let next_seq_stderr = next_seq;
        tokio::spawn(async move {
            read_stream(
                BufReader::new(stderr),
                Stream::Stderr,
                next_seq_stderr,
                stderr_buf,
                blended_buf,
                log_tx,
            )
            .await;
        });
    }

    Ok(SpawnedChild { pid, child })
}

async fn read_stream<R: tokio::io::AsyncRead + Unpin>(
    reader: BufReader<R>,
    stream: Stream,
    next_seq: Arc<AtomicU64>,
    stream_buf: Arc<Mutex<LogBuffer>>,
    blended_buf: Arc<Mutex<LogBuffer>>,
    log_tx: broadcast::Sender<LogEntry>,
) {
    let mut lines = reader.lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let ts = Utc::now();
                let seq = next_seq.fetch_add(1, Ordering::Relaxed);
                let entry = {
                    let mut buf = stream_buf.lock().unwrap();
                    buf.push_with_seq(seq, stream, line.clone(), ts);
                    LogEntry {
                        seq,
                        ts,
                        stream,
                        line: line.clone(),
                    }
                };
                {
                    let mut blended = blended_buf.lock().unwrap();
                    blended.push_with_seq(seq, stream, line, ts);
                }
                let _ = log_tx.send(entry);
            }
            Ok(None) => break, // EOF
            Err(e) => {
                tracing::warn!("read_stream I/O error ({:?}): {e}", stream);
                break;
            }
        }
    }
}

/// Runs the supervisor loop for a session.
/// Spawns the child, handles Stop/Restart commands, and updates SessionManager state.
pub async fn run_supervisor(
    session_id: Uuid,
    mgr: SessionManager,
    mut cmd_rx: mpsc::Receiver<SessionCommand>,
) {
    loop {
        let state = match mgr.with(&session_id, |s| s.state) {
            Some(state) => state,
            None => return,
        };

        match state {
            SessionState::Starting => {
                let spawn_result = {
                    let (command, cwd, env, next_seq, stdout_buf, stderr_buf, blended_buf, log_tx) =
                        match mgr.with(&session_id, |s| {
                            (
                                s.command.clone(),
                                s.cwd.clone(),
                                s.env_overrides.clone(),
                                Arc::clone(&s.next_log_seq),
                                Arc::clone(&s.stdout_buf),
                                Arc::clone(&s.stderr_buf),
                                Arc::clone(&s.blended_buf),
                                s.log_tx.clone(),
                            )
                        }) {
                            Some(values) => values,
                            None => return,
                        };
                    spawn_child(
                        &command,
                        &cwd,
                        &env,
                        next_seq,
                        stdout_buf,
                        stderr_buf,
                        blended_buf,
                        log_tx,
                    )
                };

                match spawn_result {
                    Ok(spawned) => {
                        mgr.with_mut(&session_id, |s| {
                            s.clear_exit_metadata();
                            s.state = SessionState::Running;
                            s.pid = Some(spawned.pid);
                            s.last_started_at = Some(Utc::now());
                        });
                        if supervise_running_session(session_id, &mgr, &mut cmd_rx, spawned).await {
                            continue;
                        }
                    }
                    Err(e) => {
                        tracing::error!("failed to spawn session {session_id}: {e}");
                        mgr.with_mut(&session_id, |s| {
                            s.state = SessionState::Failed;
                            s.pid = None;
                            s.last_stopped_at = Some(Utc::now());
                            s.exit_code = None;
                            s.term_signal = None;
                        });
                    }
                }
            }
            SessionState::Running => {
                tracing::warn!("session {session_id} entered supervisor loop in running state");
                mgr.with_mut(&session_id, |s| s.state = SessionState::Starting);
            }
            SessionState::Stopping => {
                tracing::warn!("session {session_id} entered supervisor loop in stopping state");
                mgr.with_mut(&session_id, |s| s.state = SessionState::Exited);
            }
            SessionState::Exited | SessionState::Failed => {
                let cmd = match cmd_rx.recv().await {
                    Some(cmd) => cmd,
                    None => return,
                };
                match cmd {
                    SessionCommand::Restart { is_watch } => {
                        mgr.with_mut(&session_id, |s| {
                            s.restart_count += 1;
                            if is_watch {
                                s.watch_restart_count += 1;
                            } else {
                                s.manual_restart_count += 1;
                            }
                            s.clear_exit_metadata();
                            s.state = SessionState::Starting;
                            s.pid = None;
                        });
                    }
                    SessionCommand::Stop => {}
                }
            }
        }
    }
}

async fn supervise_running_session(
    session_id: Uuid,
    mgr: &SessionManager,
    cmd_rx: &mut mpsc::Receiver<SessionCommand>,
    mut spawned: SpawnedChild,
) -> bool {
    let restart_after = 'select: {
        tokio::select! {
            status = spawned.child.wait() => {
                match status {
                    Ok(status) => record_terminal_exit(mgr, &session_id, SessionState::Exited, status),
                    Err(e) => {
                        tracing::error!("failed to wait on session {session_id}: {e}");
                        mgr.with_mut(&session_id, |s| {
                            s.state = SessionState::Failed;
                            s.pid = None;
                            s.last_stopped_at = Some(Utc::now());
                            s.exit_code = None;
                            s.term_signal = None;
                        });
                    }
                }
                return false;
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(SessionCommand::Stop) => {
                        break 'select false;
                    }
                    Some(SessionCommand::Restart { is_watch }) => {
                        mgr.with_mut(&session_id, |s| {
                            s.restart_count += 1;
                            if is_watch {
                                s.watch_restart_count += 1;
                            } else {
                                s.manual_restart_count += 1;
                            }
                        });
                        break 'select true;
                    }
                    None => return false,
                }
            }
        }
    };

    mgr.with_mut(&session_id, |s| {
        s.state = SessionState::Stopping;
    });

    let status = terminate_child(&mut spawned).await;
    record_terminal_exit(mgr, &session_id, SessionState::Exited, status);

    if restart_after {
        mgr.with_mut(&session_id, |s| {
            s.clear_exit_metadata();
            s.state = SessionState::Starting;
        });
    }

    restart_after
}

async fn terminate_child(child: &mut SpawnedChild) -> std::process::ExitStatus {
    let pid = child.pid as i32;
    #[cfg(unix)]
    let _ = kill(Pid::from_raw(-pid), Signal::SIGTERM);
    #[cfg(not(unix))]
    let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);

    match tokio::time::timeout(std::time::Duration::from_secs(2), child.child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => {
            tracing::warn!("failed while waiting for child shutdown: {e}");
            child
                .child
                .wait()
                .await
                .expect("child wait after shutdown error")
        }
        Err(_) => {
            #[cfg(unix)]
            let _ = kill(Pid::from_raw(-pid), Signal::SIGKILL);
            #[cfg(not(unix))]
            let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
            child
                .child
                .wait()
                .await
                .expect("child wait after SIGKILL")
        }
    }
}

fn record_terminal_exit(
    mgr: &SessionManager,
    session_id: &Uuid,
    state: SessionState,
    status: std::process::ExitStatus,
) {
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;

    mgr.with_mut(session_id, |s| {
        s.state = state;
        s.pid = None;
        s.last_stopped_at = Some(Utc::now());
        s.exit_code = status.code();
        #[cfg(unix)]
        {
            s.term_signal = status.signal();
        }
        #[cfg(not(unix))]
        {
            s.term_signal = None;
        }
    });
}
