//! RAR 成员读取与单文件识别。
//!
//! 业务意图：
//! - 从压缩包成员读取父模块拆出具体格式 reader，避免 ZIP/RAR/TAR/GZIP/7Z 的库调用细节堆在同一个文件。
//! - 函数签名、错误文案、读取上限和成员路径安全校验保持原样，父模块仍负责统一格式分发。

use super::*;

/// 从 RAR 文件目录中找出唯一普通文件成员路径。
pub(super) fn single_file_rar_member_path(
    archive_path: &Path,
    label: &str,
) -> Result<String, ArchiveReadError> {
    let archive = unrar::Archive::new(archive_path)
        .open_for_listing()
        .map_err(|error| ArchiveReadError::new(format!("无法打开 RAR 目录：{}", error)))?;
    let mut single_member = None;

    for entry in archive {
        let entry = entry
            .map_err(|error| ArchiveReadError::new(format!("无法读取 RAR 条目：{}", error)))?;
        if entry.is_directory() {
            continue;
        }
        if entry.is_encrypted() {
            return Err(ArchiveReadError::new(format!(
                "{} 内唯一文件是加密 RAR 条目，暂不支持读取",
                label
            )));
        }

        let raw_name = entry.filename.to_string_lossy();
        let normalized = normalize_archive_member_path(&raw_name).map_err(|reason| {
            ArchiveReadError::new(format!("{} 内包含非法 RAR 条目：{}", label, reason))
        })?;
        remember_single_archive_member(&mut single_member, normalized, label)?;
    }

    require_single_archive_member(single_member, label)
}

/// 从 RAR 压缩包中读取成员。
///
/// 边界条件：
/// - 加密成员需要密码规则，当前没有密码输入和缓存策略，因此直接返回可读错误。
pub(super) fn read_rar_member(
    archive_path: &Path,
    member_path: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    let mut archive = unrar::Archive::new(archive_path)
        .open_for_processing()
        .map_err(|error| ArchiveReadError::new(format!("无法打开 RAR 压缩包：{}", error)))?;

    loop {
        let Some(entry_archive) = archive
            .read_header()
            .map_err(|error| ArchiveReadError::new(format!("无法读取 RAR 条目头：{}", error)))?
        else {
            break;
        };

        let header = entry_archive.entry();
        let raw_name = header.filename.to_string_lossy();
        let normalized = normalize_archive_member_path(&raw_name).ok();
        if header.is_directory() || normalized.as_deref() != Some(member_path) {
            archive = entry_archive
                .skip()
                .map_err(|error| ArchiveReadError::new(format!("跳过 RAR 条目失败：{}", error)))?;
            continue;
        }

        if header.is_encrypted() {
            return Err(ArchiveReadError::new("加密 RAR 日志文件暂不支持读取"));
        }

        ensure_size_within_limit(header.unpacked_size, member_path)?;
        let (bytes, _archive) = entry_archive
            .read()
            .map_err(|error| ArchiveReadError::new(format!("读取 RAR 日志文件失败：{}", error)))?;
        ensure_buffer_within_limit(bytes.len(), member_path)?;
        return Ok(bytes);
    }

    Err(ArchiveReadError::new(format!(
        "压缩包中未找到日志文件：{}",
        member_path
    )))
}

/// 从 RAR 压缩包中把成员内容写入 writer。
///
/// 业务意图：
/// - 对宿主插件内容流保持统一的“写入 writer”接口，调用方不需要知道 RAR 库的特殊限制。
///
/// 关键约束：
/// - 当前 `unrar` crate 的处理接口只提供 `read()` 返回完整成员字节，不能像 ZIP/TAR/7Z 一样边解压边写。
/// - 因此这里仍会为单个 RAR 成员短暂持有 `Vec<u8>`，但不会创建新的临时日志文件；用户可见行为仍是后台执行并显示错误。
pub(super) fn stream_rar_member_to_writer<W: Write + ?Sized>(
    archive_path: &Path,
    member_path: &str,
    writer: &mut W,
) -> Result<(), ArchiveReadError> {
    let bytes = read_rar_member(archive_path, member_path)?;
    writer
        .write_all(&bytes)
        .map_err(|error| ArchiveReadError::new(format!("流式写入 RAR 日志文件失败：{}", error)))
}
