//! Widget implementations

mod button;
pub mod context_menu;
pub mod file_list;
mod header_bar;
#[allow(dead_code)]
mod label;
#[allow(dead_code)]
mod panel;
mod repo_dialog;
pub mod scrollbar;
mod settings_dialog;
pub mod search_bar;
mod shortcut_bar;
mod tab_bar;
mod text_area;
mod text_input;
mod toast;

pub use button::Button;
#[allow(unused_imports)]
pub use context_menu::{ContextMenu, MenuItem, MenuAction};
pub use file_list::{FileList, FileListAction};
pub use header_bar::{HeaderBar, HeaderAction};
pub use repo_dialog::{RepoDialog, RepoDialogAction};
pub use settings_dialog::{SettingsDialog, SettingsDialogAction};
#[allow(unused_imports)]
pub use scrollbar::{Scrollbar, ScrollAction};
#[allow(unused_imports)]
pub use search_bar::{SearchBar, SearchAction};
pub use shortcut_bar::{ShortcutBar, ShortcutContext};
pub use tab_bar::{TabBar, TabAction};
pub use text_area::TextArea;
pub use text_input::TextInput;
pub use toast::{ToastManager, ToastSeverity};
