// Shared by `build.rs` (which generates an `allow-<command>` permission for
// each) and `main.rs` (which grants them to the dashboard origin), so the two
// lists cannot drift apart.

/// Commands the dashboard may call.
pub const COMMANDS: &[&str] = &[
    "desktop_info",
    "get_settings",
    "save_settings",
    "get_autostart",
    "set_autostart",
    "restart_app",
    "test_notification",
    "open_data_dir",
    "reveal_path",
    "save_export",
];
