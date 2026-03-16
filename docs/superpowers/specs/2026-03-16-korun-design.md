# KORun Design Document

**Date:** 2026-03-16
**Status:** Approved
**Language:** Rust
**Crate name:** `korun`

---

## Overview

KORun is a local developer process runner implemented as a single Rust binary. It runs as a local daemon on `127.0.0.1:7777`, managing one or more named sessions. Each session supervises a developer command, captures its output, watches files for changes, and exposes state and logs over a curl-friendly HTTP API.

---

## Stack

| Concern | Crate |
|---|---|
| Async runtime | `tokio` (multi-thread) |
| HTTP server | `axum` |
| HTTP client (CLI) | `reqwest` |
| File watching | `notify` + `notify-debouncer-mini` |
| CLI parsing | `clap` (derive macros) |
| UUIDs | `uuid` |
| Timestamps | `chrono` |
| Serialization | `serde` + `serde_json` |

---

## Architecture: Single Binary

One binary serves as both daemon and CLI client.

### `korun serve` — daemon auto-start behavior

`korun serve -- <cmd>` does the following on each invocation:

1. Attempt to connect to `127.0.0.1:7777`. If the daemon is not running, daemonize using a double-fork: the fork **must happen before any tokio runtime threads are spawned** (i.e., in `main()` before `#[tokio::main]` or before `Runtime::new()`). The grandchild calls `setsid()`, redirects stdin/stdout/stderr to `/dev/null`, and then starts the tokio runtime and axum daemon loop. Forking after a multi-threaded tokio runtime is running is unsafe and must not occur. The original parent process waits until `/healthz` returns 200 (polling up to 5s with 100ms sleep between attempts), then continues to step 2.
2. Call `POST /v1/sessions` with the given command, watch paths, and env overrides.
3. Print the session UUID to stdout and exit. The daemon continues running in the background.

If the daemon is already running (port 7777 responds to `/healthz`), step 1 is skipped and step 2 proceeds immediately. This means developers can run `korun serve -- cargo run` repeatedly and always get a new session against the same running daemon.

All other subcommands (`ls`, `inspect`, `restart`, `stop`, `tail`, `head`) are thin HTTP clients that call the running daemon. They do not start the daemon.

---

## Module Structure

```
src/
  main.rs              — entry point, clap dispatch
  cli/
    mod.rs             — CLI subcommand definitions (clap derive)
    serve.rs           — starts daemon, sends POST /v1/sessions
    client.rs          — shared reqwest HTTP client helpers
  daemon/
    mod.rs             — daemon startup: binds axum, spawns session manager
    router.rs          — axum route table
    handlers.rs        — HTTP handler functions
    session.rs         — Session struct, state machine, metadata fields
    session_mgr.rs     — SessionManager: Arc<RwLock<HashMap<Uuid, SessionState>>>
    process.rs         — child process spawn, stdout/stderr reader tasks
    buffer.rs          — LogBuffer: rolling VecDeque with seq + timestamp
    watcher.rs         — notify watcher, debounce, restart trigger channel
    error.rs           — AppError → axum IntoResponse
```

---

## Concurrency Model

The daemon runs on tokio's multi-thread runtime. Each session owns:

- **Supervisor task** (`tokio::spawn`) — owns the child process lifecycle: spawn, wait for exit, handle Stop/Restart commands, apply state transitions.
- **Two reader tasks** (`tokio::spawn`) — one each for stdout and stderr pipes, feed parsed lines into the session's `LogBuffer`. Child process stdin is set to `Stdio::null()` (redirected to `/dev/null`). This is functionally equivalent to "inherited from nowhere" per the spec — the child immediately reads EOF. True fd closing is not used because `tokio::process::Command` does not expose that directly; `/dev/null` achieves the same observable behavior.
- **Watcher task** (`tokio::spawn`) — receives file-change events from `notify-debouncer-mini` (configured with a 250ms debounce window). The debouncer resets its timer on each incoming event within the window, so rapid successive changes are collapsed into one. After the debounce window expires without further events, the watcher task sends one `Restart` command to the supervisor.

### Inter-task Communication

- `SessionManager` holds `Arc<RwLock<HashMap<Uuid, SessionState>>>` — read-heavy (list/inspect), written only on state transitions.
- Each session has a `tokio::sync::mpsc::Sender<SessionCommand>` (variants: `Stop`, `Restart`) shared with HTTP handlers and the watcher task. The supervisor task owns the receiver.
- `LogBuffer` is behind `Arc<Mutex<LogBuffer>>` — appended by reader tasks, read by HTTP handlers.
- Each session has a `tokio::sync::broadcast::Sender<LogEntry>` (capacity 1024) for live log following. Reader tasks publish into it; `/logs?follow=1` and `/tail?follow=1` handlers subscribe. When a slow consumer falls behind and receives `RecvError::Lagged(n)`, the handler injects a sentinel entry. **Extension beyond spec:** `Stream` gains a third variant `System` (serialized as `"system"`) used only for these sentinel entries. Clients should treat `stream: "system"` as a diagnostic message, not process output. In text format the sentinel renders as `[korun: dropped N lines]`. The connection is not closed; streaming resumes from the current broadcast head.

---

## Session State Machine

```
         start()
            │
        ┌───▼────┐
        │starting│◄──────────────────┐
        └───┬────┘                   │
            │ process spawned        │ restart: new child
        ┌───▼────┐                   │
        │running │                   │
        └───┬────┘                   │
            │ Stop/Restart command   │
        ┌───▼────┐                   │
        │stopping├───────────────────┘  (on Restart, loops back to starting)
        └───┬────┘
            │ Stop only: grace period elapsed or process dead
        ┌───▼────┐
        │ exited │  (child exited on its own, or Stop completed)
        └────────┘

        Any state → failed  (spawn error or unexpected internal error)
```

**`failed` state fields:** When a session enters `failed`, `pid` is cleared (set to null), `exit_code` and `term_signal` are populated if the failure was due to a process exit. `state` is `"failed"` in all API responses. The session remains visible in the session list.

**State transitions are the supervisor task's exclusive responsibility.** HTTP handlers and the watcher send commands via `mpsc`; the supervisor applies transitions and updates `SessionState` in the shared map.

### Restart Sequence

1. Supervisor receives `Restart` command. **Immediately increments `restart_count` and the appropriate sub-counter** (`manual_restart_count` or `watch_restart_count`) so the updated count is visible during the stopping/starting transition.
2. Transitions to `stopping`, sends `SIGTERM` to child.
3. Waits up to 2s grace period (fixed in v1; no per-session configuration per spec).
4. If child still alive, sends `SIGKILL`.
5. Transitions to `starting`, spawns new child, transitions to `running`.

---

## Log Buffer

```rust
enum Stream {
    Stdout,
    Stderr,
    System, // extension: used only for sentinel/diagnostic entries (e.g., lag notices)
}

struct LogEntry {
    seq: u64,           // monotonically increasing per session
    ts: DateTime<Utc>,  // RFC 3339
    stream: Stream,     // Stdout | Stderr | System
    line: String,
}

struct LogBuffer {
    entries: VecDeque<LogEntry>,
    capacity: usize,
    next_seq: u64,
    dropped: u64,       // lines evicted when buffer was full
    total_bytes: u64,   // bytes read from this stream; always 0 and unused on blended buffer
}
```

**Note on `total_bytes` in the blended buffer:** This is a known dead field. Using a single struct for all three buffers is simpler than a separate type. The field is always zero on the blended buffer and must not be exposed in the API response for that buffer. Per-stream byte counts are read from the stdout and stderr buffers respectively.

### Key Operations

- `push(entry)` — appends; if at capacity, pops from front and increments `dropped`.
- `since_seq(n, limit)` — entries with `seq >= n`, up to `limit`. If `since_seq` is omitted in an HTTP request, it defaults to returning the last `limit` entries (equivalent to `tail(limit)`).
- `tail(limit)` — last N entries in the buffer.
- `head(limit)` — first N entries currently in the buffer.

### Partial Lines

Reader tasks maintain a `String` scratch buffer. Lines are flushed on `\n`. On process exit, any remaining partial line is flushed as a final entry.

### Blended Buffer

A single `Arc<Mutex<LogBuffer>>` shared between both reader tasks. Both append in arrival order, providing a naturally interleaved cross-stream view. The `total_bytes` field on the blended `LogBuffer` is always zero and unused — byte counting is tracked only on the per-stream (stdout, stderr) buffers to match the spec's "Total bytes read from stdout/stderr" stats. The blended buffer exists solely for ordered log retrieval.

### Buffer Capacities (v1 defaults)

| Buffer | Capacity |
|---|---|
| stdout | 10,000 lines |
| stderr | 10,000 lines |
| blended | 20,000 lines |

---

## Session List Response Fields

`GET /v1/sessions` returns a summary object per session. The per-session fields are:

```json
{
  "id": "...",
  "state": "running",
  "command": ["cargo", "run"],
  "cwd": "/tmp/app",
  "pid": 43122,
  "started_at": "2026-03-16T12:00:00Z",
  "restart_count": 3
}
```

Full metadata (all fields) is only returned by `GET /v1/sessions/{id}`.

---

## Head and Tail Response Format

`GET /v1/sessions/{id}/head` and `GET /v1/sessions/{id}/tail` return the same envelope as `/logs` (non-follow mode):

```json
{
  "session_id": "...",
  "stream": "blended",
  "entries": [...],
  "next_seq": 103
}
```

They support the same `stream`, `limit`, and `format` query parameters. `/tail` additionally supports `follow=0|1` and `since_seq=<n>`. `/head` does not support `follow`.

---

## Invalid State Transitions (HTTP 409)

The following operations return HTTP 409:

| Operation | Invalid when session state is | Reason |
|---|---|---|
| `POST .../stop` | `exited`, `failed` | Session already in terminal state |

All other operations:

- `POST .../restart` is valid from any state including `exited` and `failed` (the product spec lists no restriction). From `exited` or `failed`, the supervisor spawns a new child and transitions back through `starting → running`.
- `POST .../restart` while already in `stopping` (mid-restart): the incoming `Restart` command is **dropped silently**. The in-progress stop/restart sequence will complete and transition the session to `running` on its own. The HTTP handler returns the same `{ "ok": true, "state": "stopping" }` response without queuing a second restart.
- `POST .../stop` while in `stopping`: idempotent — returns 200 with current state, no second signal sent.

---

## `since_seq` Default (HTTP Handler Behavior)

- `/logs` and `/tail`: When `since_seq` is omitted, the handler returns the last `limit` entries (tail behavior). This is a deliberate interpretation of the product spec, which leaves omission behavior undefined.
- `/head`: Does **not** support `since_seq`. `/head` is defined as returning the oldest currently-buffered entries (from the front of the `VecDeque`). Adding `since_seq` would conflict with its semantics. `since_seq` is silently ignored if provided.

---

## Session Metadata Response Fields

The `GET /v1/sessions/{id}` response includes all fields from the product spec. Clarifications:

- `started_at` — the time the session was first created (session creation timestamp, immutable).
- `last_started_at` — the time the most recent child process was spawned (updated on every restart). On the first start, equals `started_at`.
- `stdout_bytes` — total bytes read from the child's stdout (from `LogBuffer.total_bytes` on the stdout buffer).
- `stderr_bytes` — total bytes read from the child's stderr (from `LogBuffer.total_bytes` on the stderr buffer).

The blended buffer's `total_bytes` is not exposed in the API.

---

## HTTP API Summary

| Method | Path | Purpose |
|---|---|---|
| GET | `/healthz` | Daemon health check |
| GET | `/v1/sessions` | List all sessions |
| POST | `/v1/sessions` | Create and start a session |
| GET | `/v1/sessions/{id}` | Full session metadata |
| POST | `/v1/sessions/{id}/restart` | Restart a session |
| POST | `/v1/sessions/{id}/stop` | Stop a session |
| GET | `/v1/sessions/{id}/logs` | Fetch logs (json/text, follow + since_seq support) |
| GET | `/v1/sessions/{id}/head` | Oldest buffered lines |
| GET | `/v1/sessions/{id}/tail` | Newest buffered lines (follow + since_seq support) |

All error responses follow:

```json
{ "error": { "code": "not_found", "message": "session not found" } }
```

---

## CLI Subcommands

| Command | HTTP call |
|---|---|
| `korun serve [--watch <path>]... -- <cmd>` | Starts daemon + `POST /v1/sessions` |
| `korun ls` | `GET /v1/sessions` |
| `korun inspect <UUID>` | `GET /v1/sessions/{id}` |
| `korun restart <UUID>` | `POST /v1/sessions/{id}/restart` |
| `korun stop <UUID>` | `POST /v1/sessions/{id}/stop` |
| `korun tail [-f] <UUID>` | `GET /v1/sessions/{id}/tail` |
| `korun head <UUID>` | `GET /v1/sessions/{id}/head` |

---

## Error Handling

- HTTP 400 — invalid input
- HTTP 404 — unknown session ID
- HTTP 409 — invalid state transition
- HTTP 500 — unexpected internal failure

All errors serialize to the standard JSON error envelope.

---

## Session Cleanup (v1)

There is no `DELETE /v1/sessions/{id}` endpoint in v1. Sessions accumulate in memory for the lifetime of the daemon. This is intentional — the session list is expected to be small in a local developer workflow and sessions are useful for post-mortem inspection after exit. Session cleanup is deferred to a future version.

---

## Log Streaming Wire Format (`follow=1`)

For `follow=1` (on both `/logs` and `/tail`), the connection stays open and new entries are streamed as they arrive using chunked HTTP transfer encoding.

- **`format=text`**: each new line is sent as a plain text chunk, newline-terminated. No framing.
- **`format=json`**: each new entry is sent as a newline-delimited JSON object (NDJSON). Each chunk is a complete, self-contained JSON object followed by `\n`. Example:
  ```
  {"seq":103,"ts":"2026-03-16T12:01:02Z","stream":"stdout","line":"hello"}\n
  {"seq":104,"ts":"2026-03-16T12:01:03Z","stream":"stderr","line":"warn"}\n
  ```
  There is no wrapping envelope object in follow mode — each line is a standalone `LogEntry`.

## `/tail` and `/logs` Reconnect Semantics

Both `/logs` and `/tail` accept `since_seq=<n>`. When a client reconnects after a disconnect, it passes the last `seq` it received to resume without replaying already-seen entries. If `since_seq` is omitted, the endpoint returns the last `limit` buffered entries and, if `follow=1`, streams new ones from that point.

**Note:** `since_seq` on `/tail` is an extension beyond the product spec, added to support reconnect use cases for the `korun tail -f` CLI command.

---

## Release Distribution (cargo-dist)

KORun uses [cargo-dist](https://github.com/axodotdev/cargo-dist) to build and publish cross-platform binary releases via GitHub Releases.

### Supported targets (v1)

| Target | Platform |
|---|---|
| `x86_64-unknown-linux-gnu` | Linux x86-64 |
| `aarch64-unknown-linux-gnu` | Linux ARM64 |
| `x86_64-apple-darwin` | macOS Intel |
| `aarch64-apple-darwin` | macOS Apple Silicon |
| `x86_64-pc-windows-msvc` | Windows x86-64 |

### Setup

`cargo dist init` is run once during project setup. It adds a `[workspace.metadata.dist]` section to `Cargo.toml` and generates `.github/workflows/release.yml`. Configuration:

```toml
[workspace.metadata.dist]
cargo-dist-version = "0.x"
ci = ["github"]
installers = ["shell", "powershell"]
targets = [
  "x86_64-unknown-linux-gnu",
  "aarch64-unknown-linux-gnu",
  "x86_64-apple-darwin",
  "aarch64-apple-darwin",
  "x86_64-pc-windows-msvc",
]
```

### Release trigger

Releases are triggered by pushing a git tag matching `v*` (e.g., `v0.1.0`). cargo-dist's generated `release.yml` workflow handles building, packaging, and uploading artifacts to the GitHub Release.

Install script for users:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/<owner>/korun/releases/latest/download/korun-installer.sh | sh
```

---

## GitHub CI

Two workflow files under `.github/workflows/`:

### `ci.yml` — runs on push to `main` and all PRs

Steps:
1. `cargo fmt --check` — formatting gate
2. `cargo clippy -- -D warnings` — lint gate
3. `cargo test` — unit and integration tests
4. Build matrix: `ubuntu-latest`, `macos-latest`, `windows-latest` — ensures the crate compiles on all tier-1 platforms

```yaml
on:
  push:
    branches: [main]
  pull_request:
```

### `release.yml` — generated by cargo-dist

Triggered by `v*` tags. Builds release binaries for all configured targets and publishes them to GitHub Releases. This file is managed by `cargo dist` and should not be hand-edited.

---

## Non-Goals for v1

- PTY support
- Authentication
- Persistence across daemon restarts
- Remote networking
- Web UI
- Automatic crash restart (no crash-loop restart policy)
- Session deletion / list cleanup
