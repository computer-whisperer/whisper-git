use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

/// Debounce interval: coalesce rapid filesystem events into a single refresh signal.
const DEBOUNCE_MS: u64 = 500;

/// Watches a repository's working directory and git metadata files for changes,
/// sending a debounced `()` signal when something relevant changes.
pub struct RepoWatcher {
    _watcher: RecommendedWatcher,
}

impl RepoWatcher {
    /// Create a new watcher for the given workdir and git dir.
    ///
    /// Returns the watcher handle and a receiver that yields `()` after a
    /// debounced period of quiet following filesystem changes.
    pub fn new(workdir: &Path, git_dir: &Path) -> notify::Result<(Self, Receiver<()>)> {
        let (debounce_tx, debounce_rx) = mpsc::channel::<()>();
        let (raw_tx, raw_rx) = mpsc::channel::<Event>();

        // Spawn a debounce thread that coalesces rapid events
        spawn_debounce_thread(raw_rx, debounce_tx);

        // Build the event filter with cloned paths for the closure
        let git_dir_owned = git_dir.to_path_buf();

        let watcher_tx = raw_tx;
        let mut watcher = RecommendedWatcher::new(
            move |res: notify::Result<Event>| {
                if let Ok(event) = res {
                    if is_relevant_event(&event, &git_dir_owned) {
                        let _ = watcher_tx.send(event);
                    }
                }
            },
            Config::default(),
        )?;

        // Watch the working directory recursively for file edits
        watcher.watch(workdir, RecursiveMode::Recursive)?;

        // Watch specific git metadata paths (non-recursive) for branch/commit changes.
        // These fire when the user runs git commands externally.
        let refs_dir = git_dir.join("refs");
        let _ = watcher.watch(git_dir, RecursiveMode::NonRecursive);
        let _ = watcher.watch(&refs_dir, RecursiveMode::Recursive);

        Ok((RepoWatcher { _watcher: watcher }, debounce_rx))
    }
}

/// Returns true if the event is something we care about (file create/modify/remove),
/// filtering out noisy internal `.git/` churn we don't need.
fn is_relevant_event(event: &Event, git_dir: &Path) -> bool {
    // Only care about data-changing events
    match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {}
        _ => return false,
    }

    for path in &event.paths {
        // If the path is inside the git dir, only allow specific metadata files
        if path.starts_with(git_dir) {
            if let Ok(relative) = path.strip_prefix(git_dir) {
                let rel_str = relative.to_string_lossy();
                // HEAD, index, refs/*, MERGE_HEAD, REBASE_HEAD, CHERRY_PICK_HEAD
                if rel_str == "HEAD"
                    || rel_str == "index"
                    || rel_str.starts_with("refs")
                    || rel_str == "MERGE_HEAD"
                    || rel_str == "REBASE_HEAD"
                    || rel_str == "CHERRY_PICK_HEAD"
                {
                    return true;
                }
                // Skip everything else inside .git (objects, logs, etc.)
                continue;
            }
        }
        // Paths outside .git are always relevant
        return true;
    }

    false
}

/// Spawns a background thread that receives raw events and, after `DEBOUNCE_MS`
/// of quiet, sends a single `()` on `out_tx`. Multiple rapid events collapse
/// into one signal.
fn spawn_debounce_thread(raw_rx: Receiver<Event>, out_tx: Sender<()>) {
    std::thread::Builder::new()
        .name("fs-watcher-debounce".into())
        .spawn(move || {
            let mut last_event: Option<Instant> = None;

            loop {
                let timeout = match last_event {
                    Some(t) => {
                        let elapsed = t.elapsed();
                        let debounce = Duration::from_millis(DEBOUNCE_MS);
                        if elapsed >= debounce {
                            // Debounce period passed, fire immediately
                            last_event = None;
                            if out_tx.send(()).is_err() {
                                return; // Main thread gone
                            }
                            Duration::from_millis(DEBOUNCE_MS)
                        } else {
                            debounce - elapsed
                        }
                    }
                    None => Duration::from_secs(60), // Idle: long wait
                };

                match raw_rx.recv_timeout(timeout) {
                    Ok(_event) => {
                        // Got an event, reset the debounce timer
                        last_event = Some(Instant::now());
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // Timeout expired
                        if last_event.is_some() {
                            last_event = None;
                            if out_tx.send(()).is_err() {
                                return;
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        // Watcher dropped, exit thread
                        return;
                    }
                }
            }
        })
        .expect("Failed to spawn fs-watcher-debounce thread");
}
