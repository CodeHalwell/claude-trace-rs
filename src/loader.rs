//! One-shot loader: walk directories of trace files and replay every entry
//! into a [`SessionStore`] without setting up any filesystem watcher.
//!
//! Used by the CLI `export` and `list` subcommands so they can produce a
//! consistent snapshot of historical session data and exit, without keeping
//! the server alive.

use std::{
    io::{BufRead, BufReader},
    path::Path,
};

use crate::{
    event::TraceEvent,
    sources::{self, AgentSource, WatchRoot},
    state::SessionStore,
};

/// Load every trace file under `root` into `store`, auto-detecting the agent
/// source per file. Returns the number of events successfully ingested.
/// (Backwards-compatible single-root entry point.)
#[allow(dead_code)] // compat shim; production path uses ingest_roots
pub fn ingest_directory(root: &Path, store: &SessionStore) -> std::io::Result<usize> {
    ingest_roots(
        &[WatchRoot {
            path: root.to_path_buf(),
            source: None,
        }],
        store,
    )
}

/// Load every trace file under each root (each with an optional forced
/// source) into `store`. Returns the number of events successfully ingested.
pub fn ingest_roots(roots: &[WatchRoot], store: &SessionStore) -> std::io::Result<usize> {
    let mut count = 0usize;
    for root in roots {
        ingest_inner(&root.path, root.source, store, &mut count)?;
    }
    Ok(count)
}

fn ingest_inner(
    dir: &Path,
    root_source: Option<AgentSource>,
    store: &SessionStore,
    count: &mut usize,
) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            ingest_inner(&path, root_source, store, count)?;
        } else if sources::matches_file(root_source, &path) {
            *count += ingest_file(&path, root_source, store)?;
        }
    }
    Ok(())
}

fn ingest_file(
    path: &Path,
    root_source: Option<AgentSource>,
    store: &SessionStore,
) -> std::io::Result<usize> {
    // Whole-file JSON array sources (Cline).
    if root_source == Some(AgentSource::Cline) && sources::cline::matches_file(path) {
        return ingest_whole_file(path, root_source, store);
    }

    let session_fallback = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_owned();
    let file = std::fs::File::open(path)?;
    let mut n = 0;
    let mut detected = root_source;
    for (idx, line) in BufReader::new(file).lines().enumerate() {
        let Ok(line) = line else { continue };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        let source = match detected {
            Some(s) => s,
            None => {
                let s = sources::detect(root_source, path, Some(&val));
                detected = Some(s);
                s
            }
        };
        let ev = TraceEvent::from_raw_as(&session_fallback, idx, val, source);
        store.ingest(&ev);
        n += 1;
    }
    Ok(n)
}

fn ingest_whole_file(
    path: &Path,
    root_source: Option<AgentSource>,
    store: &SessionStore,
) -> std::io::Result<usize> {
    let body = std::fs::read_to_string(path)?;
    let arr: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap_or_default();
    let session_fallback = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_owned();
    let source = root_source.unwrap_or(AgentSource::Cline);
    let mut n = 0;
    for (idx, val) in arr.into_iter().enumerate() {
        let ev = TraceEvent::from_raw_as(&session_fallback, idx, val, source);
        store.ingest(&ev);
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn loads_all_jsonl_in_tree() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("project-a");
        std::fs::create_dir_all(&sub).unwrap();

        let mut f = std::fs::File::create(sub.join("s1.jsonl")).unwrap();
        writeln!(f, r#"{{"type":"user","sessionId":"s1","content":"hi"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","sessionId":"s1","message":{{"content":[{{"type":"text","text":"hello"}}]}}}}"#).unwrap();

        let mut g = std::fs::File::create(sub.join("s2.jsonl")).unwrap();
        writeln!(g, r#"{{"type":"user","sessionId":"s2","content":"x"}}"#).unwrap();

        let store = SessionStore::new();
        let n = ingest_directory(dir.path(), &store).unwrap();
        assert_eq!(n, 3);
        assert_eq!(store.sessions().len(), 2);
        let s1 = store.session("s1").unwrap();
        assert_eq!(s1.event_count, 2);
    }

    #[test]
    fn missing_dir_is_not_an_error() {
        let store = SessionStore::new();
        let n = ingest_directory(Path::new("/this/does/not/exist"), &store).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn multi_root_ingest_tags_sources() {
        let dir = tempfile::tempdir().unwrap();
        let claude_root = dir.path().join("claude");
        let codex_root = dir.path().join("codex");
        std::fs::create_dir_all(&claude_root).unwrap();
        std::fs::create_dir_all(&codex_root).unwrap();
        std::fs::write(
            claude_root.join("s.jsonl"),
            "{\"type\":\"user\",\"sessionId\":\"cs\",\"content\":\"hi\"}\n",
        )
        .unwrap();
        std::fs::write(
            codex_root.join("rollout-1.jsonl"),
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"shell\",\"call_id\":\"c1\"}}\n",
        )
        .unwrap();

        let store = SessionStore::new();
        let n = ingest_roots(
            &[
                WatchRoot {
                    path: claude_root,
                    source: Some(AgentSource::ClaudeCode),
                },
                WatchRoot {
                    path: codex_root,
                    source: Some(AgentSource::Codex),
                },
            ],
            &store,
        )
        .unwrap();
        assert_eq!(n, 2);
        let sessions = store.sessions();
        assert_eq!(sessions.len(), 2);
        let codex_sess = sessions.iter().find(|s| s.source == "codex").unwrap();
        assert_eq!(codex_sess.tool_use_count, 1);
    }

    #[test]
    fn cline_whole_file_ingest() {
        let dir = tempfile::tempdir().unwrap();
        let task = dir.path().join("tasks").join("42");
        std::fs::create_dir_all(&task).unwrap();
        std::fs::write(
            task.join("api_conversation_history.json"),
            r#"[{"role":"user","content":"go"},{"role":"assistant","content":[{"type":"text","text":"ok"},{"type":"tool_use","name":"read_file","id":"t1"}]}]"#,
        )
        .unwrap();

        let store = SessionStore::new();
        let n = ingest_roots(
            &[WatchRoot {
                path: dir.path().join("tasks"),
                source: Some(AgentSource::Cline),
            }],
            &store,
        )
        .unwrap();
        assert_eq!(n, 2);
        let s = store.session("42").unwrap();
        assert_eq!(s.source, "cline");
        assert_eq!(s.tool_counts.get("read_file"), Some(&1));
    }
}
