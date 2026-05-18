//! 压缩包功能域装配模块。
//!
//! 业务意图：
//! - 对外提供压缩包格式识别和成员路径安全工具的统一入口。
//! - 具体扫描、读取和物化逻辑会继续收敛到本目录内子模块，调用方不需要直接理解各压缩库差异。
//!
//! 边界条件：
//! - 本模块只做内部子模块装配和 crate 内重导出，不改变日志来源模型、配置格式或用户可见行为。

mod format;
mod materialize;
mod path;
mod read;
mod scan;

pub use self::format::ArchiveFormat;
pub(crate) use self::materialize::{
    MaterializedLogSource, cleanup_materialized_file, cleanup_stale_large_log_cache,
    large_log_cache_root, large_log_session_dir, materialize_source_for_paging,
};
pub(crate) use self::path::{
    is_single_gzip_member_path, join_archive_segments, normalize_archive_member_path,
    single_gzip_member_display_name, single_gzip_member_path_for_archive, split_archive_entry_path,
};
pub(crate) use self::read::{
    ArchiveReadError, read_archive_member, read_archive_member_from_bytes,
    read_single_file_archive_from_bytes, read_single_file_archive_from_path,
    single_file_archive_member_path_from_path, stream_archive_member_from_bytes_to_writer,
    stream_archive_member_to_writer, stream_single_file_archive_from_bytes_to_writer,
    stream_single_file_archive_from_path_to_writer, write_temporary_nested_archive_bytes,
};
#[cfg(test)]
pub(crate) use self::scan::normalized_rar_nested_archive_member_path;
pub(crate) use self::scan::{
    ArchiveMemberSource, ArchiveScanEntryKind, ArchiveScanNode, ArchiveScanResult, scan_archive,
};
