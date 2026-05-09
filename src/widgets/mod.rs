//! Whisper-git's local widget catalog. Each widget here is a candidate
//! for upstreaming into aetna once the API has settled — the data
//! interfaces are deliberately decoupled from libgit2 / `RepoTab`
//! types so the widget renders a value, not a query.

pub mod diff;
