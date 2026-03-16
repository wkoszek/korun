# KORun

KORun is a local developer process runner.

Its job is to start application processes, watch selected files, restart the process when those files change, and expose process state and logs over a simple local HTTP API that works well with `curl`.

This is a tool for developers running apps on their own machine. It is not a production supervisor.

## Goals

- Run a command in the background and keep track of it as a session.
- Watch selected files or directories and restart the process when they change.
- Capture `stdout` and `stderr`.
- Keep recent output in memory in rolling line buffers.
- Expose process state, logs, and metadata over local HTTP.
- Keep the interface simple enough to inspect with `curl`.

## Non-goals for v1

- Remote access.
- Authentication or multi-user access control.
- PTY support.
- Persistent storage across KORun restarts.
- Full interactive terminal attachment.

## Core model

KORun runs as a local daemon bound to `127.0.0.1:7777` by default.

The daemon manages one or more sessions. Each session represents one developer command being supervised.

Example:

```text
$ korun serve --watch src --watch Cargo.toml -- cargo run
<UUID>
```

Each session has:

- A UUID.
- The command and arguments.
- The working directory.
- The environment overrides, if any.
- The current child PID, if running.
- Restart counters.
- File watch configuration.
- Rolling output buffers.
- Runtime metadata and health state.

## Process execution

For v1, KORun uses pipes, not a PTY.

The child process is started with:

- `stdin`: inherited from nowhere by default, effectively closed unless later extended.
- `stdout`: captured by KORun.
- `stderr`: captured by KORun.

KORun reads `stdout` and `stderr` concurrently.

## Output buffering

Buffers are line-based, not byte-based.

Each session stores:

- A rolling `stdout` line buffer.
- A rolling `stderr` line buffer.
- A rolling blended buffer that preserves cross-stream event order as observed by KORun.

Each stored log entry contains:

- A monotonically increasing sequence number.
- Timestamp in RFC 3339 format.
- Stream: `stdout` or `stderr`.
- The line text.

Default buffer sizes for v1:

- `stdout`: 10_000 lines
- `stderr`: 10_000 lines
- `blended`: 20_000 lines

When a buffer is full, the oldest lines are removed.

Partial trailing lines may be buffered internally until a newline arrives or the process exits. At process exit, a remaining partial line should be flushed as one final entry.

## File watching and reload behavior

KORun can watch files and directories attached to a session.

Supported watch targets:

- Individual files.
- Directories, recursively.

The CLI should allow repeated `--watch` flags.

Example:

```text
$ korun serve --watch src --watch Cargo.toml -- cargo run
```

Reload behavior:

1. A watched file changes.
2. KORun records a watch event in session metadata.
3. KORun restarts the process.

Restart means:

1. Send graceful termination first.
2. Wait a short configurable grace period.
3. If the process is still alive, kill it.
4. Start a new child for the same session.

For v1, the grace period can default to 2 seconds.

For v1, this is the only restart strategy. There is no per-session restart mode or signal configuration.

The spec should treat rapid repeated file changes as one restart opportunity using debounce. For v1, use a default debounce window of 250 ms.

Session metadata should record:

- Last file-change timestamp.
- Last changed path.
- Total file-change events observed.
- Total restarts triggered by file changes.

## Session lifecycle

Session states:

- `starting`
- `running`
- `stopping`
- `exited`
- `failed`

Session operations:

- Start a new session.
- List sessions.
- Inspect one session.
- Stop a session.
- Restart a session.

Exit behavior:

- If the child exits on its own, the session remains visible.
- Metadata must include exit code or terminating signal.
- For v1, automatic restart on crash is disabled unless the restart was explicitly triggered by the user or by a watched-file change.

## Health

KORun exposes a health endpoint so developers can quickly tell whether the daemon is alive.

This is only about KORun itself, not whether any given child process is healthy.

Suggested endpoint:

```text
GET /healthz
```

Response:

```json
{
  "ok": true,
  "service": "korun",
  "time": "2026-03-16T12:00:00Z"
}
```

## HTTP API

The HTTP API is local-only and JSON-first, except for endpoints that intentionally return plain text logs.

Bind address for v1:

```text
127.0.0.1:7777
```

All endpoints should be usable with `curl`.

### `GET /healthz`

Purpose:

- Check whether the daemon is running.

Response:

- `200 OK`
- JSON body as shown above.

### `GET /v1/sessions`

Purpose:

- List all sessions.

Example:

```text
$ curl -s http://127.0.0.1:7777/v1/sessions
```

Response:

```json
{
  "sessions": [
    {
      "id": "9b2d6c33-8b13-4c51-8cd0-8de7d8f4ef01",
      "state": "running",
      "command": ["cargo", "run"],
      "cwd": "/tmp/app",
      "pid": 43122,
      "started_at": "2026-03-16T12:00:00Z",
      "restart_count": 3
    }
  ]
}
```

### `POST /v1/sessions`

Purpose:

- Create and start a session.

This endpoint is the HTTP equivalent of `korun serve`.

Path handling for v1:

- `cwd` is the working directory for the session.
- If omitted by the CLI, `cwd` defaults to the caller's current directory.
- Relative entries in `watch` are resolved against `cwd`.
- Absolute entries in `watch` are used as-is.

Request:

```json
{
  "command": ["cargo", "run"],
  "cwd": "/tmp/app",
  "watch": ["src", "Cargo.toml"],
  "env": {
    "RUST_LOG": "debug"
  }
}
```

Response:

- `201 Created`

```json
{
  "id": "9b2d6c33-8b13-4c51-8cd0-8de7d8f4ef01",
  "state": "starting"
}
```

### `GET /v1/sessions/{id}`

Purpose:

- Return full session metadata.

Response shape:

```json
{
  "id": "9b2d6c33-8b13-4c51-8cd0-8de7d8f4ef01",
  "state": "running",
  "command": ["cargo", "run"],
  "cwd": "/tmp/app",
  "env_overrides": {
    "RUST_LOG": "debug"
  },
  "watch": ["src", "Cargo.toml"],
  "pid": 43122,
  "started_at": "2026-03-16T12:00:00Z",
  "last_started_at": "2026-03-16T12:03:11Z",
  "last_stopped_at": "2026-03-16T12:03:10Z",
  "uptime_ms": 81000,
  "restart_count": 3,
  "manual_restart_count": 1,
  "watch_restart_count": 2,
  "file_change_count": 7,
  "last_change_at": "2026-03-16T12:03:09Z",
  "last_change_path": "src/main.rs",
  "exit_code": null,
  "term_signal": null,
  "stdout_lines": 128,
  "stderr_lines": 2,
  "blended_lines": 130,
  "stdout_dropped_lines": 0,
  "stderr_dropped_lines": 0,
  "blended_dropped_lines": 0
}
```

The session metadata endpoint should be the main place where all stats are exposed.

### `POST /v1/sessions/{id}/restart`

Purpose:

- Restart the session now.

Request body:

- Empty body allowed.

Response:

```json
{
  "ok": true,
  "id": "9b2d6c33-8b13-4c51-8cd0-8de7d8f4ef01",
  "state": "starting"
}
```

### `POST /v1/sessions/{id}/stop`

Purpose:

- Stop the session and leave it registered for inspection.

Response:

```json
{
  "ok": true,
  "id": "9b2d6c33-8b13-4c51-8cd0-8de7d8f4ef01",
  "state": "stopping"
}
```

### `GET /v1/sessions/{id}/logs`

Purpose:

- Fetch buffered logs.

Query parameters:

- `stream=stdout|stderr|blended`
- `limit=<n>`
- `since_seq=<n>`
- `follow=0|1`
- `format=json|text`

Defaults:

- `stream=blended`
- `limit=100`
- `follow=0`
- `format=json`

Examples:

```text
$ curl -s "http://127.0.0.1:7777/v1/sessions/<id>/logs"
$ curl -s "http://127.0.0.1:7777/v1/sessions/<id>/logs?stream=stderr&limit=20"
$ curl -N "http://127.0.0.1:7777/v1/sessions/<id>/logs?follow=1&format=text"
```

For `format=json` and `follow=0`, response:

```json
{
  "session_id": "9b2d6c33-8b13-4c51-8cd0-8de7d8f4ef01",
  "stream": "blended",
  "entries": [
    {
      "seq": 101,
      "ts": "2026-03-16T12:01:00Z",
      "stream": "stdout",
      "line": "server started on :3000"
    },
    {
      "seq": 102,
      "ts": "2026-03-16T12:01:01Z",
      "stream": "stderr",
      "line": "warning: config file missing"
    }
  ],
  "next_seq": 103
}
```

For `format=text`, output should be plain text lines. For blended output, prefix each line with the stream name:

```text
[stdout] server started on :3000
[stderr] warning: config file missing
```

For `follow=1`, the connection remains open and new lines are streamed as they arrive. This may be implemented as chunked HTTP streaming.

### `GET /v1/sessions/{id}/head`

Purpose:

- Return the oldest buffered lines in a given stream.

Query parameters:

- `stream=stdout|stderr|blended`
- `limit=<n>`
- `format=json|text`

This endpoint exists because the CLI concept already includes `head`.

### `GET /v1/sessions/{id}/tail`

Purpose:

- Return the newest buffered lines in a given stream.

Query parameters:

- `stream=stdout|stderr|blended`
- `limit=<n>`
- `follow=0|1`
- `format=json|text`

This endpoint exists because the CLI concept already includes `tail -f`.

In implementation, `/head` and `/tail` may reuse the same log reader internally.

## Error handling

All JSON error responses should be curl-friendly and machine-readable.

Suggested shape:

```json
{
  "error": {
    "code": "not_found",
    "message": "session not found"
  }
}
```

Suggested status codes:

- `400 Bad Request` for invalid input.
- `404 Not Found` for unknown session IDs.
- `409 Conflict` for invalid state transitions.
- `500 Internal Server Error` for unexpected failures.

## CLI mapping

The CLI can be a thin wrapper over HTTP.

Examples:

```text
$ korun serve --watch src --watch Cargo.toml -- cargo run
$ korun ls
$ korun inspect <UUID>
$ korun restart <UUID>
$ korun stop <UUID>
$ korun tail -f <UUID>
$ korun head <UUID>
```

Suggested behavior:

- `serve` creates a session and prints the UUID.
- `ls` reads `GET /v1/sessions`.
- `inspect` reads `GET /v1/sessions/{id}`.
- `restart` calls `POST /v1/sessions/{id}/restart`.
- `stop` calls `POST /v1/sessions/{id}/stop`.
- `tail` and `head` call the matching log endpoints.

## Metadata and stats

Every session metadata response should track at least:

- Session ID.
- State.
- Command and arguments.
- Working directory.
- Watched paths.
- Child PID.
- Start time.
- Last restart time.
- Last exit time.
- Uptime.
- Exit code or signal.
- Total restart count.
- Manual restart count.
- Watch-triggered restart count.
- File-change count.
- Last changed path.
- Per-stream line counts stored.
- Per-stream dropped-line counts.
- Total bytes read from `stdout`.
- Total bytes read from `stderr`.

Daemon-level metadata for future consideration:

- Total sessions created since daemon start.
- Currently running session count.
- Daemon start time.
- Daemon uptime.

## Initial implementation boundaries

v1 should support:

- Local daemon on `127.0.0.1:7777`
- Multiple sessions
- Pipes for `stdout` and `stderr`
- Line-based rolling buffers
- Separate and blended log views
- File watching with debounce
- Manual restart and stop
- Health endpoint
- Session metadata endpoint
- Curl-friendly JSON and text log endpoints

v1 should not require:

- PTY support
- Authentication
- Persistence
- Remote networking
- Web UI
