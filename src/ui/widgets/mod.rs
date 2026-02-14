//! Widget implementations

mod branch_name_dialog;
mod button;
pub mod confirm_dialog;
pub mod context_menu;
pub mod file_list;
mod header_bar;
mod merge_dialog;
mod pull_dialog;
mod push_dialog;
mod remote_dialog;
mod repo_dialog;
pub mod scrollbar;
mod settings_dialog;
pub mod search_bar;
mod shortcut_bar;
mod tab_bar;
mod text_area;
mod text_input;
mod toast;

pub use branch_name_dialog::{BranchNameDialog, BranchNameDialogAction};
pub use button::Button;
pub use confirm_dialog::{ConfirmDialog, ConfirmDialogAction};
pub use context_menu::{ContextMenu, MenuItem, MenuAction};
pub use file_list::{FileList, FileListAction};
pub use header_bar::{HeaderBar, HeaderAction};
pub use merge_dialog::{MergeDialog, MergeDialogAction, MergeStrategy};
pub use pull_dialog::{PullDialog, PullDialogAction};
pub use push_dialog::{PushDialog, PushDialogAction};
pub use remote_dialog::{RemoteDialog, RemoteDialogAction};
pub use repo_dialog::{RepoDialog, RepoDialogAction};
pub use settings_dialog::{SettingsDialog, SettingsDialogAction};
pub use shortcut_bar::{ShortcutBar, ShortcutContext};
pub use tab_bar::{TabBar, TabAction};
pub use text_area::TextArea;
pub use text_input::TextInput;
pub use toast::{ToastManager, ToastSeverity};
