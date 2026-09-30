//! Persistent desktop settings (`settings.json` in the app config directory).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Port for the embedded dashboard server. If a claude-trace-rs server
    /// (e.g. the background service) already answers here, the app attaches
    /// to it instead of starting a second tracer.
    pub port: u16,
    /// Trace database path; `None` uses the shared platform default so the
    /// CLI, the background service and the app all see the same history.
    pub db_path: Option<String>,
    /// Import everything already on disk the first time the app runs.
    pub import_history: bool,
    /// Restrict tracing to these agent ids (empty = every detected agent).
    pub only: Vec<String>,
    /// Extra directories to watch, as `path` or `path=agent-id`.
    pub extra_roots: Vec<String>,

    /// Notify when an agent finishes its turn and is waiting on you.
    pub notify_turn_end: bool,
    /// Notify when a new session starts.
    pub notify_new_session: bool,
    /// Only notify while the dashboard window is hidden or unfocused.
    pub notify_only_when_away: bool,
    /// Seconds of silence after an assistant reply before an agent without an
    /// explicit end-of-turn marker is considered to be waiting.
    pub idle_seconds: u64,
    /// Daily spend alert in USD (`None` disables it).
    pub daily_budget_usd: Option<f64>,

    /// Closing the window hides it to the tray instead of quitting.
    pub close_to_tray: bool,
    /// Start hidden in the tray (useful with launch-at-login).
    pub start_hidden: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            port: 7779,
            db_path: None,
            import_history: true,
            only: Vec::new(),
            extra_roots: Vec::new(),
            notify_turn_end: true,
            notify_new_session: false,
            notify_only_when_away: true,
            idle_seconds: 45,
            daily_budget_usd: None,
            close_to_tray: true,
            start_hidden: false,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    /// Whether changing from `self` to `other` needs a restart to apply.
    pub fn needs_restart(&self, other: &Settings) -> bool {
        self.port != other.port
            || self.db_path != other.db_path
            || self.only != other.only
            || self.extra_roots != other.extra_roots
    }
}

pub fn settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join("settings.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_defaults_for_missing_fields() {
        let dir = std::env::temp_dir().join(format!("agent-trace-settings-{}", std::process::id()));
        let p = settings_path(&dir);
        let s = Settings {
            daily_budget_usd: Some(12.5),
            ..Default::default()
        };
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p), s);
        std::fs::write(&p, r#"{"port": 9000}"#).unwrap();
        let partial = Settings::load(&p);
        assert_eq!(partial.port, 9000);
        assert!(partial.notify_turn_end);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restart_detection() {
        let a = Settings::default();
        let mut b = a.clone();
        b.notify_turn_end = false;
        assert!(!a.needs_restart(&b));
        b.port = 1234;
        assert!(a.needs_restart(&b));
    }
}
