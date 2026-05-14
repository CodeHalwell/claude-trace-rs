mod dashboard;
mod event;
mod server;
mod state;
mod watcher;

use clap::Parser;
use std::path::PathBuf;
use tokio::sync::broadcast;
use tracing::{info, warn};

/// Claude Trace — local-first real-time observability for Claude Code sessions.
///
/// Watches one or more directories of Claude Code JSONL session logs, parses
/// new events as they appear, and surfaces per-session insights through a
/// built-in dashboard at http://127.0.0.1:<port>/.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
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

    /// Replay every event already on disk into the in-memory store at startup.
    /// Without this flag, the watcher starts at EOF so only newly produced
    /// events stream into the dashboard.
    #[arg(long, env = "CLAUDE_TRACE_BACKFILL")]
    backfill: bool,

    /// Open the dashboard URL in the default browser once the server is up.
    #[arg(long, env = "CLAUDE_TRACE_OPEN")]
    open: bool,
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

    anyhow::ensure!(
        cli.channel_capacity > 0,
        "--channel-capacity must be at least 1"
    );

    let watch_root = expand_tilde(&cli.watch_root);

    if !watch_root.exists() {
        info!(
            "Watch root {} does not exist; creating it",
            watch_root.display()
        );
        std::fs::create_dir_all(&watch_root)?;
    }

    let (tx, _) = broadcast::channel::<event::TraceEvent>(cli.channel_capacity);
    let store = state::SessionStore::new();

    let server_state = server::AppState {
        tx: tx.clone(),
        watch_root: watch_root.to_string_lossy().to_string(),
        port: cli.port,
        store: store.clone(),
    };

    // Spawn the session watcher on a dedicated blocking thread because notify
    // uses a synchronous callback internally.
    let watcher_tx = tx.clone();
    let watcher_root = watch_root.clone();
    let watcher_store = store.clone();
    let opts = watcher::WatcherOptions {
        backfill: cli.backfill,
    };
    std::thread::spawn(move || {
        let watcher =
            watcher::SessionWatcher::new(watcher_root, watcher_tx, watcher_store, opts);
        if let Err(e) = watcher.run() {
            tracing::error!("SessionWatcher exited with error: {e}");
        }
    });

    // Optionally fire up a browser tab once the server is bound.  We do this
    // on a tiny delay so the listener has time to start accepting connections.
    if cli.open {
        let url = format!("http://127.0.0.1:{}/", cli.port);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            if let Err(e) = open_in_browser(&url) {
                warn!("Could not open browser ({url}): {e}");
            }
        });
    }

    server::serve(server_state).await?;
    Ok(())
}

/// Expand a leading `~/` or bare `~` in a path string to the user's home.
fn expand_tilde(raw: &str) -> PathBuf {
    if raw == "~" || raw.starts_with("~/") {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_owned());
        let rest = raw.strip_prefix("~/").unwrap_or("");
        if rest.is_empty() {
            PathBuf::from(home)
        } else {
            PathBuf::from(home).join(rest)
        }
    } else {
        PathBuf::from(raw)
    }
}

/// Best-effort cross-platform "open this URL in the default browser".
fn open_in_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let cmd = ("open", vec![url]);
    #[cfg(target_os = "windows")]
    let cmd = ("cmd", vec!["/C", "start", "", url]);
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = ("xdg-open", vec![url]);

    std::process::Command::new(cmd.0)
        .args(cmd.1)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::expand_tilde;

    #[test]
    fn tilde_expansion() {
        std::env::set_var("HOME", "/home/test");
        assert_eq!(
            expand_tilde("~/.claude/projects"),
            std::path::PathBuf::from("/home/test/.claude/projects")
        );
        assert_eq!(expand_tilde("~"), std::path::PathBuf::from("/home/test"));
        assert_eq!(
            expand_tilde("/abs/path"),
            std::path::PathBuf::from("/abs/path")
        );
        assert_eq!(
            expand_tilde("rel/path"),
            std::path::PathBuf::from("rel/path")
        );
    }
}
