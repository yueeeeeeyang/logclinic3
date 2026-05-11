//! ZIP 成员读取与单文件识别。
//!
//! 业务意图：
//! - 从压缩包成员读取父模块拆出具体格式 reader，避免 ZIP/RAR/TAR/GZIP/7Z 的库调用细节堆在同一个文件。
//! - 函数签名、错误文案、读取上限和成员路径安全校验保持原样，父模块仍负责统一格式分发。

use super::*;

/// 从 ZIP 文件目录中找出唯一普通文件成员路径。
pub(super) fn single_file_zip_member_path(
    archive_path: &Path,
    label: &str,
) -> Result<String, ArchiveReadError> {
    let file = File::open(archive_path).map_err(|error| {
        ArchiveReadError::new(format!(
            "无法打开 ZIP 压缩包 {}：{}",
            archive_path.display(),
            error
        ))
    })?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|error| ArchiveReadError::new(format!("无法读取 ZIP 目录：{}", error)))?;
    let mut single_member = None;

    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| {
            ArchiveReadError::new(format!("无法读取 ZIP 条目 {}：{}", index, error))
        })?;
        if entry.is_dir() {
            continue;
        }

        let normalized = normalize_archive_member_path(entry.name()).map_err(|reason| {
            ArchiveReadError::new(format!("{} 内包含非法 ZIP 条目：{}", label, reason))
        })?;
        remember_single_archive_member(&mut single_member, normalized, label)?;
    }

    require_single_archive_member(single_member, label)
}

/// 从内存 ZIP 字节中读取唯一普通文件。
///
/// 业务意图：
/// - 外层压缩包中的 ZIP 成员已经以字节流形式读入内存；这里继续用 `ZipArchive` 解析，不写临时文件。
pub(super) fn read_single_file_zip_from_bytes(
    archive_bytes: &[u8],
    label: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    let cursor = Cursor::new(archive_bytes);
    let mut archive = ZipArchive::new(cursor)
        .map_err(|error| ArchiveReadError::new(format!("无法读取嵌套 ZIP 目录：{}", error)))?;
    let mut single_index = None;
    let mut single_member = None;

    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| {
            ArchiveReadError::new(format!("无法读取嵌套 ZIP 条目 {}：{}", index, error))
        })?;
        if entry.is_dir() {
            continue;
        }

        let normalized = normalize_archive_member_path(entry.name()).map_err(|reason| {
            ArchiveReadError::new(format!("{} 内包含非法 ZIP 条目：{}", label, reason))
        })?;
        remember_single_archive_member(&mut single_member, normalized, label)?;
        single_index = Some(index);
    }

    let member_label = require_single_archive_member(single_member, label)?;
    let Some(index) = single_index else {
        return Err(ArchiveReadError::new(format!(
            "{} 内没有可打开的普通文件，无法直接显示日志内容",
            label
        )));
    };
    let mut entry = archive
        .by_index(index)
        .map_err(|error| ArchiveReadError::new(format!("无法读取嵌套 ZIP 唯一文件：{}", error)))?;
    let size = entry.size();
    ensure_size_within_limit(size, &member_label)?;
    read_reader_to_vec_with_limit(&mut entry, Some(size), &member_label)
}

/// 从 ZIP 压缩包中读取成员。
///
/// 边界条件：
/// - 不使用 `by_name`，而是遍历并按安全归一化路径匹配，兼容压缩包内部使用反斜杠的情况。
pub(super) fn read_zip_member(
    archive_path: &Path,
    member_path: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    let file = File::open(archive_path).map_err(|error| {
        ArchiveReadError::new(format!(
            "无法打开 ZIP 压缩包 {}：{}",
            archive_path.display(),
            error
        ))
    })?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|error| ArchiveReadError::new(format!("无法读取 ZIP 目录：{}", error)))?;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| {
            ArchiveReadError::new(format!("无法读取 ZIP 条目 {}：{}", index, error))
        })?;
        if entry.is_dir()
            || normalize_archive_member_path(entry.name()).ok().as_deref() != Some(member_path)
        {
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

/// 从内存 ZIP 字节中读取指定成员。
pub(super) fn read_zip_member_from_bytes(
    archive_bytes: &[u8],
    member_path: &str,
    label: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    let mut archive = ZipArchive::new(Cursor::new(archive_bytes)).map_err(|error| {
        ArchiveReadError::new(format!("无法读取嵌套 ZIP {} 的目录：{}", label, error))
    })?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| {
            ArchiveReadError::new(format!("无法读取嵌套 ZIP 条目 {}：{}", index, error))
        })?;
        if entry.is_dir()
            || normalize_archive_member_path(entry.name()).ok().as_deref() != Some(member_path)
        {
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
