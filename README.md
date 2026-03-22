# korun

This is a process runner.
It's a local developer process runner built for the era of AI-assisted coding.
It's like 'air' for Golang, but you can configure it in more ways.
You do it via REST API.
It's made for the era of agents, where I can have LLM call the tools for more visibility.
korun gives you a way to peak at the process behavior.

korun runs your dev processes, watches files, and exposes everything — state, logs, restarts — over a simple HTTP API. It's designed so that both you and your AI coding assistant can control running processes without a TTY, without a GUI, and without parsing terminal output.

When your AI tool needs to restart a server, tail logs after a code change, or check whether a build is still running, it calls `curl`. korun answers.

## Install

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/wkoszek/korun/releases/latest/download/korun-installer.sh | sh
```

Or with Cargo:

```sh
cargo install korun
```

## Quick start

Start a session and watch for file changes:

```sh
korun serve --watch src --watch Cargo.toml -- cargo run
# prints a session UUID
```

The daemon starts automatically on first use and keeps running in the background. Subsequent `korun serve` calls add new sessions to the same daemon.

## Commands

| Command | Description |
|---------|-------------|
| `korun serve [--watch <path>]... -- <cmd>` | Start a session (auto-starts daemon) |
| `korun ls` | List all sessions |
| `korun inspect <UUID>` | Full session metadata |
| `korun restart <UUID>` | Restart a session |
| `korun stop <UUID>` | Stop a session |
| `korun tail [-f] <UUID>` | Tail session logs |
| `korun head <UUID>` | Head (oldest buffered) logs |

## HTTP API

The daemon binds to `127.0.0.1:7777`. Every operation is a plain HTTP call — no special client required. AI tools, shell scripts, and humans all use the same interface.

```sh
# Health check
curl http://127.0.0.1:7777/healthz

# List sessions
curl http://127.0.0.1:7777/v1/sessions

# Inspect a session
curl http://127.0.0.1:7777/v1/sessions/<UUID>

# Tail logs (last 100 lines)
curl "http://127.0.0.1:7777/v1/sessions/<UUID>/logs"

# Stream logs live
curl -N "http://127.0.0.1:7777/v1/sessions/<UUID>/logs?follow=1&format=text"

# Stderr only, last 20 lines
curl "http://127.0.0.1:7777/v1/sessions/<UUID>/logs?stream=stderr&limit=20"

# Restart
curl -X POST http://127.0.0.1:7777/v1/sessions/<UUID>/restart

# Stop
curl -X POST http://127.0.0.1:7777/v1/sessions/<UUID>/stop
```

### Log query parameters

| Parameter | Values | Default |
|-----------|--------|---------|
| `stream` | `stdout`, `stderr`, `blended` | `blended` |
| `limit` | integer | `100` |
| `since_seq` | integer | — |
| `follow` | `0`, `1` | `0` |
| `format` | `json`, `text` | `json` |

Use `since_seq` to resume after a disconnect without replaying already-seen lines — useful when an AI tool polls logs incrementally.

## How it works

- Runs as a local daemon on `127.0.0.1:7777`
- Each session manages one child process with captured stdout/stderr
- Rolling line buffers: 10,000 lines stdout, 10,000 stderr, 20,000 blended
- File watching with 250ms debounce — rapid saves collapse into one restart
- Graceful shutdown: SIGTERM → 2s grace → SIGKILL
- Sessions persist after the child exits for post-mortem inspection

## Why HTTP?

Most process supervisors expose a TUI, a proprietary socket protocol, or nothing at all. That works fine when a human is watching. It breaks down when an AI coding assistant needs to:

- Check whether the dev server is still up after editing a file
- Read the last 50 lines of stderr to understand a crash
- Trigger a restart after applying a patch
- Confirm the process came back healthy before continuing

korun's HTTP API was designed with this use case in mind. Every response is JSON. Every log endpoint supports plain text. Everything works with `curl` from a shell tool call.

## Non-goals for v1

- Remote access or authentication
- PTY / interactive terminal
- Persistent sessions across daemon restarts
- Automatic crash-loop restart
- Web UI

## License

MIT OR Apache-2.0
