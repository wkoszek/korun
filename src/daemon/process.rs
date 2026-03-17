use chrono::Utc;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::collections::HashMap;
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
pub fn spawn_child(
    command: &[String],
    cwd: &str,
    env_overrides: &HashMap<String, String>,
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
        tokio::spawn(async move {
            read_stream(
                BufReader::new(stdout),
                Stream::Stdout,
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
        tokio::spawn(async move {
            read_stream(
                BufReader::new(stderr),
                Stream::Stderr,
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
    stream_buf: Arc<Mutex<LogBuffer>>,
    blended_buf: Arc<Mutex<LogBuffer>>,
    log_tx: broadcast::Sender<LogEntry>,
) {
    let mut lines = reader.lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let ts = Utc::now();
                let entry = {
                    let mut buf = stream_buf.lock().unwrap();
                    let seq = buf.push(stream, line.clone(), ts);
                    LogEntry {
                        seq,
                        ts,
                        stream,
                        line: line.clone(),
                    }
                };
                {
                    let mut blended = blended_buf.lock().unwrap();
                    blended.push(stream, line, ts);
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
        // Spawn child
        let spawn_result = {
            let (command, cwd, env, stdout_buf, stderr_buf, blended_buf, log_tx) = {
                mgr.with(&session_id, |s| {
                    (
                        s.command.clone(),
                        s.cwd.clone(),
                        s.env_overrides.clone(),
                        Arc::clone(&s.stdout_buf),
                        Arc::clone(&s.stderr_buf),
                        Arc::clone(&s.blended_buf),
                        s.log_tx.clone(),
                    )
                })
                .expect("session must exist")
            };
            spawn_child(
                &command,
                &cwd,
                &env,
                stdout_buf,
                stderr_buf,
                blended_buf,
                log_tx,
            )
        };

        let mut spawned = match spawn_result {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("failed to spawn session {session_id}: {e}");
                mgr.remove(&session_id);
                return;
            }
        };

        mgr.with_mut(&session_id, |s| {
            s.state = SessionState::Running;
            s.pid = Some(spawned.pid);
            s.last_started_at = Some(Utc::now());
        });

        // Wait for exit or command
        let restart_after = 'select: {
            tokio::select! {
                _ = spawned.child.wait() => {
                    mgr.remove(&session_id);
                    return; // no auto-restart on natural exit
                }
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(SessionCommand::Stop) => {
                            break 'select false; // stop, don't restart
                        }
                        Some(SessionCommand::Restart { is_watch }) => {
                            // Increment counters immediately
                            mgr.with_mut(&session_id, |s| {
                                s.restart_count += 1;
                                if is_watch {
                                    s.watch_restart_count += 1;
                                } else {
                                    s.manual_restart_count += 1;
                                }
                            });
                            break 'select true; // restart
                        }
                        None => return, // channel closed
                    }
                }
            }
        };

        // Hard kill: SIGKILL immediately, no grace period
        mgr.with_mut(&session_id, |s| {
            s.state = SessionState::Stopping;
        });

        let pid = spawned.pid as i32;
        // Kill the entire process group (negative PID) to catch grandchildren
        // such as the binary spawned by `cargo run`. Falls back to direct kill
        // on non-Unix where process groups aren't used.
        #[cfg(unix)]
        let _ = kill(Pid::from_raw(-pid), Signal::SIGKILL);
        #[cfg(not(unix))]
        let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
        let _ = spawned.child.wait().await;

        mgr.with_mut(&session_id, |s| s.pid = None);

        if !restart_after {
            mgr.remove(&session_id);
            return;
        }

        // Loop continues → restart
        mgr.with_mut(&session_id, |s| s.state = SessionState::Starting);
    }
}
