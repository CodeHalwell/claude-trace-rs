use anyhow::Context;
use clap::{Parser, Subcommand};
use claude_trace_rs::{event, expand_tilde, export, loader, runtime, service, sources, state};
use tracing::{info, warn};

/// Agent Trace — local-first real-time observability for coding agents
/// (Claude Code, Codex, Gemini CLI, Copilot, Cursor, Cline, OpenCode, Aider,
/// and more — run `claude-trace-rs agents` for the full list).
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

    /// Force the agent source for `--watch-root` directories (see
    /// `claude-trace-rs agents` for ids). Auto-detected when omitted.
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
    /// Show every supported agent, where its logs live, and whether they were
    /// found on this machine.
    Agents(AgentsArgs),
    /// Install/manage a background service so the dashboard starts with your OS.
    Service(ServiceArgs),
}

#[derive(clap::Args, Debug)]
struct AgentsArgs {
    /// Print JSON instead of a table.
    #[arg(long)]
    json: bool,
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

    /// Replay every event already on disk at startup. Without this flag,
    /// files seen by an earlier run resume from where they left off, files
    /// written while the tracer was stopped are read in full, and anything
    /// older is skipped (a first run starts at the end of existing logs).
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
    // Logs go to stderr so `export` / `list` output on stdout stays pipeable.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
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
    let only = cli.only.as_deref().map(parse_only).transpose()?;
    let roots = runtime::resolve_roots(&cli.watch_root, forced_source, only, cli.no_default_roots);

    match cli.cmd.unwrap_or(Cmd::Serve(ServeArgs::default())) {
        // Service install persists the user's CLI intent rather than the
        // resolved roots, so `--source` survives and default-root discovery
        // re-runs at service start-up.
        Cmd::Service(args) => run_service(
            &cli.watch_root,
            cli.source.as_deref(),
            cli.only.as_deref(),
            cli.no_default_roots,
            args,
        ),
        Cmd::Serve(args) => run_serve(roots, args).await,
        Cmd::Export(args) => run_export(&roots, args),
        Cmd::List => run_list(&roots),
        Cmd::Agents(args) => run_agents(args),
    }
}

/// Parse `--only`, rejecting ids that name no agent: silently dropping a
/// typo would trace fewer agents than asked for (or none).
fn parse_only(ids: &[String]) -> anyhow::Result<std::collections::HashSet<sources::AgentSource>> {
    let mut out = std::collections::HashSet::new();
    let mut unknown = Vec::new();
    for id in ids.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        match sources::AgentSource::parse(id) {
            Some(s) => {
                out.insert(s);
            }
            None => unknown.push(id.to_owned()),
        }
    }
    anyhow::ensure!(
        unknown.is_empty(),
        "unknown agent id(s) in --only: {} (run `claude-trace-rs agents` for the list)",
        unknown.join(", ")
    );
    Ok(out)
}

async fn run_serve(roots: Vec<sources::WatchRoot>, args: ServeArgs) -> anyhow::Result<()> {
    anyhow::ensure!(!roots.is_empty(), "No watch roots to serve");
    let tracer = runtime::Tracer::start(runtime::TracerConfig {
        roots,
        db_path: args.db.as_deref().map(expand_tilde),
        backfill: args.backfill,
        channel_capacity: args.channel_capacity,
        discover: None,
        create_missing_roots: true,
    })?;

    if args.open {
        let url = format!("http://127.0.0.1:{}/", args.port);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            if let Err(e) = open_in_browser(&url) {
                warn!("Could not open browser ({url}): {e}");
            }
        });
    }

    let listener = runtime::bind_local(args.port)
        .await
        .with_context(|| format!("binding 127.0.0.1:{}", args.port))?;
    tracer.serve(listener).await
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

fn run_service(
    explicit_roots: &[String],
    source: Option<&str>,
    only: Option<&[String]>,
    no_default_roots: bool,
    args: ServiceArgs,
) -> anyhow::Result<()> {
    match args.action {
        ServiceAction::Install(opts) => {
            let exe = std::env::current_exe()
                .context("could not determine the path to the running executable")?;
            // Persist the explicitly requested roots as absolute paths so the
            // service is independent of the directory it was installed from.
            // Auto-discovered roots are deliberately not baked in: the service
            // rediscovers them at start-up, so they stay correctly source-tagged
            // and a newly installed agent is picked up without reinstalling.
            let watch_roots: Vec<String> = explicit_roots
                .iter()
                .map(|r| {
                    let p = expand_tilde(r);
                    p.canonicalize().unwrap_or(p).to_string_lossy().to_string()
                })
                .collect();
            let cfg = service::ServiceConfig {
                exe,
                port: opts.port,
                watch_roots,
                source: source.map(str::to_owned),
                only: only.map(<[String]>::to_vec).unwrap_or_default(),
                no_default_roots,
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

fn run_agents(args: AgentsArgs) -> anyhow::Result<()> {
    let rows: Vec<serde_json::Value> = sources::AgentSource::all_known()
        .iter()
        .map(|src| {
            let spec = src.spec();
            let dirs: Vec<serde_json::Value> = src
                .candidate_dirs()
                .into_iter()
                .map(|d| serde_json::json!({ "path": d, "exists": d.is_dir() }))
                .collect();
            serde_json::json!({
                "id": src.as_str(),
                "name": spec.name,
                "format": spec.format,
                "homepage": spec.homepage,
                "resume": spec.resume,
                "detected": dirs.iter().any(|d| d["exists"] == true),
                "dirs": dirs,
            })
        })
        .collect();
    if args.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    println!("{:<14} {:<22} {:<9} LOG LOCATION", "ID", "AGENT", "FOUND");
    for r in &rows {
        let dirs = r["dirs"].as_array().cloned().unwrap_or_default();
        let found: Vec<&serde_json::Value> = dirs.iter().filter(|d| d["exists"] == true).collect();
        let shown = found.first().copied().or(dirs.first());
        println!(
            "{:<14} {:<22} {:<9} {}",
            r["id"].as_str().unwrap_or(""),
            r["name"].as_str().unwrap_or(""),
            if found.is_empty() { "-" } else { "yes" },
            shown
                .and_then(|d| d["path"].as_str())
                .unwrap_or("(set --watch-root)")
        );
    }
    println!(
        "\nFound agents are watched automatically. Point at anything else with \
         --watch-root <DIR> [--source <ID>]."
    );
    Ok(())
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

    use claude_trace_rs::runtime::resolve_roots;
    use claude_trace_rs::{expand_tilde, sources::AgentSource};

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
    fn only_rejects_unknown_agent_ids() {
        let ok = super::parse_only(&["codex".into(), " claude ".into()]).unwrap();
        assert!(ok.contains(&AgentSource::Codex) && ok.contains(&AgentSource::ClaudeCode));
        let err = super::parse_only(&["codex".into(), "codx".into()]).unwrap_err();
        assert!(err.to_string().contains("codx"), "{err}");
    }

    #[test]
    fn resolve_roots_no_default_roots_yields_nothing_without_explicit() {
        let _guard = lock_env();
        let empty = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", empty.path());
        assert!(resolve_roots(&[], None, None, true).is_empty());
    }
}
