//! 7Z 成员读取与单文件识别。
//!
//! 业务意图：
//! - 从压缩包成员读取父模块拆出具体格式 reader，避免 ZIP/RAR/TAR/GZIP/7Z 的库调用细节堆在同一个文件。
//! - 函数签名、错误文案、读取上限和成员路径安全校验保持原样，父模块仍负责统一格式分发。

use super::*;

/// 从 7Z 文件目录中找出唯一普通文件成员路径。
pub(super) fn single_file_7z_member_path(
    archive_path: &Path,
    label: &str,
) -> Result<String, ArchiveReadError> {
    let archive = sevenz_rust::Archive::open(archive_path)
        .map_err(|error| ArchiveReadError::new(format!("无法读取 7Z 目录：{}", error)))?;
    let mut single_member = None;

    for entry in archive.files {
        if entry.is_directory() {
            continue;
        }

        let normalized = normalize_archive_member_path(&entry.name).map_err(|reason| {
            ArchiveReadError::new(format!("{} 内包含非法 7Z 条目：{}", label, reason))
        })?;
        remember_single_archive_member(&mut single_member, normalized, label)?;
    }

    require_single_archive_member(single_member, label)
}

/// 从内存 7Z 字节中读取唯一普通文件。
///
/// 业务意图：
/// - `sevenz-rust` 支持 `Read + Seek` reader，因此可以用内存 cursor 处理外层压缩包中的 7Z 成员。
pub(super) fn read_single_file_7z_from_bytes(
    archive_bytes: &[u8],
    label: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    let cursor = Cursor::new(archive_bytes);
    let mut reader = sevenz_rust::SevenZReader::new(
        cursor,
        archive_bytes.len() as u64,
        sevenz_rust::Password::empty(),
    )
    .map_err(|error| ArchiveReadError::new(format!("无法读取嵌套 7Z 目录：{}", error)))?;
    let mut single_member = None;
    let mut result: Option<Result<Vec<u8>, ArchiveReadError>> = None;

    reader
        .for_each_entries(|entry, entry_reader| {
            if entry.is_directory() {
                return Ok(true);
            }

            let normalized = match normalize_archive_member_path(entry.name()) {
                Ok(path) => path,
                Err(reason) => {
                    result = Some(Err(ArchiveReadError::new(format!(
                        "{} 内包含非法 7Z 条目：{}",
                        label, reason
                    ))));
                    return Ok(false);
                }
            };
            if let Err(error) =
                remember_single_archive_member(&mut single_member, normalized.clone(), label)
            {
                result = Some(Err(error));
                return Ok(false);
            }

            result = Some(
                ensure_size_within_limit(entry.size, &normalized).and_then(|_| {
                    read_reader_to_vec_with_limit(entry_reader, Some(entry.size), &normalized)
                }),
            );
            Ok(true)
        })
        .map_err(|error| ArchiveReadError::new(format!("读取嵌套 7Z 日志文件失败：{}", error)))?;

    match result {
        Some(Ok(bytes)) => {
            require_single_archive_member(single_member, label)?;
            Ok(bytes)
        }
        Some(Err(error)) => Err(error),
        None => Err(ArchiveReadError::new(format!(
            "{} 内没有可打开的普通文件，无法直接显示日志内容",
            label
        ))),
    }
}

/// 从 7Z 压缩包中读取成员。
///
/// 边界条件：
/// - 7Z 固实压缩可能需要顺序解码前置条目；这里仍通过 `for_each_entries` 流式遍历，不写临时目录。
pub(super) fn read_7z_member(
    archive_path: &Path,
    member_path: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    let mut reader = sevenz_rust::SevenZReader::open(archive_path, sevenz_rust::Password::empty())
        .map_err(|error| ArchiveReadError::new(format!("无法打开 7Z 压缩包：{}", error)))?;
    let mut result: Option<Result<Vec<u8>, ArchiveReadError>> = None;

    reader
        .for_each_entries(|entry, entry_reader| {
            if entry.is_directory()
                || normalize_archive_member_path(entry.name()).ok().as_deref() != Some(member_path)
            {
                drain_7z_entry_reader(entry_reader)?;
                return Ok(true);
            }

            let member_result = ensure_size_within_limit(entry.size, member_path).and_then(|_| {
                read_reader_to_vec_with_limit(entry_reader, Some(entry.size), member_path)
            });
            result = Some(member_result);
            Ok(false)
        })
        .map_err(|error| ArchiveReadError::new(format!("读取 7Z 日志文件失败：{}", error)))?;

    match result {
        Some(result) => result,
        None => Err(ArchiveReadError::new(format!(
            "压缩包中未找到日志文件：{}",
            member_path
        ))),
    }
}

/// 从内存 7Z 字节中读取指定成员。
pub(super) fn read_7z_member_from_bytes(
    archive_bytes: &[u8],
    member_path: &str,
    label: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    let cursor = Cursor::new(archive_bytes);
    let mut reader = sevenz_rust::SevenZReader::new(
        cursor,
        archive_bytes.len() as u64,
        sevenz_rust::Password::empty(),
    )
    .map_err(|error| {
        ArchiveReadError::new(format!("无法读取嵌套 7Z {} 的目录：{}", label, error))
    })?;
    let mut result: Option<Result<Vec<u8>, ArchiveReadError>> = None;

    reader
        .for_each_entries(|entry, entry_reader| {
            if entry.is_directory()
                || normalize_archive_member_path(entry.name()).ok().as_deref() != Some(member_path)
            {
                drain_7z_entry_reader(entry_reader)?;
                return Ok(true);
            }

            result = Some(
                ensure_size_within_limit(entry.size, member_path).and_then(|_| {
                    read_reader_to_vec_with_limit(entry_reader, Some(entry.size), member_path)
                }),
            );
            Ok(false)
        })
        .map_err(|error| ArchiveReadError::new(format!("读取嵌套 7Z 日志文件失败：{}", error)))?;

    result.unwrap_or_else(|| {
        Err(ArchiveReadError::new(format!(
            "嵌套压缩包 {} 中未找到日志文件：{}",
            label, member_path
        )))
    })
}

/// 消费 7Z 当前条目的 reader。
///
/// 业务意图：
/// - `sevenz-rust` 在处理 solid archive 时要求顺序消费目标条目前面的数据，不能直接跳过 reader。
/// - 如果不 drain 非目标条目，后续读取目标日志可能触发 `ChecksumVerificationFailed`。
pub(super) fn drain_7z_entry_reader(reader: &mut dyn Read) -> Result<(), sevenz_rust::Error> {
    io::copy(reader, &mut io::sink())?;
    Ok(())
}
