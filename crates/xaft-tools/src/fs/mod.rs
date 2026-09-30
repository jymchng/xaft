//! Filesystem tools for reading, writing, editing, and searching files.

pub mod append_to_file;
pub mod batch_copy;
pub mod batch_delete;
pub mod batch_move;
pub mod batch_read;
pub mod batch_transfer;
pub mod batch_write;
pub mod checksum_file;
pub mod copy_file;
pub mod create_directory;
pub mod delete_file;
pub mod diff_files;
pub mod edit_file;
pub mod file_exists;
pub mod file_stat;
pub mod glob;
pub mod grep;
pub mod list_files;
pub mod move_file;
pub mod patch_file;
pub mod read_before_edit;
pub mod read_file;
pub mod read_lines;
pub mod remove_directory;
pub mod search_files;
pub mod touch_file;
pub mod tree;
pub mod truncate_file;
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
pub use search_files::SearchFilesTool;
pub use tree::{TreeTool, TreeToolFs};

// agenthicc fs-parity tools (PRD 53 follow-up)
pub use batch_copy::BatchCopyTool;
pub use batch_delete::BatchDeleteTool;
pub use batch_move::BatchMoveTool;
pub use batch_read::BatchReadTool;
pub use batch_write::BatchWriteTool;
pub use checksum_file::ChecksumFileTool;
pub use file_exists::FileExistsTool;
pub use read_lines::ReadLinesTool;
pub use touch_file::TouchFileTool;
pub use truncate_file::TruncateFileTool;

// New write tools
pub use append_to_file::AppendToFileTool;
pub use copy_file::CopyFileTool;
pub use create_directory::CreateDirectoryTool;
pub use delete_file::DeleteFileTool;
pub use move_file::MoveFileTool;
pub use patch_file::PatchFileTool;
pub use remove_directory::RemoveDirectoryTool;
