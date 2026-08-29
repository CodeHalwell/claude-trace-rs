//! One-shot loader: walk directories of trace files and replay every entry
//! into a [`SessionStore`] without setting up any filesystem watcher.
//!
//! Used by the CLI `export` and `list` subcommands so they can produce a
//! consistent snapshot of historical session data and exit, without keeping
//! the server alive.

use std::{
    collections::HashSet,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use tracing::warn;

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
            allowed_sources: None,
        }],
        store,
    )
}

/// Load every trace file under each root (each with an optional forced
/// source) into `store`. Returns the number of events successfully ingested.
pub fn ingest_roots(roots: &[WatchRoot], store: &SessionStore) -> std::io::Result<usize> {
    let mut count = 0usize;
    // Roots can nest: an explicit `--watch-root ~/.codex` sits above the
    // auto-discovered `~/.codex/sessions`, and resolve_roots only drops exact
    // duplicates. Each root is walked recursively, so without cross-root
    // de-duplication every nested file is ingested once per covering root —
    // doubling session counts and duplicating exported training records.
    //
    // Walk the most specific root first so the more precisely source-tagged
    // root claims its own files, then skip anything already ingested. The
    // watcher resolves the same overlap with `most_specific_root`.
    let mut ordered: Vec<&WatchRoot> = roots.iter().collect();
    ordered.sort_by_key(|r| std::cmp::Reverse(r.path.as_os_str().len()));

    let mut seen: HashSet<PathBuf> = HashSet::new();
    for root in ordered {
        ingest_inner(&root.path, root, store, &mut count, &mut seen)?;
    }
    Ok(count)
}

fn ingest_inner(
    dir: &Path,
    root: &WatchRoot,
    store: &SessionStore,
    count: &mut usize,
    seen: &mut HashSet<PathBuf>,
) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            ingest_inner(&path, root, store, count, seen)?;
        } else if sources::matches_file(root.source, &path) {
            // Already ingested through a more specific overlapping root.
            if !seen.insert(path.clone()) {
                continue;
            }
            *count += ingest_file(&path, root, store)?;
        }
    }
    Ok(())
}

fn ingest_file(path: &Path, root: &WatchRoot, store: &SessionStore) -> std::io::Result<usize> {
    // Whole-file JSON array sources (Cline). Routed on the filename rather than
    // the root's source tag so Cline tasks under an auto-detect root are read
    // as whole-file JSON instead of being line-parsed as JSONL.
    if sources::cline::matches_file(path) {
        return ingest_whole_file(path, root, store);
    }

    let session_fallback = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_owned();
    let file = std::fs::File::open(path)?;
    let mut n = 0;
    let mut detected = root.source;
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
                let s = sources::detect(root.source, path, Some(&val));
                // Only cache a conclusive answer. A generic first record (say a
                // metadata header) sniffs as Unknown, and caching that would
                // stop us ever inspecting the later records that do carry an
                // unmistakable signature.
                if s != AgentSource::Unknown {
                    detected = Some(s);
                }
                s
            }
        };
        if !root.allows(source) {
            // Skip this record rather than abandoning the file: under `--only`
            // an inconclusive first line would otherwise drop a file whose next
            // line identifies it as a source the user did ask for.
            continue;
        }
        let ev = TraceEvent::from_raw_as(&session_fallback, idx, val, source);
        store.ingest(&ev);
        n += 1;
    }
    Ok(n)
}

fn ingest_whole_file(
    path: &Path,
    root: &WatchRoot,
    store: &SessionStore,
) -> std::io::Result<usize> {
    let body = std::fs::read_to_string(path)?;
    // Cline rewrites this file in place as a task progresses, so reading it
    // mid-write is expected rather than exceptional. Warn and skip the one
    // file: propagating the error would abort the whole walk and lose every
    // other session in the export, and silently treating it as empty (the
    // previous `unwrap_or_default`) gave no indication anything was missed.
    let arr: Vec<serde_json::Value> = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "Skipping {}: could not parse as a JSON array: {e}",
                path.display()
            );
            return Ok(0);
        }
    };
    let session_fallback = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_owned();
    let source = root.source.unwrap_or(AgentSource::Cline);
    if !root.allows(source) {
        return Ok(0);
    }
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
                    allowed_sources: None,
                },
                WatchRoot {
                    path: codex_root,
                    source: Some(AgentSource::Codex),
                    allowed_sources: None,
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
                allowed_sources: None,
            }],
            &store,
        )
        .unwrap();
        assert_eq!(n, 2);
        let s = store.session("42").unwrap();
        assert_eq!(s.source, "cline");
        assert_eq!(s.tool_counts.get("read_file"), Some(&1));
    }

    #[test]
    fn explicit_root_only_filters_auto_detected_sources() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("claude.jsonl"),
            "{\"type\":\"user\",\"sessionId\":\"claude\",\"content\":\"hi\"}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("codex.jsonl"),
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"shell\",\"call_id\":\"c1\"}}\n",
        )
        .unwrap();

        let store = SessionStore::new();
        let n = ingest_roots(
            &[WatchRoot {
                path: dir.path().to_path_buf(),
                source: None,
                allowed_sources: Some(std::collections::HashSet::from([AgentSource::Codex])),
            }],
            &store,
        )
        .unwrap();

        assert_eq!(n, 1);
        let sessions = store.sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].source, "codex");
    }

    #[test]
    fn invalid_cline_whole_file_is_skipped_without_aborting_the_walk() {
        // Cline rewrites this file in place, so catching one mid-write is
        // routine; it must not take the rest of the export down with it.
        let dir = tempfile::tempdir().unwrap();
        let tasks = dir.path().join("tasks");
        std::fs::create_dir_all(tasks.join("42")).unwrap();
        std::fs::create_dir_all(tasks.join("43")).unwrap();
        std::fs::write(
            tasks.join("42").join("api_conversation_history.json"),
            "{not json",
        )
        .unwrap();
        std::fs::write(
            tasks.join("43").join("api_conversation_history.json"),
            r#"[{"role":"user","content":"fine"}]"#,
        )
        .unwrap();

        let store = SessionStore::new();
        let n = ingest_roots(
            &[WatchRoot {
                path: tasks,
                source: Some(AgentSource::Cline),
                allowed_sources: None,
            }],
            &store,
        )
        .expect("a half-written file must not fail the whole ingest");

        assert_eq!(n, 1, "the intact task is still ingested");
        assert!(store.session("43").is_some());
        assert!(store.session("42").is_none());
    }
    #[test]
    fn cline_task_with_both_files_is_not_double_counted() {
        // Both files live in tasks/<taskId>/ and each enumerates from index 0,
        // so ingesting both collided on (session_id, line_index): 4 events
        // without a database, 2 with one (the loser silently dropped).
        let dir = tempfile::tempdir().unwrap();
        let task = dir.path().join("tasks").join("42");
        std::fs::create_dir_all(&task).unwrap();
        std::fs::write(
            task.join("api_conversation_history.json"),
            r#"[{"role":"user","content":"go"},{"role":"assistant","content":[{"type":"text","text":"ok"}]}]"#,
        )
        .unwrap();
        std::fs::write(
            task.join("ui_messages.json"),
            r#"[{"ts":1,"type":"say","say":"text","text":"go"},{"ts":2,"type":"say","say":"text","text":"ok"}]"#,
        )
        .unwrap();

        let store = SessionStore::new();
        let n = ingest_roots(
            &[WatchRoot {
                path: dir.path().join("tasks"),
                source: Some(AgentSource::Cline),
                allowed_sources: None,
            }],
            &store,
        )
        .unwrap();

        assert_eq!(n, 2, "the API history is ingested once, the UI log skipped");
        let s = store.session("42").unwrap();
        assert_eq!(s.event_count, 2);
        assert_eq!(s.source, "cline");
    }

    #[test]
    fn cline_task_is_ingested_under_an_auto_detect_root() {
        // An installed service persists roots without a source tag; an untagged
        // root previously admitted only *.jsonl, so Cline was never watched.
        let dir = tempfile::tempdir().unwrap();
        let task = dir.path().join("tasks").join("7");
        std::fs::create_dir_all(&task).unwrap();
        std::fs::write(
            task.join("api_conversation_history.json"),
            r#"[{"role":"user","content":"hello"}]"#,
        )
        .unwrap();

        let store = SessionStore::new();
        let n = ingest_roots(
            &[WatchRoot {
                path: dir.path().to_path_buf(),
                source: None,
                allowed_sources: None,
            }],
            &store,
        )
        .unwrap();

        assert_eq!(n, 1);
        assert_eq!(store.session("7").unwrap().source, "cline");
    }
    #[test]
    fn overlapping_roots_ingest_each_file_once() {
        // `--watch-root ~/.codex` alongside the auto-discovered
        // ~/.codex/sessions: resolve_roots keeps both (they are not equal), and
        // each is walked recursively, so the nested file was ingested twice.
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join("rollout-1.jsonl"),
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hi\"}]}}\n",
        )
        .unwrap();

        let store = SessionStore::new();
        let n = ingest_roots(
            &[
                WatchRoot {
                    path: dir.path().to_path_buf(),
                    source: None,
                    allowed_sources: None,
                },
                WatchRoot {
                    path: sessions,
                    source: Some(AgentSource::Codex),
                    allowed_sources: None,
                },
            ],
            &store,
        )
        .unwrap();

        assert_eq!(n, 1, "one file on disk must be ingested once");
        let sessions = store.sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].event_count, 1);
        // The more specific root claims the file, so its source tag wins.
        assert_eq!(sessions[0].source, "codex");
    }

    #[test]
    fn inconclusive_first_record_does_not_drop_the_file() {
        // Under --only codex a generic first line sniffs as Unknown. Caching
        // that verdict, and bailing out of the file on it, discarded the rest —
        // including the very records that identify it as Codex.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("mixed.jsonl"),
            "{\"note\":\"generic header\"}\n             {\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hi\"}]}}\n",
        )
        .unwrap();

        let store = SessionStore::new();
        let n = ingest_roots(
            &[WatchRoot {
                path: dir.path().to_path_buf(),
                source: None,
                allowed_sources: Some(HashSet::from([AgentSource::Codex])),
            }],
            &store,
        )
        .unwrap();

        assert_eq!(n, 1, "the Codex record is still ingested");
        assert_eq!(store.sessions()[0].source, "codex");
    }
}
