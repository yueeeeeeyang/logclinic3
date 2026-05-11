//! TAR.GZ 成员读取与单文件识别。
//!
//! 业务意图：
//! - 从压缩包成员读取父模块拆出具体格式 reader，避免 ZIP/RAR/TAR/GZIP/7Z 的库调用细节堆在同一个文件。
//! - 函数签名、错误文案、读取上限和成员路径安全校验保持原样，父模块仍负责统一格式分发。

use super::*;

/// 从 TAR.GZ 文件目录中找出唯一普通文件成员路径。
pub(super) fn single_file_tar_gz_member_path(
    archive_path: &Path,
    label: &str,
) -> Result<String, ArchiveReadError> {
    let file = File::open(archive_path).map_err(|error| {
        ArchiveReadError::new(format!(
            "无法打开 TAR.GZ 压缩包 {}：{}",
            archive_path.display(),
            error
        ))
    })?;
    let decoder = GzDecoder::new(BufReader::new(file));
    let mut archive = TarArchive::new(decoder);
    let entries = match archive.entries() {
        Ok(entries) => entries,
        Err(error) => {
            if single_gzip_payload_is_readable_from_path(archive_path) {
                return Ok(single_gzip_member_path_for_archive(archive_path));
            }
            return Err(ArchiveReadError::new(format!(
                "无法读取 TAR.GZ 目录：{}",
                error
            )));
        }
    };
    let mut single_member = None;
    let mut entry_errors = Vec::new();

    for entry in entries {
        let mut entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                // 扩展名为 `.tar.gz` 的单文件 gzip 日志会在这里表现为 tar 条目错误。
                // 先延迟错误，循环结束后若没有任何有效文件成员，再尝试按单 gzip 日志打开。
                entry_errors.push(error.to_string());
                continue;
            }
        };
        if entry.header().entry_type().is_dir() {
            continue;
        }

        let entry_path = entry.path().map_err(|error| {
            ArchiveReadError::new(format!("无法读取 TAR.GZ 条目路径：{}", error))
        })?;
        let raw_name = entry_path.to_string_lossy();
        let normalized = normalize_archive_member_path(&raw_name).map_err(|reason| {
            ArchiveReadError::new(format!("{} 内包含非法 TAR.GZ 条目：{}", label, reason))
        })?;
        remember_single_archive_member(&mut single_member, normalized, label)?;
        drain_tar_entry(&mut entry, label)?;
    }

    if single_member.is_none()
        && !entry_errors.is_empty()
        && single_gzip_payload_is_readable_from_path(archive_path)
    {
        return Ok(single_gzip_member_path_for_archive(archive_path));
    }
    if let Some(error) = entry_errors.into_iter().next() {
        return Err(ArchiveReadError::new(format!(
            "无法读取 TAR.GZ 条目：{}",
            error
        )));
    }

    require_single_archive_member(single_member, label)
}

/// 从内存 TAR.GZ 字节中读取唯一普通文件。
///
/// 业务意图：
/// - TAR.GZ 是顺序格式，读取第一个普通文件后仍需继续遍历条目，确认不存在第二个文件。
pub(super) fn read_single_file_tar_gz_from_bytes(
    archive_bytes: &[u8],
    label: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    let decoder = GzDecoder::new(Cursor::new(archive_bytes));
    let mut archive = TarArchive::new(decoder);
    let entries = match archive.entries() {
        Ok(entries) => entries,
        Err(_) => return read_single_gzip_payload_from_bytes(archive_bytes, label),
    };
    let mut single_member = None;
    let mut single_bytes = None;
    let mut entry_errors = Vec::new();

    for entry in entries {
        let mut entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                entry_errors.push(error.to_string());
                continue;
            }
        };
        if entry.header().entry_type().is_dir() {
            continue;
        }

        let entry_path = entry.path().map_err(|error| {
            ArchiveReadError::new(format!("无法读取嵌套 TAR.GZ 条目路径：{}", error))
        })?;
        let raw_name = entry_path.to_string_lossy();
        let normalized = normalize_archive_member_path(&raw_name).map_err(|reason| {
            ArchiveReadError::new(format!("{} 内包含非法 TAR.GZ 条目：{}", label, reason))
        })?;
        remember_single_archive_member(&mut single_member, normalized.clone(), label)?;
        let size = entry.size();
        ensure_size_within_limit(size, &normalized)?;
        single_bytes = Some(read_reader_to_vec_with_limit(
            &mut entry,
            Some(size),
            &normalized,
        )?);
    }

    if single_member.is_none() && !entry_errors.is_empty() {
        return read_single_gzip_payload_from_bytes(archive_bytes, label);
    }
    if let Some(error) = entry_errors.into_iter().next() {
        return Err(ArchiveReadError::new(format!(
            "无法读取嵌套 TAR.GZ 条目：{}",
            error
        )));
    }

    require_single_archive_member(single_member, label)?;
    single_bytes.ok_or_else(|| {
        ArchiveReadError::new(format!(
            "{} 内没有可打开的普通文件，无法直接显示日志内容",
            label
        ))
    })
}

/// 从 tar.gz 或 tgz 压缩包中读取成员。
///
/// 业务意图：
/// - gzip 层和 tar 条目遍历都走 reader，不把成员写入临时目录。
pub(super) fn read_tar_gz_member(
    archive_path: &Path,
    member_path: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    if is_single_gzip_member_path(member_path) {
        return read_single_gzip_payload_from_path(archive_path, member_path);
    }

    let file = File::open(archive_path).map_err(|error| {
        ArchiveReadError::new(format!(
            "无法打开 TAR.GZ 压缩包 {}：{}",
            archive_path.display(),
            error
        ))
    })?;
    let decoder = GzDecoder::new(BufReader::new(file));
    let mut archive = TarArchive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| ArchiveReadError::new(format!("无法读取 TAR.GZ 目录：{}", error)))?;

    for entry in entries {
        let mut entry = entry
            .map_err(|error| ArchiveReadError::new(format!("无法读取 TAR.GZ 条目：{}", error)))?;
        let entry_path = entry.path().map_err(|error| {
            ArchiveReadError::new(format!("无法读取 TAR.GZ 条目路径：{}", error))
        })?;
        let raw_name = entry_path.to_string_lossy().to_string();
        if entry.header().entry_type().is_dir() {
            continue;
        }
        if normalize_archive_member_path(&raw_name).ok().as_deref() != Some(member_path) {
            drain_tar_entry(&mut entry, member_path)?;
            continue;
        }

        let size = entry.size();
        ensure_size_within_limit(size, member_path)?;
        return read_reader_to_vec_with_limit(&mut entry, Some(size), member_path);
    }

    Err(ArchiveReadError::new(format!(
        "压缩包中未找到日志文件：{}",
        member_path
    )))
}

/// 从内存 TAR.GZ 字节中读取指定成员。
pub(super) fn read_tar_gz_member_from_bytes(
    archive_bytes: &[u8],
    member_path: &str,
    label: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    if is_single_gzip_member_path(member_path) {
        return read_single_gzip_payload_from_bytes(archive_bytes, member_path);
    }

    let decoder = GzDecoder::new(Cursor::new(archive_bytes));
    let mut archive = TarArchive::new(decoder);
    let entries = archive.entries().map_err(|error| {
        ArchiveReadError::new(format!("无法读取嵌套 TAR.GZ {} 的目录：{}", label, error))
    })?;

    for entry in entries {
        let mut entry = entry.map_err(|error| {
            ArchiveReadError::new(format!("无法读取嵌套 TAR.GZ 条目：{}", error))
        })?;
        let entry_path = entry.path().map_err(|error| {
            ArchiveReadError::new(format!("无法读取嵌套 TAR.GZ 条目路径：{}", error))
        })?;
        let raw_name = entry_path.to_string_lossy().to_string();
        if entry.header().entry_type().is_dir() {
            continue;
        }
        if normalize_archive_member_path(&raw_name).ok().as_deref() != Some(member_path) {
            drain_tar_entry(&mut entry, member_path)?;
            continue;
        }

        let size = entry.size();
        ensure_size_within_limit(size, member_path)?;
        return read_reader_to_vec_with_limit(&mut entry, Some(size), member_path);
    }

    Err(ArchiveReadError::new(format!(
        "嵌套压缩包 {} 中未找到日志文件：{}",
        label, member_path
    )))
}
