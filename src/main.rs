mod dashboard;
mod db;
mod event;
mod export;
mod loader;
mod server;
mod service;
mod sources;
mod state;
mod watcher;

use anyhow::Context;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tokio::sync::broadcast;
use tracing::{info, warn};

/// Agent Trace — local-first real-time observability for terminal coding
/// agents (Claude Code, Codex CLI, Copilot CLI, Kimi Code, Cline, Cursor).
///
/// Watches one or more directories of agent session logs, parses new events
/// as they appear, and either serves a built-in browser dashboard (`serve`,
/// the default) or dumps them to disk in a training-friendly format
/// (`export`).
#[derive(Parser, Debug)]
#[command(version, about, long_about = None, arg_required_else_help = false)]
struct Cli {
    /// Root directory to watch / read agent trace files from. Repeatable.
    /// When omitted (and --no-default-roots is not set), every known agent
    /// log directory that exists is watched.
    #[arg(
        short = 'w',
        long,
        env = "CLAUDE_TRACE_WATCH_ROOT",
        value_delimiter = ',',
        global = true
    )]
    watch_root: Vec<String>,

    /// Force the agent source for `--watch-root` directories
    /// (claude|codex|copilot|kimi|cline|cursor). Auto-detected when omitted.
    #[arg(long, env = "CLAUDE_TRACE_SOURCE", global = true)]
    source: Option<String>,

    /// Only trace these agent sources (comma-separated).
    #[arg(long, value_delimiter = ',', global = true)]
    only: Option<Vec<String>>,

    /// Do not auto-add known agent log directories as watch roots.
    #[arg(long, global = true)]
    no_default_roots: bool,

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
    /// Install/manage a background service so the dashboard starts with your OS.
    Service(ServiceArgs),
}

#[derive(clap::Args, Debug)]
struct ServiceArgs {
    #[command(subcommand)]
    action: ServiceAction,
}

#[derive(Subcommand, Debug)]
enum ServiceAction {
    /// Install and start the background service (auto-starts at login).
    Install(ServiceInstallArgs),
    /// Stop and remove the background service.
    Uninstall,
    /// Show whether the background service is installed/running.
    Status,
}

#[derive(clap::Args, Debug)]
struct ServiceInstallArgs {
    /// Port the background dashboard should listen on.
    #[arg(short, long, default_value_t = 7779)]
    port: u16,

    /// Path to the SQLite database file (defaults to the platform data dir).
    #[arg(long)]
    db: Option<String>,

    /// Open the dashboard in a browser each time the service starts.
    #[arg(long)]
    open: bool,
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

    /// Path to the SQLite database file. Defaults to the platform data dir
    /// (e.g. `~/.local/share/claude-trace-rs/trace.db`).
    #[arg(long, env = "CLAUDE_TRACE_DB")]
    db: Option<String>,
}

impl Default for ServeArgs {
    fn default() -> Self {
        Self {
            port: 7779,
            channel_capacity: 1024,
            backfill: false,
            open: false,
            db: None,
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

    /// Only export sessions from these agent sources (comma-separated).
    #[arg(long = "from", value_delimiter = ',')]
    from_source: Option<Vec<String>>,

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
    let forced_source = cli.source.as_deref().and_then(|s| {
        let p = sources::AgentSource::parse(s);
        if p.is_none() {
            warn!("Unrecognised --source '{s}'; falling back to auto-detect");
        }
        p
    });
    let only: Option<std::collections::HashSet<sources::AgentSource>> =
        cli.only.as_ref().map(|v| {
            v.iter()
                .filter_map(|s| sources::AgentSource::parse(s))
                .collect()
        });
    let roots = resolve_roots(&cli.watch_root, forced_source, only, cli.no_default_roots);

    match cli.cmd.unwrap_or(Cmd::Serve(ServeArgs::default())) {
        Cmd::Service(args) => run_service(&roots, args),
        Cmd::Serve(args) => run_serve(roots, args).await,
        Cmd::Export(args) => run_export(&roots, args),
        Cmd::List => run_list(&roots),
    }
}

/// Turn the CLI flags into a concrete set of watch roots.
///
/// - Any explicit `--watch-root` entries are always included (tagged with
///   `--source` if given, else auto-detect per file within any `--only` filter).
/// - Unless `--no-default-roots`, every known agent log directory that exists
///   on disk is added (tagged with its agent), filtered by `--only`.
fn resolve_roots(
    explicit: &[String],
    forced_source: Option<sources::AgentSource>,
    only: Option<std::collections::HashSet<sources::AgentSource>>,
    no_default_roots: bool,
) -> Vec<sources::WatchRoot> {
    let mut roots: Vec<sources::WatchRoot> = Vec::new();

    for raw in explicit {
        let path = expand_tilde(raw);
        // Honour --only for explicit roots too, either by dropping a forced
        // source outside the allow-list or by carrying the allow-list forward
        // for per-file auto-detection.
        if let (Some(only), Some(src)) = (&only, forced_source) {
            if !only.contains(&src) {
                continue;
            }
        }
        roots.push(sources::WatchRoot {
            path,
            source: forced_source,
            allowed_sources: forced_source.is_none().then(|| only.clone()).flatten(),
        });
    }

    if !no_default_roots {
        for r in sources::default_roots() {
            if let Some(only) = &only {
                if let Some(src) = r.source {
                    if !only.contains(&src) {
                        continue;
                    }
                }
            }
            // Avoid double-adding a directory the user already listed.
            if roots.iter().any(|e| e.path == r.path) {
                continue;
            }
            roots.push(r);
        }
    }

    // Fallback: if nothing was specified and nothing exists on disk yet, use
    // the historical Claude Code default so `claude-trace-rs` with no args
    // behaves exactly as before (and creates the directory).
    //
    // This only applies when the user has not narrowed the source set: with
    // `--only codex` or `--no-default-roots` an empty result is the honest
    // answer, and the caller reports "no watch roots" rather than silently
    // watching (and creating) a Claude Code directory the user excluded.
    let claude_code_wanted = only
        .as_ref()
        .map(|o| o.contains(&sources::AgentSource::ClaudeCode))
        .unwrap_or(true);
    if roots.is_empty() && explicit.is_empty() && !no_default_roots && claude_code_wanted {
        roots.push(sources::WatchRoot {
            path: expand_tilde("~/.claude/projects"),
            source: Some(sources::AgentSource::ClaudeCode),
            allowed_sources: None,
        });
    }

    roots
}

async fn run_serve(roots: Vec<sources::WatchRoot>, args: ServeArgs) -> anyhow::Result<()> {
    anyhow::ensure!(
        args.channel_capacity > 0,
        "--channel-capacity must be at least 1"
    );
    anyhow::ensure!(!roots.is_empty(), "No watch roots to serve");

    for root in &roots {
        if !root.path.exists() {
            info!(
                "Watch root {} does not exist; creating it",
                root.path.display()
            );
            std::fs::create_dir_all(&root.path)?;
        }
    }

    // Open the persistent trace database and seed the in-memory store with the
    // historical session aggregates so the dashboard is populated immediately.
    let db_path = args
        .db
        .as_deref()
        .map(expand_tilde)
        .unwrap_or_else(db::default_db_path);
    let database = db::Db::open(&db_path)?;
    info!("Trace database: {}", database.path().display());
    let store = state::SessionStore::with_db(database.clone());
    match database.load_sessions() {
        Ok(sessions) => {
            info!("Loaded {} session(s) from the database", sessions.len());
            store.seed_sessions(sessions);
        }
        Err(e) => warn!("Could not load sessions from the database: {e}"),
    }

    let (tx, _) = broadcast::channel::<event::TraceEvent>(args.channel_capacity);

    let root_strings: Vec<String> = roots
        .iter()
        .map(|r| r.path.to_string_lossy().to_string())
        .collect();
    let server_state = server::AppState {
        tx: tx.clone(),
        watch_root: root_strings.first().cloned().unwrap_or_default(),
        watch_roots: root_strings,
        port: args.port,
        store: store.clone(),
        db: database,
    };

    let watcher_tx = tx.clone();
    let watcher_store = store.clone();
    let opts = watcher::WatcherOptions {
        backfill: args.backfill,
    };
    std::thread::spawn(move || {
        let watcher = watcher::SessionWatcher::multi(roots, watcher_tx, watcher_store, opts);
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

fn run_export(roots: &[sources::WatchRoot], args: ExportArgs) -> anyhow::Result<()> {
    use std::io::Write as _;

    let store = state::SessionStore::new();
    let n = loader::ingest_roots(roots, &store)?;
    info!(
        "Loaded {} events across {} sessions",
        n,
        store.sessions().len()
    );

    let want: std::collections::HashSet<String> = args.session.into_iter().collect();
    let want_source: Option<std::collections::HashSet<String>> = args.from_source.map(|v| {
        v.iter()
            .filter_map(|s| sources::AgentSource::parse(s))
            .map(|s| s.as_str().to_owned())
            .collect()
    });
    let sessions: Vec<_> = store
        .sessions()
        .into_iter()
        .filter(|s| s.event_count >= args.min_events)
        .filter(|s| want.is_empty() || want.contains(&s.id))
        .filter(|s| match &want_source {
            Some(ws) => ws.contains(&s.source),
            None => true,
        })
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
            println!("Wrote {} session(s) to {}", exports.len(), path.display());
        }
    }
    Ok(())
}

fn run_service(roots: &[sources::WatchRoot], args: ServiceArgs) -> anyhow::Result<()> {
    match args.action {
        ServiceAction::Install(opts) => {
            let exe = std::env::current_exe()
                .context("could not determine the path to the running executable")?;
            // Persist the watch roots as absolute paths so the service is
            // independent of the directory it was installed from.
            let watch_roots: Vec<String> = roots
                .iter()
                .map(|r| {
                    r.path
                        .canonicalize()
                        .unwrap_or_else(|_| r.path.clone())
                        .to_string_lossy()
                        .to_string()
                })
                .collect();
            let cfg = service::ServiceConfig {
                exe,
                port: opts.port,
                watch_roots,
                db: opts
                    .db
                    .map(|d| expand_tilde(&d).to_string_lossy().to_string()),
                open: opts.open,
            };
            service::install(&cfg)
        }
        ServiceAction::Uninstall => service::uninstall(),
        ServiceAction::Status => service::status(),
    }
}

fn run_list(roots: &[sources::WatchRoot]) -> anyhow::Result<()> {
    let store = state::SessionStore::new();
    loader::ingest_roots(roots, &store)?;
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
    use std::collections::HashSet;
    use std::sync::Mutex;

    use super::{expand_tilde, resolve_roots};
    use crate::sources::AgentSource;

    /// `HOME` is process-global, so the tests that override it must not run
    /// concurrently with each other.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn tilde_expansion() {
        let _guard = lock_env();
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

    #[test]
    fn resolve_roots_preserves_only_for_explicit_auto_detect_root() {
        let dir = tempfile::tempdir().unwrap();
        let roots = resolve_roots(
            &[dir.path().display().to_string()],
            None,
            Some(HashSet::from([AgentSource::Codex])),
            true,
        );

        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].path, dir.path());
        assert_eq!(roots[0].source, None);
        assert!(roots[0].allows(AgentSource::Codex));
        assert!(!roots[0].allows(AgentSource::ClaudeCode));
    }
    #[test]
    fn resolve_roots_fallback_respects_only_filter() {
        // `--only codex` with no Codex directory on disk must not fall back to
        // watching (and creating) the Claude Code root the user excluded.
        let _guard = lock_env();
        let empty = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", empty.path());

        let roots = resolve_roots(&[], None, Some(HashSet::from([AgentSource::Codex])), false);
        assert!(
            roots.is_empty(),
            "expected no roots, got {:?}",
            roots.iter().map(|r| r.path.clone()).collect::<Vec<_>>()
        );

        // Without --only the historical Claude Code default still applies.
        let roots = resolve_roots(&[], None, None, false);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].source, Some(AgentSource::ClaudeCode));
    }

    #[test]
    fn resolve_roots_no_default_roots_yields_nothing_without_explicit() {
        let _guard = lock_env();
        let empty = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", empty.path());
        assert!(resolve_roots(&[], None, None, true).is_empty());
    }
}
