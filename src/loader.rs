//! One-shot loader: walk a directory of `.jsonl` files and replay every entry
//! into a [`SessionStore`] without setting up any filesystem watcher.
//!
//! Used by the CLI `export` and `list` subcommands so they can produce a
//! consistent snapshot of historical session data and exit, without keeping
//! the server alive.

use std::{
    io::{BufRead, BufReader},
    path::Path,
};

use crate::{event::TraceEvent, state::SessionStore};

/// Load every `.jsonl` file under `root` into `store`.  Returns the number of
/// events successfully ingested.
pub fn ingest_directory(root: &Path, store: &SessionStore) -> std::io::Result<usize> {
    let mut count = 0usize;
    ingest_inner(root, store, &mut count)?;
    Ok(count)
}

fn ingest_inner(dir: &Path, store: &SessionStore, count: &mut usize) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            ingest_inner(&path, store, count)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let session_fallback = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_owned();
            let file = std::fs::File::open(&path)?;
            for (idx, line) in BufReader::new(file).lines().enumerate() {
                let Ok(line) = line else { continue };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                    continue;
                };
                let ev = TraceEvent::from_raw(&session_fallback, idx, val);
                store.ingest(&ev);
                *count += 1;
            }
        }
    }
    Ok(())
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
}
