mod dashboard;
mod event;
mod server;
mod watcher;

use clap::Parser;
use std::path::PathBuf;
use tokio::sync::broadcast;
use tracing::info;

/// Claude Trace RS — local-first real-time observability for Claude Code sessions.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// Root directory to watch for Claude Code JSONL session files.
    #[arg(
        short = 'w',
        long,
        env = "CLAUDE_TRACE_WATCH_ROOT",
        default_value = "~/.claude/projects"
    )]
    watch_root: String,

    /// TCP port to bind the HTTP and WebSocket server to.
    #[arg(short, long, env = "CLAUDE_TRACE_PORT", default_value_t = 7779)]
    port: u16,

    /// Broadcast channel capacity (number of events buffered per subscriber).
    #[arg(long, default_value_t = 1024)]
    channel_capacity: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "claude_trace_rs=info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();

    // Validate broadcast channel capacity before use — broadcast::channel panics on 0.
    anyhow::ensure!(
        cli.channel_capacity > 0,
        "--channel-capacity must be at least 1"
    );

    // Expand `~` in the watch root path.
    let raw_root = cli.watch_root.clone();
    let watch_root: PathBuf = if raw_root.starts_with("~/") || raw_root == "~" {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_owned());
        // strip_prefix returns "" for bare "~", which join() ignores correctly.
        let rest = raw_root.strip_prefix("~/").unwrap_or("");
        if rest.is_empty() {
            PathBuf::from(home)
        } else {
            PathBuf::from(home).join(rest)
        }
    } else {
        PathBuf::from(&raw_root)
    };

    // Create the watch root if it doesn't exist so the watcher can start cleanly.
    if !watch_root.exists() {
        info!(
            "Watch root {} does not exist; creating it",
            watch_root.display()
        );
        std::fs::create_dir_all(&watch_root)?;
    }

    let (tx, _) = broadcast::channel::<event::TraceEvent>(cli.channel_capacity);

    let server_state = server::AppState {
        tx: tx.clone(),
        watch_root: watch_root.to_string_lossy().to_string(),
        port: cli.port,
    };

    // Spawn the session watcher on a dedicated blocking thread because notify
    // uses a synchronous callback internally.
    let watcher_tx = tx.clone();
    let watcher_root = watch_root.clone();
    std::thread::spawn(move || {
        let watcher = watcher::SessionWatcher::new(watcher_root, watcher_tx);
        if let Err(e) = watcher.run() {
            tracing::error!("SessionWatcher exited with error: {e}");
        }
    });

    server::serve(server_state).await?;
    Ok(())
}
