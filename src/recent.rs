//! Recent-repository normalization.
//!
//! Config stores paths as strings, but the UI wants repo-level entries:
//! live repositories only, deduped by shared git common directory, with a
//! stable path that opens the repo rather than a linked worktree.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use crate::git::GitRepo;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentRepoEntry {
    pub name: String,
    pub path: PathBuf,
    pub description: String,
}

struct ResolvedRecentRepo {
    entry: RecentRepoEntry,
    identity: PathBuf,
}

pub fn recent_repo_entries(paths: &[String]) -> Vec<RecentRepoEntry> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for path in paths {
        let Some(resolved) = resolve_recent_repo(Path::new(path)) else {
            continue;
        };
        if seen.insert(resolved.identity) {
            entries.push(resolved.entry);
        }
    }
    entries
}

pub fn compact_recent_paths(paths: &[String]) -> Vec<String> {
    recent_repo_entries(paths)
        .into_iter()
        .map(|entry| entry.path.to_string_lossy().into_owned())
        .collect()
}

pub fn recent_repo_entry(path: impl AsRef<Path>) -> Option<RecentRepoEntry> {
    resolve_recent_repo(path.as_ref()).map(|resolved| resolved.entry)
}

fn resolve_recent_repo(path: &Path) -> Option<ResolvedRecentRepo> {
    if !path.exists() {
        return None;
    }
    let repo = GitRepo::open(path).ok()?;
    let identity = existing_path_key(repo.common_dir());
    let open_path = repo_open_path(&repo, path, &identity);
    let description = open_path.to_string_lossy().into_owned();
    Some(ResolvedRecentRepo {
        entry: RecentRepoEntry {
            name: repo.repo_name(),
            path: open_path,
            description,
        },
        identity,
    })
}

fn repo_open_path(repo: &GitRepo, original_path: &Path, identity: &Path) -> PathBuf {
    for candidate in repo_open_path_candidates(repo, original_path) {
        let Some(candidate_repo) = GitRepo::open(&candidate).ok() else {
            continue;
        };
        if existing_path_key(candidate_repo.common_dir()) == identity {
            return existing_path_key(&candidate);
        }
    }
    existing_path_key(original_path)
}

fn repo_open_path_candidates(repo: &GitRepo, original_path: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let common_dir = repo.common_dir();
    let common_name = common_dir.file_name().and_then(|name| name.to_str());

    match common_name {
        Some(".git" | ".bare") => {
            if let Some(parent) = common_dir.parent() {
                candidates.push(parent.to_path_buf());
            }
            candidates.push(common_dir.to_path_buf());
        }
        Some(name) if name.ends_with(".git") => {
            candidates.push(common_dir.to_path_buf());
        }
        _ => {
            candidates.push(common_dir.to_path_buf());
        }
    }

    if let Some(workdir) = repo.workdir() {
        candidates.push(workdir.to_path_buf());
    }
    candidates.push(original_path.to_path_buf());
    dedupe_paths(candidates)
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for path in paths {
        let key = existing_path_key(&path);
        if seen.insert(key) {
            deduped.push(path);
        }
    }
    deduped
}

fn existing_path_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn drops_dead_recent_paths() {
        let paths = vec!["/definitely/not/a/whisper-git/repo".to_string()];
        assert!(recent_repo_entries(&paths).is_empty());
    }

    #[test]
    fn dedupes_paths_inside_the_same_repo() {
        let root = temp_root("same-repo");
        let repo_dir = root.join("project");
        let subdir = repo_dir.join("src");
        fs::create_dir_all(&subdir).unwrap();
        Repository::init(&repo_dir).unwrap();

        let paths = vec![
            subdir.to_string_lossy().into_owned(),
            repo_dir.to_string_lossy().into_owned(),
        ];
        let entries = recent_repo_entries(&paths);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "project");
        assert_eq!(entries[0].path, repo_dir.canonicalize().unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn linked_worktree_resolves_to_the_repo_root() {
        let root = temp_root("worktree");
        let repo_dir = root.join("project");
        let worktree_dir = root.join("project-feature");
        fs::create_dir_all(&repo_dir).unwrap();
        let repo = Repository::init(&repo_dir).unwrap();
        commit_initial_file(&repo, &repo_dir);
        repo.worktree("feature", &worktree_dir, None).unwrap();

        let paths = vec![
            worktree_dir.to_string_lossy().into_owned(),
            repo_dir.to_string_lossy().into_owned(),
        ];
        let entries = recent_repo_entries(&paths);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "project");
        assert_eq!(entries[0].path, repo_dir.canonicalize().unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    fn temp_root(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("whisper-git-recent-{label}-{unique}"))
    }

    fn commit_initial_file(repo: &Repository, repo_dir: &Path) {
        fs::write(repo_dir.join("README.md"), "hello\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("README.md")).unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = Signature::now("whisper-git", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
    }
}
