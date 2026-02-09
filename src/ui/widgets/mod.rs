//! Widget implementations

mod button;
pub mod file_list;
mod header_bar;
mod label;
mod panel;
mod repo_dialog;
pub mod scrollbar;
pub mod search_bar;
mod shortcut_bar;
mod tab_bar;
mod text_area;
mod text_input;
mod toast;

pub use button::Button;
pub use file_list::{FileList, FileListAction};
pub use header_bar::{HeaderBar, HeaderAction};
pub use repo_dialog::{RepoDialog, RepoDialogAction};
#[allow(unused_imports)]
pub use scrollbar::{Scrollbar, ScrollAction};
#[allow(unused_imports)]
pub use search_bar::{SearchBar, SearchAction};
pub use shortcut_bar::{ShortcutBar, ShortcutContext};
pub use tab_bar::{TabBar, TabAction};
pub use text_area::TextArea;
pub use text_input::TextInput;
pub use toast::{ToastManager, ToastSeverity};
