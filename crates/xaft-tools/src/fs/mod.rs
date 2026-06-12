//! Filesystem tools for reading, writing, editing, and searching files.

pub mod append_to_file;
pub mod copy_file;
pub mod create_directory;
pub mod delete_file;
pub mod diff_files;
pub mod edit_file;
pub mod file_stat;
pub mod glob;
pub mod grep;
pub mod list_files;
pub mod move_file;
pub mod patch_file;
pub mod read_before_edit;
pub mod read_file;
pub mod read_many;
pub mod remove_directory;
pub mod search_files;
pub mod tree;
pub mod write_file;

// Existing tools
pub use edit_file::EditFileTool;
pub use grep::GrepTool;
pub use list_files::ListFilesTool;
pub use read_before_edit::ReadBeforeEditHook;
pub use read_file::ReadFileTool;
pub use write_file::WriteFileTool;

// New read-only tools
pub use diff_files::DiffFilesTool;
pub use file_stat::{FileStatTool, FileStatToolFs};
pub use glob::{GlobTool, GlobToolFs};
pub use read_many::ReadManyTool;
pub use search_files::SearchFilesTool;
pub use tree::{TreeTool, TreeToolFs};

// New write tools
pub use append_to_file::AppendToFileTool;
pub use copy_file::CopyFileTool;
pub use create_directory::CreateDirectoryTool;
pub use delete_file::DeleteFileTool;
pub use move_file::MoveFileTool;
pub use patch_file::PatchFileTool;
pub use remove_directory::RemoveDirectoryTool;
