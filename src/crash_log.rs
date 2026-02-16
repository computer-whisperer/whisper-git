//! Crash logging infrastructure.
//!
//! Provides a breadcrumb ring buffer, system info capture, panic hook with crash report
//! persistence, and startup crash detection. Uses only std — no external dependencies.

use std::backtrace::Backtrace;
use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Breadcrumb ring buffer
// ---------------------------------------------------------------------------

struct Breadcrumb {
    elapsed: Duration,
    thread: String,
    message: String,
}

static START: OnceLock<Instant> = OnceLock::new();
static BREADCRUMBS: OnceLock<Mutex<VecDeque<Breadcrumb>>> = OnceLock::new();

const MAX_BREADCRUMBS: usize = 50;

/// Initialize globals. Call once at the very start of `main()`.
pub fn init() {
    START.get_or_init(Instant::now);
    BREADCRUMBS.get_or_init(|| Mutex::new(VecDeque::with_capacity(MAX_BREADCRUMBS)));
}

/// Record a breadcrumb. Thread-safe, lock-free if contention is low.
pub fn breadcrumb(message: String) {
    let Some(start) = START.get() else { return };
    let Some(crumbs) = BREADCRUMBS.get() else { return };
    let entry = Breadcrumb {
        elapsed: start.elapsed(),
        thread: std::thread::current().name().unwrap_or("unnamed").to_string(),
        message,
    };
    if let Ok(mut guard) = crumbs.lock() {
        if guard.len() >= MAX_BREADCRUMBS {
            guard.pop_front();
        }
        guard.push_back(entry);
    }
}

/// Format all breadcrumbs for inclusion in a crash report.
fn drain_breadcrumbs() -> String {
    let Some(crumbs) = BREADCRUMBS.get() else {
        return String::new();
    };
    let Ok(guard) = crumbs.lock() else {
        return String::new();
    };
    let mut out = String::new();
    for b in guard.iter() {
        let secs = b.elapsed.as_secs_f64();
        out.push_str(&format!("[{:8.3}s] [{}] {}\n", secs, b.thread, b.message));
    }
    out
}

// ---------------------------------------------------------------------------
// System info
// ---------------------------------------------------------------------------

struct SystemInfo {
    os: String,
    version: String,
    gpu: String,
}

static SYSTEM_INFO: OnceLock<Mutex<SystemInfo>> = OnceLock::new();

fn init_system_info() {
    SYSTEM_INFO.get_or_init(|| {
        Mutex::new(SystemInfo {
            os: std::env::consts::OS.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            gpu: String::new(),
        })
    });
}

/// Set the Vulkan device name after GPU selection.
pub fn set_vulkan_device(name: &str) {
    if let Some(info) = SYSTEM_INFO.get() {
        if let Ok(mut guard) = info.lock() {
            guard.gpu = name.to_string();
        }
    }
}

fn format_system_info() -> String {
    let Some(info) = SYSTEM_INFO.get() else {
        return String::new();
    };
    let Ok(guard) = info.lock() else {
        return String::new();
    };
    let mut out = String::new();
    out.push_str(&format!("Version: {}\n", guard.version));
    out.push_str(&format!("OS: {}\n", guard.os));
    if !guard.gpu.is_empty() {
        out.push_str(&format!("GPU: {}\n", guard.gpu));
    }
    out
}

// ---------------------------------------------------------------------------
// Crash directory helpers
// ---------------------------------------------------------------------------

fn crash_dir() -> PathBuf {
    let mut dir = dirs_path();
    dir.push("crashes");
    dir
}

fn dirs_path() -> PathBuf {
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        let mut p = PathBuf::from(config);
        p.push("whisper-git");
        return p;
    }
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".config");
        p.push("whisper-git");
        return p;
    }
    PathBuf::from("/tmp/whisper-git")
}

fn clean_exit_marker() -> PathBuf {
    let mut p = dirs_path();
    p.push(".last_clean_exit");
    p
}

// ---------------------------------------------------------------------------
// Timestamp formatting (Hinnant's civil calendar, no chrono)
// ---------------------------------------------------------------------------

fn format_timestamp(secs_since_epoch: u64) -> String {
    // Days since epoch
    let total_secs = secs_since_epoch;
    let days = (total_secs / 86400) as i64;
    let day_secs = (total_secs % 86400) as u64;
    let hh = day_secs / 3600;
    let mm = (day_secs % 3600) / 60;
    let ss = day_secs % 60;

    // Hinnant's algorithm: days since 1970-01-01 → (year, month, day)
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{:04}-{:02}-{:02}-{:02}{:02}{:02}", y, m, d, hh, mm, ss)
}

fn format_timestamp_display(secs_since_epoch: u64) -> String {
    let ts = format_timestamp(secs_since_epoch);
    // Convert "YYYY-MM-DD-HHMMSS" → "YYYY-MM-DD HH:MM:SS"
    if ts.len() >= 17 {
        format!(
            "{} {}:{}:{}",
            &ts[..10],
            &ts[11..13],
            &ts[13..15],
            &ts[15..17]
        )
    } else {
        ts
    }
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// Panic hook
// ---------------------------------------------------------------------------

/// Install a custom panic hook that writes crash reports to disk.
/// Call once at startup, after `init()`.
pub fn install_panic_hook() {
    init_system_info();

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Preserve stderr output from default hook
        default_hook(info);

        // Capture backtrace (always, regardless of RUST_BACKTRACE)
        let backtrace = Backtrace::force_capture();

        // Build crash report
        let now = now_epoch_secs();
        let mut report = String::with_capacity(4096);
        report.push_str("=== Whisper-Git Crash Report ===\n");
        report.push_str(&format!("Timestamp: {}\n", format_timestamp_display(now)));
        report.push_str(&format_system_info());

        // Panic message + location
        report.push_str("\n--- Panic ---\n");
        let message = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic payload".to_string()
        };
        report.push_str(&format!("Message: {}\n", message));
        if let Some(loc) = info.location() {
            report.push_str(&format!("Location: {}:{}:{}\n", loc.file(), loc.line(), loc.column()));
        }

        // Breadcrumbs
        let crumbs = drain_breadcrumbs();
        if !crumbs.is_empty() {
            report.push_str("\n--- Breadcrumbs ---\n");
            report.push_str(&crumbs);
        }

        // Backtrace
        report.push_str("\n--- Backtrace ---\n");
        report.push_str(&format!("{}", backtrace));

        // Write to file
        let dir = crash_dir();
        if fs::create_dir_all(&dir).is_ok() {
            let filename = format!("crash-{}.log", format_timestamp(now));
            let path = dir.join(filename);
            if let Ok(mut f) = fs::File::create(&path) {
                let _ = f.write_all(report.as_bytes());
            }
        }
    }));
}

// ---------------------------------------------------------------------------
// Startup detection
// ---------------------------------------------------------------------------

/// Write a marker file indicating a clean exit.
pub fn mark_clean_exit() {
    let path = clean_exit_marker();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&path, now_epoch_secs().to_string());
}

/// Check if there is a crash log newer than the last clean exit marker.
/// Returns the path to the newest crash log if one exists.
pub fn has_crash_since_last_exit() -> Option<PathBuf> {
    let marker = clean_exit_marker();
    let marker_mtime = fs::metadata(&marker)
        .and_then(|m| m.modified())
        .unwrap_or(UNIX_EPOCH);

    let dir = crash_dir();
    let entries = fs::read_dir(&dir).ok()?;

    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("log") {
            continue;
        }
        if let Ok(meta) = path.metadata() {
            if let Ok(mtime) = meta.modified() {
                if mtime > marker_mtime {
                    if newest.as_ref().map_or(true, |(t, _)| mtime > *t) {
                        newest = Some((mtime, path));
                    }
                }
            }
        }
    }

    newest.map(|(_, p)| p)
}

/// Delete oldest crash logs beyond the keep limit.
pub fn prune_crash_logs(keep: usize) {
    let dir = crash_dir();
    let Ok(entries) = fs::read_dir(&dir) else { return };

    let mut logs: Vec<PathBuf> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|e| e.to_str()) == Some("log") {
                Some(p)
            } else {
                None
            }
        })
        .collect();

    if logs.len() <= keep {
        return;
    }

    // Sort by filename (which embeds the timestamp) — newest last
    logs.sort();

    // Remove the oldest entries
    let to_remove = logs.len() - keep;
    for path in &logs[..to_remove] {
        let _ = fs::remove_file(path);
    }
}
