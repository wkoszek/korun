use clap::{Parser, Subcommand};

mod cli;
mod daemon;

#[derive(Parser)]
#[command(name = "korun", about = "Local developer process runner")]
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
        #[arg(long = "env", short = 'e')]
        env: Vec<String>,
        #[arg(long)]
        cwd: Option<String>,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// List all sessions
    Ls,
    /// Inspect a session
    Inspect { id: String },
    /// Restart a session
    Restart { id: String },
    /// Stop a session
    Stop { id: String },
    /// Stream tail of session logs
    Tail {
        id: String,
        #[arg(short = 'f', long)]
        follow: bool,
    },
    /// Show head of session logs
    Head { id: String },
    /// Run the daemon in the foreground (useful for systemd/launchd/Docker)
    Daemon,
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

    // For the `serve` subcommand, we may need to fork the daemon before starting
    // any tokio threads. We do this here, synchronously, before building the runtime.
    if let Commands::Serve {
        ref watch,
        ref env,
        ref cwd,
        ref command,
    } = cli.command
    {
        // Check if daemon is already running (blocking HTTP call)
        let daemon_running = is_daemon_running();
        if !daemon_running {
            // Fork must happen HERE — before any tokio Runtime::new()
            daemonize_sync()?;
        }
        // Now it's safe to start the async runtime for the CLI POST
        let (watch, env, cwd, command) = (watch.clone(), env.clone(), cwd.clone(), command.clone());
        let rt = tokio::runtime::Runtime::new()?;
        return rt.block_on(async {
            tracing_subscriber::fmt::init();
            cli::serve::serve_cmd(command, watch, env, cwd).await
        });
    }

    // All other subcommands: just start the runtime
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        tracing_subscriber::fmt::init();
        match cli.command {
            Commands::Ls => cli::cmd_ls().await?,
            Commands::Inspect { id } => cli::cmd_inspect(&id).await?,
            Commands::Restart { id } => cli::cmd_restart(&id).await?,
            Commands::Stop { id } => cli::cmd_stop(&id).await?,
            Commands::Tail { id, follow } => cli::cmd_tail(&id, follow).await?,
            Commands::Head { id } => cli::cmd_head(&id).await?,
            Commands::Daemon => {
                let addr: std::net::SocketAddr = "127.0.0.1:7777".parse().unwrap();
                daemon::run_daemon(addr).await?;
            }
            Commands::Serve { .. } => unreachable!("handled above"),
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
