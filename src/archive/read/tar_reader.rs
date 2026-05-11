//! TAR 成员读取与单文件识别。
//!
//! 业务意图：
//! - 从压缩包成员读取父模块拆出具体格式 reader，避免 ZIP/RAR/TAR/GZIP/7Z 的库调用细节堆在同一个文件。
//! - 函数签名、错误文案、读取上限和成员路径安全校验保持原样，父模块仍负责统一格式分发。

use super::*;

/// 从 TAR 文件目录中找出唯一普通文件成员路径。
///
/// 业务意图：
/// - 支持 `.tar` 文件，以及扩展名误写成 `.tar.gz` 但真实内容为纯 tar 的日志包。
/// - 只返回成员路径，不读取正文；真正读取仍走按成员匹配的流式路径。
pub(super) fn single_file_tar_member_path(
    archive_path: &Path,
    label: &str,
) -> Result<String, ArchiveReadError> {
    let file = File::open(archive_path).map_err(|error| {
        ArchiveReadError::new(format!(
            "无法打开 TAR 压缩包 {}：{}",
            archive_path.display(),
            error
        ))
    })?;
    let mut archive = TarArchive::new(BufReader::new(file));
    let entries = archive
        .entries()
        .map_err(|error| ArchiveReadError::new(format!("无法读取 TAR 目录：{}", error)))?;
    let mut single_member = None;

    for entry in entries {
        let mut entry = entry
            .map_err(|error| ArchiveReadError::new(format!("无法读取 TAR 条目：{}", error)))?;
        if entry.header().entry_type().is_dir() {
            continue;
        }

        let entry_path = entry
            .path()
            .map_err(|error| ArchiveReadError::new(format!("无法读取 TAR 条目路径：{}", error)))?;
        let raw_name = entry_path.to_string_lossy();
        let normalized = normalize_archive_member_path(&raw_name).map_err(|reason| {
            ArchiveReadError::new(format!("{} 内包含非法 TAR 条目：{}", label, reason))
        })?;
        remember_single_archive_member(&mut single_member, normalized, label)?;
        drain_tar_entry(&mut entry, label)?;
    }

    require_single_archive_member(single_member, label)
}

/// 从内存 TAR 字节中读取唯一普通文件。
///
/// 业务意图：
/// - 外层压缩包中的纯 tar 成员可以直接基于内存字节扫描，不需要落盘。
/// - 只有确认内部恰好一个普通文件时才返回正文，避免多文件包误打开第一个文件。
pub(super) fn read_single_file_tar_from_bytes(
    archive_bytes: &[u8],
    label: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    let mut archive = TarArchive::new(Cursor::new(archive_bytes));
    let entries = archive
        .entries()
        .map_err(|error| ArchiveReadError::new(format!("无法读取嵌套 TAR 目录：{}", error)))?;
    let mut single_member = None;
    let mut single_bytes = None;

    for entry in entries {
        let mut entry = entry
            .map_err(|error| ArchiveReadError::new(format!("无法读取嵌套 TAR 条目：{}", error)))?;
        if entry.header().entry_type().is_dir() {
            continue;
        }

        let entry_path = entry.path().map_err(|error| {
            ArchiveReadError::new(format!("无法读取嵌套 TAR 条目路径：{}", error))
        })?;
        let raw_name = entry_path.to_string_lossy();
        let normalized = normalize_archive_member_path(&raw_name).map_err(|reason| {
            ArchiveReadError::new(format!("{} 内包含非法 TAR 条目：{}", label, reason))
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

    require_single_archive_member(single_member, label)?;
    single_bytes.ok_or_else(|| {
        ArchiveReadError::new(format!(
            "{} 内没有可打开的普通文件，无法直接显示日志内容",
            label
        ))
    })
}

/// 从 TAR 压缩包中读取成员。
///
/// 业务意图：
/// - 支持纯 `.tar` 归档和扩展名误写成 `.tar.gz` 的纯 tar 日志包。
/// - 读取时顺序跳过非目标条目，找到目标后才把正文读入内存或触发大小上限错误。
pub(super) fn read_tar_member(
    archive_path: &Path,
    member_path: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    let file = File::open(archive_path).map_err(|error| {
        ArchiveReadError::new(format!(
            "无法打开 TAR 压缩包 {}：{}",
            archive_path.display(),
            error
        ))
    })?;
    let mut archive = TarArchive::new(BufReader::new(file));
    let entries = archive
        .entries()
        .map_err(|error| ArchiveReadError::new(format!("无法读取 TAR 目录：{}", error)))?;

    for entry in entries {
        let mut entry = entry
            .map_err(|error| ArchiveReadError::new(format!("无法读取 TAR 条目：{}", error)))?;
        let entry_path = entry
            .path()
            .map_err(|error| ArchiveReadError::new(format!("无法读取 TAR 条目路径：{}", error)))?;
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
        "TAR 压缩包中未找到成员 {}",
        member_path
    )))
}

/// 从内存 TAR 字节中读取指定成员。
///
/// 业务意图：
/// - 多文件内层 TAR 在左侧树展开后，用户点击具体文件时需要从内存中的内层 tar 字节定位该成员。
pub(super) fn read_tar_member_from_bytes(
    archive_bytes: &[u8],
    member_path: &str,
    label: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    let mut archive = TarArchive::new(Cursor::new(archive_bytes));
    let entries = archive.entries().map_err(|error| {
        ArchiveReadError::new(format!("无法读取嵌套 TAR {} 的目录：{}", label, error))
    })?;

    for entry in entries {
        let mut entry = entry
            .map_err(|error| ArchiveReadError::new(format!("无法读取嵌套 TAR 条目：{}", error)))?;
        let entry_path = entry.path().map_err(|error| {
            ArchiveReadError::new(format!("无法读取嵌套 TAR 条目路径：{}", error))
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
