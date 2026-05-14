mod dashboard;
mod event;
mod export;
mod loader;
mod server;
mod state;
mod watcher;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tokio::sync::broadcast;
use tracing::{info, warn};

/// Claude Trace — local-first real-time observability for Claude Code sessions.
///
/// Watches one or more directories of Claude Code JSONL session logs, parses
/// new events as they appear, and either serves a built-in browser dashboard
/// (`serve`, the default) or dumps them to disk in a training-friendly format
/// (`export`).
#[derive(Parser, Debug)]
#[command(version, about, long_about = None, arg_required_else_help = false)]
struct Cli {
    /// Root directory to watch / read Claude Code JSONL session files from.
    #[arg(
        short = 'w',
        long,
        env = "CLAUDE_TRACE_WATCH_ROOT",
        default_value = "~/.claude/projects",
        global = true
    )]
    watch_root: String,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the live dashboard server (default if no subcommand is given).
    Serve(ServeArgs),
    /// Export one or more sessions to disk in a training-friendly format.
    Export(ExportArgs),
    /// Print every session discovered on disk as JSON to stdout.
    List,
}

#[derive(clap::Args, Debug)]
struct ServeArgs {
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

impl Default for ServeArgs {
    fn default() -> Self {
        Self {
            port: 7779,
            channel_capacity: 1024,
            backfill: false,
            open: false,
        }
    }
}

#[derive(clap::Args, Debug)]
struct ExportArgs {
    /// Output format.
    #[arg(short = 'f', long, default_value = "messages")]
    format: export::ExportFormat,

    /// Output file path. Use `-` for stdout. For `--format huggingface` this
    /// is treated as a directory (created if missing).
    #[arg(short = 'o', long)]
    out: Option<String>,

    /// Optional list of session IDs to include. Omit to export every session.
    #[arg(long, value_delimiter = ',')]
    session: Vec<String>,

    /// Skip sessions whose event count is below this threshold.
    #[arg(long, default_value_t = 1)]
    min_events: usize,
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
    let watch_root = expand_tilde(&cli.watch_root);

    match cli.cmd.unwrap_or(Cmd::Serve(ServeArgs::default())) {
        Cmd::Serve(args) => run_serve(watch_root, args).await,
        Cmd::Export(args) => run_export(&watch_root, args),
        Cmd::List => run_list(&watch_root),
    }
}

async fn run_serve(watch_root: PathBuf, args: ServeArgs) -> anyhow::Result<()> {
    anyhow::ensure!(
        args.channel_capacity > 0,
        "--channel-capacity must be at least 1"
    );

    if !watch_root.exists() {
        info!(
            "Watch root {} does not exist; creating it",
            watch_root.display()
        );
        std::fs::create_dir_all(&watch_root)?;
    }

    let (tx, _) = broadcast::channel::<event::TraceEvent>(args.channel_capacity);
    let store = state::SessionStore::new();

    let server_state = server::AppState {
        tx: tx.clone(),
        watch_root: watch_root.to_string_lossy().to_string(),
        port: args.port,
        store: store.clone(),
    };

    let watcher_tx = tx.clone();
    let watcher_root = watch_root.clone();
    let watcher_store = store.clone();
    let opts = watcher::WatcherOptions {
        backfill: args.backfill,
    };
    std::thread::spawn(move || {
        let watcher =
            watcher::SessionWatcher::new(watcher_root, watcher_tx, watcher_store, opts);
        if let Err(e) = watcher.run() {
            tracing::error!("SessionWatcher exited with error: {e}");
        }
    });

    if args.open {
        let url = format!("http://127.0.0.1:{}/", args.port);
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

fn run_export(watch_root: &std::path::Path, args: ExportArgs) -> anyhow::Result<()> {
    use std::io::Write as _;

    let store = state::SessionStore::new();
    let n = loader::ingest_directory(watch_root, &store)?;
    info!("Loaded {} events across {} sessions", n, store.sessions().len());

    let want: std::collections::HashSet<String> = args.session.into_iter().collect();
    let sessions: Vec<_> = store
        .sessions()
        .into_iter()
        .filter(|s| s.event_count >= args.min_events)
        .filter(|s| want.is_empty() || want.contains(&s.id))
        .collect();

    anyhow::ensure!(!sessions.is_empty(), "No sessions matched the filter");

    // Build SessionExport vec — we need the events to outlive the borrow.
    let session_events: Vec<(state::SessionStats, Vec<event::TraceEvent>)> = sessions
        .into_iter()
        .map(|s| {
            let evs = store.session_events(&s.id);
            (s, evs)
        })
        .collect();
    let exports: Vec<export::SessionExport<'_>> = session_events
        .iter()
        .map(|(s, e)| export::SessionExport {
            stats: s,
            events: e.as_slice(),
        })
        .collect();

    if matches!(args.format, export::ExportFormat::Huggingface) {
        let out = args
            .out
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("--out <dir> is required for the huggingface format"))?;
        let dir = expand_tilde(out);
        export::write_huggingface_dir(&dir, &exports)?;
        println!("Wrote HuggingFace dataset to {}", dir.display());
        return Ok(());
    }

    let body = export::render_many(&exports, args.format);
    match args.out.as_deref() {
        None | Some("-") => {
            std::io::stdout().write_all(body.as_bytes())?;
        }
        Some(path) => {
            let path = expand_tilde(path);
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            std::fs::write(&path, body)?;
            println!(
                "Wrote {} session(s) to {}",
                exports.len(),
                path.display()
            );
        }
    }
    Ok(())
}

fn run_list(watch_root: &std::path::Path) -> anyhow::Result<()> {
    let store = state::SessionStore::new();
    loader::ingest_directory(watch_root, &store)?;
    let sessions = store.sessions();
    let out = serde_json::to_string_pretty(&sessions)?;
    println!("{out}");
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
