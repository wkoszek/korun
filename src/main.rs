use clap::{Parser, Subcommand};

mod cli;
mod daemon;

#[derive(Parser)]
#[command(
    name = "korun",
    about = "Local developer process runner",
    after_help = "\
EXAMPLES:
  korun serve -- cargo run              # run in foreground, stream logs
  korun serve -w src -- cargo run       # restart on src/ changes
  korun serve -W watch.txt -- npm start # watch paths from file
  korun daemon -- cargo run             # background; prints session UUID
  korun ls                              # list sessions
  korun tail -f                         # follow logs (auto UUID if 1 session)
  korun tail -f <UUID>                  # follow logs for specific session
  korun restart                         # restart (auto UUID if 1 session)
  korun stop --all                      # stop all sessions
  korun inspect                         # full session metadata"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start a new session (auto-starts daemon if needed)
    Serve {
        #[arg(long = "watch", short = 'w')]
        watch: Vec<String>,
        /// Read watch paths from a file (one path per line)
        #[arg(long = "watch-file", short = 'W')]
        watch_file: Option<String>,
        #[arg(long = "env", short = 'e')]
        env: Vec<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// List all sessions
    Ls,
    /// Inspect a session (omit UUID if only one session is active)
    Inspect { id: Option<String> },
    /// Restart a session (omit UUID if only one session is active)
    Restart { id: Option<String> },
    /// Stop a session (omit UUID if only one session is active, or use --all)
    Stop {
        id: Option<String>,
        /// Stop all active sessions
        #[arg(long, conflicts_with = "id")]
        all: bool,
    },
    /// Stream tail of session logs (omit UUID if only one session is active)
    Tail {
        id: Option<String>,
        #[arg(short = 'f', long)]
        follow: bool,
    },
    /// Show head of session logs (omit UUID if only one session is active)
    Head { id: Option<String> },
    /// Start a session with the daemon backgrounded (prints session UUID and exits)
    Daemon {
        #[arg(long = "watch", short = 'w')]
        watch: Vec<String>,
        /// Read watch paths from a file (one path per line)
        #[arg(long = "watch-file", short = 'W')]
        watch_file: Option<String>,
        #[arg(long = "env", short = 'e')]
        env: Vec<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
}

/// Entry point — plain fn main so we can fork BEFORE tokio threads start.
fn main() -> anyhow::Result<()> {
    // --daemon-internal: grandchild runs the daemon. Start runtime and serve.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--daemon-internal") {
        let rt = tokio::runtime::Runtime::new()?;
        return rt.block_on(async {
            tracing_subscriber::fmt::init();
            let addr: std::net::SocketAddr = "127.0.0.1:7777".parse().unwrap();
            daemon::run_daemon(addr).await
        });
    }

    let cli = Cli::parse();

    // For `daemon`: fork the daemon to background before starting tokio threads.
    if let Commands::Daemon {
        ref watch,
        ref watch_file,
        ref env,
        ref cwd,
        ref command,
    } = cli.command
    {
        if !is_daemon_running() {
            // Fork must happen HERE — before any tokio Runtime::new()
            daemonize_sync()?;
        }
        let (watch, watch_file, env, cwd, command) = (
            watch.clone(),
            watch_file.clone(),
            env.clone(),
            cwd.clone(),
            command.clone(),
        );
        let rt = tokio::runtime::Runtime::new()?;
        return rt.block_on(async {
            tracing_subscriber::fmt::init();
            let watch = cli::load_watch_paths(watch, watch_file)?;
            let id = cli::serve::serve_cmd(command, watch, env, cwd).await?;
            println!("{id}");
            Ok(())
        });
    }

    // All other subcommands: start the runtime.
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        tracing_subscriber::fmt::init();
        match cli.command {
            Commands::Serve { watch, watch_file, env, cwd, command } => {
                let addr: std::net::SocketAddr = "127.0.0.1:7777".parse().unwrap();
                if !is_daemon_running() {
                    tokio::spawn(daemon::run_daemon(addr));
                    cli::client::wait_for_daemon(cli::client::DEFAULT_ADDR).await?;
                }
                let watch = cli::load_watch_paths(watch, watch_file)?;
                let id = cli::serve::serve_cmd(command, watch, env, cwd).await?;
                eprintln!("session: {id}");
                cli::cmd_tail(Some(id), true).await?;
            }
            Commands::Ls => cli::cmd_ls().await?,
            Commands::Inspect { id } => cli::cmd_inspect(id).await?,
            Commands::Restart { id } => cli::cmd_restart(id).await?,
            Commands::Stop { id, all } => {
                if all {
                    cli::cmd_stop_all().await?;
                } else {
                    cli::cmd_stop(id).await?;
                }
            }
            Commands::Tail { id, follow } => cli::cmd_tail(id, follow).await?,
            Commands::Head { id } => cli::cmd_head(id).await?,
            Commands::Daemon { .. } => unreachable!("handled above"),
        }
        Ok::<(), anyhow::Error>(())
    })
}

/// Blocking check: returns true if daemon is already running.
fn is_daemon_running() -> bool {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()
        .ok()
        .and_then(|c| c.get("http://127.0.0.1:7777/healthz").send().ok())
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Fork the daemon. Returns in the parent after the daemon is ready.
/// The grandchild re-execs with --daemon-internal and exits.
fn daemonize_sync() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use nix::unistd::{fork, setsid, ForkResult};

        match unsafe { fork() }? {
            ForkResult::Parent { .. } => {
                // First parent: wait for daemon to be ready, then return
                wait_for_daemon_blocking()?;
                return Ok(());
            }
            ForkResult::Child => {}
        }

        // First child: fork again
        match unsafe { fork() }? {
            ForkResult::Parent { .. } => std::process::exit(0),
            ForkResult::Child => {}
        }

        // Grandchild: become daemon, re-exec with --daemon-internal
        setsid()?;
        use std::fs::OpenOptions;
        use std::os::unix::io::IntoRawFd;
        let devnull = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")?
            .into_raw_fd();
        unsafe {
            libc::dup2(devnull, 0);
            libc::dup2(devnull, 1);
            libc::dup2(devnull, 2);
            libc::close(devnull);
        }
        let exe = std::env::current_exe()?;
        std::process::Command::new(exe)
            .arg("--daemon-internal")
            .spawn()?;
        std::process::exit(0);
    }

    #[cfg(not(unix))]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        let exe = std::env::current_exe()?;
        std::process::Command::new(exe)
            .arg("--daemon-internal")
            .creation_flags(DETACHED_PROCESS)
            .spawn()?;
        wait_for_daemon_blocking()?;
        Ok(())
    }
}

fn wait_for_daemon_blocking() -> anyhow::Result<()> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()?;
    for _ in 0..50 {
        if client
            .get("http://127.0.0.1:7777/healthz")
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    anyhow::bail!("daemon did not start within 5 seconds")
}
