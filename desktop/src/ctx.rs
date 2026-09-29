//! State shared by the setup code, the tray, the notification loop and the
//! commands the dashboard calls.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
    time::Duration,
};

use crate::{activity::Activity, backend::Backend, settings::Settings, tray::Tray};

pub struct Ctx {
    pub settings_path: PathBuf,
    pub settings: Mutex<Settings>,
    /// Settings the running tracer was started with; a restart is needed
    /// only when the saved settings differ from these.
    pub launched_with: Settings,
    pub backend: OnceLock<Backend>,
    pub startup_error: Mutex<Option<String>>,
    pub activity: Mutex<Activity>,
    pub tray: OnceLock<Tray>,
    pub log_path: Option<PathBuf>,
    /// Set by "Quit" so closing the window really closes it.
    pub quitting: AtomicBool,
    /// Whether the "still running in the tray" hint was shown this run.
    pub hid_once: AtomicBool,
}

impl Ctx {
    pub fn new(settings_path: PathBuf, settings: Settings, log_path: Option<PathBuf>) -> Self {
        let activity = Activity::new(
            Duration::from_secs(settings.idle_seconds),
            settings.daily_budget_usd,
        );
        Self {
            settings_path,
            launched_with: settings.clone(),
            settings: Mutex::new(settings),
            backend: OnceLock::new(),
            startup_error: Mutex::new(None),
            activity: Mutex::new(activity),
            tray: OnceLock::new(),
            log_path,
            quitting: AtomicBool::new(false),
            hid_once: AtomicBool::new(false),
        }
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().expect("settings poisoned").clone()
    }

    pub fn is_quitting(&self) -> bool {
        self.quitting.load(Ordering::SeqCst)
    }

    /// The database folder: the running backend's, else the configured one.
    pub fn data_dir(&self) -> PathBuf {
        let db = match self.backend.get() {
            Some(b) => b.db_path.clone(),
            None => crate::backend::db_path(&self.settings()),
        };
        db.parent().map(|p| p.to_path_buf()).unwrap_or(db)
    }
}
