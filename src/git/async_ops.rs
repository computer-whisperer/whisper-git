//! Async git CLI operations spawned on background threads.
//!
//! Provides `run_git_async` and the `define_async_git_op!` macro for generating
//! typed async wrappers, plus `classify_git_error` for user-friendly error messages.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use winit::event_loop::EventLoopProxy;

use super::RemoteOpResult;

/// Spawn a background thread to run a git CLI command and send the result over a channel.
fn run_git_async(
    args: Vec<String>,
    workdir: PathBuf,
    op_name: &str,
    proxy: EventLoopProxy<()>,
) -> Receiver<RemoteOpResult> {
    crate::crash_log::breadcrumb(format!("git_async: {op_name} args={args:?}"));
    let op_name = op_name.to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = std::process::Command::new("git")
            .args(&args)
            .current_dir(&workdir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output();
        let op_result = match result {
            Ok(output) => RemoteOpResult {
                success: output.status.success(),
                error: String::from_utf8_lossy(&output.stderr).to_string(),
            },
            Err(e) => RemoteOpResult {
                success: false,
                error: format!("Failed to run git {}: {}", op_name, e),
            },
        };
        crate::crash_log::breadcrumb(format!(
            "git_async done: {op_name} success={}",
            op_result.success
        ));
        let _ = tx.send(op_result);
        let _ = proxy.send_event(());
    });
    rx
}

/// Define an async git wrapper that delegates to `run_git_async`.
///
/// Each invocation generates a `pub fn $name(workdir: PathBuf, ...) -> Receiver<RemoteOpResult>`
/// that constructs the arg vector and calls `run_git_async`.
///
/// Syntax:
///   `fn_name(param: Type, ...) => [arg_expr, ...], "op_name";`
macro_rules! define_async_git_op {
    ($(
        $(#[doc = $doc:expr])*
        $name:ident( $($param:ident : $pty:ty),* ) => [ $($arg:expr),+ $(,)? ], $op:expr;
    )*) => {
        $(
            $(#[doc = $doc])*
            pub fn $name(workdir: PathBuf, $($param: $pty,)* proxy: EventLoopProxy<()>) -> Receiver<RemoteOpResult> {
                run_git_async(vec![$($arg.into()),+], workdir, $op, proxy)
            }
        )*
    };
}

define_async_git_op! {
    /// Spawn a background thread to run `git fetch --prune`
    fetch_remote_async(remote: String) =>
        ["fetch", "--prune", remote], "fetch";

    /// Spawn a background thread to run `git fetch --all --prune`
    fetch_all_async() =>
        ["fetch", "--all", "--prune"], "fetch --all";

    /// Spawn a background thread to run `git push`
    push_remote_async(remote: String, branch: String) =>
        ["push", remote, branch], "push";

    /// Spawn a background thread to run `git push --force-with-lease`
    push_force_async(remote: String, branch: String) =>
        ["push", "--force-with-lease", remote, branch], "push";

    /// Spawn a background thread to run `git push` with a refspec (local:remote format)
    push_refspec_async(remote: String, refspec: String) =>
        ["push", remote, refspec], "push";

    /// Spawn a background thread to run `git push --force-with-lease` with a refspec
    push_force_refspec_async(remote: String, refspec: String) =>
        ["push", "--force-with-lease", remote, refspec], "push";

    /// Spawn a background thread to run `git push --tags <remote>`
    push_tags_only_async(remote: String) =>
        ["push", "--tags", remote], "push --tags";

    /// Spawn a background thread to run `git pull`
    pull_remote_async(remote: String, branch: String) =>
        ["pull", remote, branch], "pull";

    /// Spawn a background thread to run `git pull --rebase`
    pull_rebase_async(remote: String, branch: String) =>
        ["pull", "--rebase", remote, branch], "pull --rebase";

    /// Spawn a background thread to update a submodule
    update_submodule_async(name: String) =>
        ["submodule", "update", "--init", "--recursive", "--", name], "submodule update";

    /// Spawn a background thread to force-reset a submodule checkout to the recorded pin
    reset_submodule_async(name: String) =>
        ["submodule", "update", "--init", "--recursive", "--force", "--", name], "submodule reset";

    /// Spawn a background thread to create a worktree for a branch
    create_worktree_async(path: String, branch: String) =>
        ["worktree", "add", path, branch], "worktree add";

    /// Spawn a background thread to create a detached worktree at a commit
    create_worktree_detached_async(path: String, commitish: String) =>
        ["worktree", "add", "--detach", path, commitish], "worktree add";

    /// Spawn a background thread to remove a clean worktree
    remove_worktree_async(target: String) =>
        ["worktree", "remove", target], "worktree remove";

    /// Spawn a background thread to force-remove a dirty worktree
    remove_worktree_force_async(target: String) =>
        ["worktree", "remove", "--force", target], "worktree remove --force";

    /// Spawn a background thread to merge a branch into the current branch
    merge_branch_async(branch_name: String) =>
        ["merge", branch_name], "merge";

    /// Spawn a background thread to merge with --no-ff (always create merge commit)
    merge_noff_async(branch_name: String, message: String) =>
        ["merge", "--no-ff", "-m", message, branch_name], "merge --no-ff";

    /// Spawn a background thread to merge with --ff-only (fail if not fast-forwardable)
    merge_ffonly_async(branch_name: String) =>
        ["merge", "--ff-only", branch_name], "merge --ff-only";

    /// Spawn a background thread to merge with --squash (stage changes, don't auto-commit)
    merge_squash_async(branch_name: String) =>
        ["merge", "--squash", branch_name], "merge --squash";

}

/// Spawn a background thread to delete a branch on the remote, then
/// best-effort delete the local remote-tracking ref so the sidebar
/// reflects the deletion without waiting for a later fetch/prune.
pub fn delete_remote_branch_async(
    workdir: PathBuf,
    remote: String,
    branch: String,
    proxy: EventLoopProxy<()>,
) -> Receiver<RemoteOpResult> {
    crate::crash_log::breadcrumb(format!(
        "git_async: delete remote branch remote={remote:?} branch={branch:?}"
    ));
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = std::process::Command::new("git")
            .args(["push", &remote, "--delete", &branch])
            .current_dir(&workdir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output();
        let op_result = match result {
            Ok(output) => {
                let success = output.status.success();
                if success {
                    let remote_ref = format!("{remote}/{branch}");
                    let _ = std::process::Command::new("git")
                        .args(["branch", "-dr", &remote_ref])
                        .current_dir(&workdir)
                        .env("GIT_TERMINAL_PROMPT", "0")
                        .output();
                }
                RemoteOpResult {
                    success,
                    error: String::from_utf8_lossy(&output.stderr).to_string(),
                }
            }
            Err(e) => RemoteOpResult {
                success: false,
                error: format!("Failed to run git delete remote branch: {e}"),
            },
        };
        crate::crash_log::breadcrumb(format!(
            "git_async done: delete remote branch success={}",
            op_result.success
        ));
        let _ = tx.send(op_result);
        let _ = proxy.send_event(());
    });
    rx
}

/// Spawn a background thread to run `git push` with arbitrary flag combinations.
///
/// Backs the push-options modal: any combination of `--force-with-lease`,
/// `--set-upstream`, and `--tags` may be set. The macro-generated helpers
/// can't express conditional args, so this is hand-rolled.
pub fn push_with_options_async(
    workdir: PathBuf,
    remote: String,
    branch: String,
    force_with_lease: bool,
    set_upstream: bool,
    include_tags: bool,
    proxy: EventLoopProxy<()>,
) -> Receiver<RemoteOpResult> {
    let mut args: Vec<String> = vec!["push".to_string()];
    if force_with_lease {
        args.push("--force-with-lease".to_string());
    }
    if set_upstream {
        args.push("--set-upstream".to_string());
    }
    if include_tags {
        args.push("--tags".to_string());
    }
    args.push(remote);
    args.push(branch);
    run_git_async(args, workdir, "push", proxy)
}

/// Spawn a background thread to run `git clone [--bare] <url> <dest>`.
/// Unlike the other async ops in this module, clone has no `workdir` —
/// it *creates* one — so it returns its own result type carrying either
/// the destination path on success or the captured stderr on failure.
pub fn clone_async(
    url: String,
    dest: PathBuf,
    bare: bool,
    proxy: EventLoopProxy<()>,
) -> Receiver<Result<PathBuf, String>> {
    crate::crash_log::breadcrumb(format!("clone_async: url={url} dest={dest:?} bare={bare}"));
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("clone");
        if bare {
            cmd.arg("--bare");
        }
        cmd.arg(&url).arg(&dest);
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        let result = match cmd.output() {
            Ok(out) if out.status.success() => Ok(dest),
            Ok(out) => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
            Err(e) => Err(format!("Failed to run git clone: {e}")),
        };
        crate::crash_log::breadcrumb(format!("clone_async done: ok={}", result.is_ok()));
        let _ = tx.send(result);
        let _ = proxy.send_event(());
    });
    rx
}

/// Spawn a background thread to rebase with options (--autostash, --rebase-merges)
pub fn rebase_with_options_async(
    workdir: PathBuf,
    branch: String,
    autostash: bool,
    rebase_merges: bool,
    proxy: EventLoopProxy<()>,
) -> Receiver<RemoteOpResult> {
    let mut args: Vec<String> = vec!["rebase".into()];
    if autostash {
        args.push("--autostash".into());
    }
    if rebase_merges {
        args.push("--rebase-merges".into());
    }
    args.push(branch);
    run_git_async(args, workdir, "rebase", proxy)
}

define_async_git_op! {
    /// Spawn a background thread to stash all changes
    stash_push_async() =>
        ["stash", "push"], "stash push";

    /// Spawn a background thread to pop the most recent stash
    stash_pop_async() =>
        ["stash", "pop"], "stash pop";

    /// Spawn a background thread to cherry-pick a commit
    cherry_pick_async(sha: String) =>
        ["cherry-pick", sha], "cherry-pick";

    /// Spawn a background thread to revert a commit
    revert_commit_async(sha: String) =>
        ["revert", "--no-edit", sha], "revert";
}

/// Spawn a background thread to apply a stash entry (without removing it)
pub fn stash_apply_async(
    workdir: PathBuf,
    index: usize,
    proxy: EventLoopProxy<()>,
) -> Receiver<RemoteOpResult> {
    run_git_async(
        vec![
            "stash".into(),
            "apply".into(),
            format!("stash@{{{}}}", index),
        ],
        workdir,
        "stash apply",
        proxy,
    )
}

/// Spawn a background thread to drop a stash entry
pub fn stash_drop_async(
    workdir: PathBuf,
    index: usize,
    proxy: EventLoopProxy<()>,
) -> Receiver<RemoteOpResult> {
    run_git_async(
        vec![
            "stash".into(),
            "drop".into(),
            format!("stash@{{{}}}", index),
        ],
        workdir,
        "stash drop",
        proxy,
    )
}

/// Spawn a background thread to pop a stash entry by index
pub fn stash_pop_index_async(
    workdir: PathBuf,
    index: usize,
    proxy: EventLoopProxy<()>,
) -> Receiver<RemoteOpResult> {
    run_git_async(
        vec!["stash".into(), "pop".into(), format!("stash@{{{}}}", index)],
        workdir,
        "stash pop",
        proxy,
    )
}

/// Spawn a background thread to remove a submodule (deinit + rm)
pub fn remove_submodule_async(
    workdir: PathBuf,
    name: String,
    proxy: EventLoopProxy<()>,
) -> Receiver<RemoteOpResult> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // Step 1: deinit
        let deinit = std::process::Command::new("git")
            .args(["submodule", "deinit", "-f", &name])
            .current_dir(&workdir)
            .output();
        match deinit {
            Ok(output) if output.status.success() => {
                // Step 2: rm
                let rm = std::process::Command::new("git")
                    .args(["rm", "-f", &name])
                    .current_dir(&workdir)
                    .output();
                let op_result = match rm {
                    Ok(output) => RemoteOpResult {
                        success: output.status.success(),
                        error: String::from_utf8_lossy(&output.stderr).to_string(),
                    },
                    Err(e) => RemoteOpResult {
                        success: false,
                        error: format!("Failed to run git rm: {}", e),
                    },
                };
                let _ = tx.send(op_result);
            }
            Ok(output) => {
                let _ = tx.send(RemoteOpResult {
                    success: false,
                    error: String::from_utf8_lossy(&output.stderr).to_string(),
                });
            }
            Err(e) => {
                let _ = tx.send(RemoteOpResult {
                    success: false,
                    error: format!("Failed to run git submodule deinit: {}", e),
                });
            }
        }
        let _ = proxy.send_event(());
    });
    rx
}

/// Spawn a background thread to create a worktree and then initialize submodules in it.
/// Chains `git worktree add` + `git submodule update --init --recursive` in the new worktree.
/// Create a worktree and run optional post-creation steps (submodule init, LFS checkout).
pub fn create_worktree_with_post_steps_async(
    workdir: PathBuf,
    wt_path: String,
    source: String,
    detached: bool,
    init_submodules: bool,
    checkout_lfs: bool,
    proxy: EventLoopProxy<()>,
) -> Receiver<RemoteOpResult> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // Step 1: create worktree
        let mut args = vec!["worktree", "add"];
        if detached {
            args.push("--detach");
        }
        args.push(&wt_path);
        args.push(&source);

        let wt_result = std::process::Command::new("git")
            .args(&args)
            .current_dir(&workdir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output();

        match wt_result {
            Ok(output) if output.status.success() => {
                let mut warnings = Vec::new();

                // Step 2: init submodules
                if init_submodules {
                    match std::process::Command::new("git")
                        .args(["submodule", "update", "--init", "--recursive"])
                        .current_dir(&wt_path)
                        .env("GIT_TERMINAL_PROMPT", "0")
                        .output()
                    {
                        Ok(out) if !out.status.success() => {
                            warnings.push(format!(
                                "submodule init failed:\n{}",
                                String::from_utf8_lossy(&out.stderr)
                            ));
                        }
                        Err(e) => {
                            warnings.push(format!("failed to run submodule update: {}", e));
                        }
                        _ => {}
                    }
                }

                // Step 3: LFS checkout
                if checkout_lfs {
                    match std::process::Command::new("git")
                        .args(["lfs", "checkout"])
                        .current_dir(&wt_path)
                        .env("GIT_TERMINAL_PROMPT", "0")
                        .output()
                    {
                        Ok(out) if !out.status.success() => {
                            warnings.push(format!(
                                "LFS checkout failed:\n{}",
                                String::from_utf8_lossy(&out.stderr)
                            ));
                        }
                        Err(e) => {
                            warnings.push(format!("failed to run git lfs checkout: {}", e));
                        }
                        _ => {}
                    }
                }

                let op_result = if warnings.is_empty() {
                    RemoteOpResult {
                        success: true,
                        error: String::new(),
                    }
                } else {
                    RemoteOpResult {
                        success: false,
                        error: format!("Worktree created, but {}", warnings.join("; ")),
                    }
                };
                let _ = tx.send(op_result);
            }
            Ok(output) => {
                let _ = tx.send(RemoteOpResult {
                    success: false,
                    error: String::from_utf8_lossy(&output.stderr).to_string(),
                });
            }
            Err(e) => {
                let _ = tx.send(RemoteOpResult {
                    success: false,
                    error: format!("Failed to run git worktree add: {}", e),
                });
            }
        }
        let _ = proxy.send_event(());
    });
    rx
}

/// Classify a git CLI stderr message into a user-friendly error string.
/// Returns `(friendly_message, is_rejected)` where `is_rejected` indicates
/// the remote rejected the push (e.g. non-fast-forward).
pub fn classify_git_error(op: &str, stderr: &str) -> (String, bool) {
    let lower = stderr.to_lowercase();
    let is_rejected = lower.contains("rejected") || lower.contains("non-fast-forward");

    let friendly = if lower.contains("terminal prompts disabled")
        || lower.contains("could not read username")
    {
        format!(
            "{} failed: Authentication required. Configure SSH keys or a credential helper.",
            op
        )
    } else if lower.contains("permission denied") {
        format!(
            "{} failed: Permission denied. Check your SSH key or access token.",
            op
        )
    } else if lower.contains("could not read password") {
        format!(
            "{} failed: Password required. Set up a credential helper (git config credential.helper cache).",
            op
        )
    } else if lower.contains("host key verification failed") {
        format!(
            "{} failed: SSH host key not trusted. Run ssh-keyscan to add the host.",
            op
        )
    } else if lower.contains("repository not found") || lower.contains("404") {
        format!("{} failed: Repository not found. Check the remote URL.", op)
    } else if lower.contains("connection refused") || lower.contains("could not resolve") {
        format!(
            "{} failed: Cannot connect to remote. Check your network and remote URL.",
            op
        )
    } else if lower.contains("local changes to the following files would be overwritten") {
        // Extract the file names from the stderr (they appear between the error line and "Please commit")
        let files: Vec<&str> = stderr
            .lines()
            .skip_while(|l| !l.to_lowercase().contains("would be overwritten"))
            .skip(1)
            .take_while(|l| !l.to_lowercase().starts_with("please"))
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();
        if files.is_empty() {
            format!(
                "{} aborted: Local changes would be overwritten. Commit or stash your changes first.",
                op
            )
        } else {
            format!(
                "{} aborted: Local changes to {} would be overwritten. Commit or stash first.",
                op,
                files.join(", ")
            )
        }
    } else if lower.contains("not possible to fast-forward") {
        format!(
            "{} failed: Cannot fast-forward — the branches have diverged. Pull with merge or rebase instead.",
            op
        )
    } else if lower.contains("merge conflict") || lower.contains("fix conflicts") {
        format!(
            "{} stopped: Merge conflicts need to be resolved. Check the staging area for conflicted files.",
            op
        )
    } else if lower.contains("needs merge") {
        format!(
            "{} failed: Unresolved merge in progress. Resolve conflicts and commit, or abort the merge first.",
            op
        )
    } else if lower.contains("you have unstaged changes")
        || lower.contains("your index contains uncommitted changes")
    {
        format!(
            "{} aborted: You have uncommitted changes. Commit or stash them first.",
            op
        )
    } else if is_rejected {
        format!(
            "{} rejected: Remote has new commits. Pull first, or use Force Push.",
            op
        )
    } else {
        // Show first meaningful lines of the error for context
        let error_lines: Vec<&str> = stderr
            .lines()
            .filter(|l| {
                let trimmed = l.trim();
                !trimmed.is_empty()
                    && !trimmed.starts_with("From ")
                    && !trimmed.starts_with("To ")
                    && !trimmed.starts_with(" * branch")
                    && !trimmed.starts_with(" * [new")
                    && !trimmed.starts_with("   ")
                    && !trimmed.starts_with("Updating ")
            })
            .take(3)
            .collect();
        let error_summary = error_lines.join("\n");
        if error_summary.is_empty() {
            format!("{} failed: unknown error", op)
        } else {
            format!("{} failed: {}", op, error_summary)
        }
    };

    (friendly, is_rejected)
}
