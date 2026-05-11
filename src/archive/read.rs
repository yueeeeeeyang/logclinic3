//! 压缩包成员读取功能域。
//!
//! 业务意图：
//! - 该模块集中 ZIP、RAR、TAR、TAR.GZ、GZ 和 7Z 的成员读取与“单文件压缩包直读”规则。
//! - 上层日志正文模块只负责把 `LogFileSource` 编排成读取请求，再把原始字节交给编码检测，避免压缩格式分发散落在 UI 或解码代码中。
//!
//! 跨平台约束：
//! - 压缩包内部路径统一使用安全归一化后的 `/` 分隔路径，拒绝绝对路径、盘符和 `..`。
//! - RAR 的读取 API 需要真实文件路径，内存中的嵌套 RAR 会短暂写入系统临时目录，并由调用方在读取后清理。
//! - 7Z solid archive 必须顺序消费非目标条目 reader，不能直接跳过，否则后续目标成员可能校验失败。

use std::{
    error::Error,
    fmt::{self, Display},
    fs::{self, File},
    io::{self, BufReader, Cursor, Read},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use flate2::read::GzDecoder;
use tar::Archive as TarArchive;
use zip::ZipArchive;

use super::{
    ArchiveFormat, is_single_gzip_member_path, normalize_archive_member_path,
    single_gzip_member_display_name, single_gzip_member_path_for_archive,
};

/// 压缩包成员读取或安全校验失败。
///
/// 业务意图：
/// - archive 功能域不依赖日志正文模块，因此用独立错误类型承载中文错误文案。
/// - 上层可以按原有 UI 错误模型把该错误转换成 `LogContentError`，保持用户可见行为不变。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArchiveReadError {
    /// 面向调用方和 UI 的中文错误说明。
    message: String,
}

impl ArchiveReadError {
    /// 创建新的压缩包读取错误。
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for ArchiveReadError {
    /// 将错误格式化为原有 UI 可直接展示的中文文本。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ArchiveReadError {}

/// 单个日志 tab 允许缓存的最大原始字节数。
///
/// 业务意图：
/// - 与日志正文模块保持同一 200MB 内存模式上限，避免压缩包成员绕过普通文件读取边界。
const MAX_LOG_FILE_BYTES: u64 = 200 * 1024 * 1024;

/// 把嵌套 RAR 字节写成临时文件。
///
/// 边界条件：
/// - 文件名只用于诊断和扩展名保留，会做字符级净化，避免压缩包内部路径影响系统临时目录。
/// - 读取完成后调用方负责删除临时文件；异常退出残留会落在系统临时目录下，后续可由系统清理。
pub(crate) fn write_temporary_nested_archive_bytes(
    archive_bytes: &[u8],
    label: &str,
) -> Result<PathBuf, ArchiveReadError> {
    let safe_label = label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ArchiveReadError::new(format!("无法生成嵌套 RAR 临时时间戳：{}", error)))?
        .as_nanos();
    let dir = std::env::temp_dir()
        .join("LogClinic")
        .join("nested-rar-read")
        .join(format!("session-{}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|error| {
        ArchiveReadError::new(format!(
            "无法创建嵌套 RAR 临时目录 {}：{}",
            dir.display(),
            error
        ))
    })?;
    let path = dir.join(format!("nested-{}-{}", unique, safe_label));
    fs::write(&path, archive_bytes).map_err(|error| {
        ArchiveReadError::new(format!(
            "无法写入嵌套 RAR 临时文件 {}：{}",
            path.display(),
            error
        ))
    })?;
    Ok(path)
}

/// 按压缩包格式读取指定成员的原始字节。
///
/// 业务意图：
/// - 格式分发集中在这里，UI 和 tab 状态不需要知道各压缩库 API 差异。
pub(crate) fn read_archive_member(
    archive_path: &Path,
    archive_format: ArchiveFormat,
    member_path: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    let archive_format = if archive_format == ArchiveFormat::TarGz {
        ArchiveFormat::from_file(archive_path).unwrap_or(archive_format)
    } else {
        archive_format
    };
    match archive_format {
        ArchiveFormat::Zip => read_zip_member(archive_path, member_path),
        ArchiveFormat::Rar => read_rar_member(archive_path, member_path),
        ArchiveFormat::Tar => read_tar_member(archive_path, member_path),
        ArchiveFormat::TarGz => read_tar_gz_member(archive_path, member_path),
        ArchiveFormat::Gzip => read_gzip_member(archive_path, member_path),
        ArchiveFormat::SevenZ => read_7z_member(archive_path, member_path),
    }
}

/// 从内存中的压缩包字节读取指定成员。
///
/// 业务意图：
/// - 外层压缩包里的多文件内层压缩包在左侧树中会展开为目录，点击内层文件时需要按“内存中的内层压缩包 + 内层成员路径”读取。
///
/// 边界条件：
/// - ZIP、TAR.GZ、GZ 和 7Z 可以基于内存 reader 读取；RAR 需要文件路径，当前返回清晰错误。
pub(crate) fn read_archive_member_from_bytes(
    archive_bytes: &[u8],
    archive_format: ArchiveFormat,
    member_path: &str,
    label: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    let archive_format = archive_format.resolve_from_bytes(archive_bytes);
    match archive_format {
        ArchiveFormat::Zip => read_zip_member_from_bytes(archive_bytes, member_path, label),
        ArchiveFormat::Tar => read_tar_member_from_bytes(archive_bytes, member_path, label),
        ArchiveFormat::TarGz => read_tar_gz_member_from_bytes(archive_bytes, member_path, label),
        ArchiveFormat::Gzip => read_gzip_member_from_bytes(archive_bytes, member_path, label),
        ArchiveFormat::SevenZ => read_7z_member_from_bytes(archive_bytes, member_path, label),
        ArchiveFormat::Rar => Err(ArchiveReadError::new(format!(
            "嵌套 RAR {} 暂不支持直接从内存读取，请先选择外层解包后的 RAR 文件",
            label
        ))),
    }
}

/// 从本地压缩包文件中读取唯一的普通文件成员。
///
/// 业务意图：
/// - 用户可能在已加载目录树中直接点击 `.zip`、`.rar`、`.tar.gz`、`.gz` 或 `.7z` 文件。
/// - 如果该压缩包内部只有一个文件，直接打开这个文件可以减少一次展开和选择操作。
///
/// 边界条件：
/// - 压缩包为空或包含多个文件时返回明确错误，不把压缩包二进制伪装成日志文本。
/// - 读取仍通过各格式 reader 完成，不把成员写入临时目录。
pub(crate) fn read_single_file_archive_from_path(
    archive_path: &Path,
    archive_format: ArchiveFormat,
) -> Result<Vec<u8>, ArchiveReadError> {
    let label = archive_path.display().to_string();
    let archive_format = if archive_format == ArchiveFormat::TarGz {
        ArchiveFormat::from_file(archive_path).unwrap_or(archive_format)
    } else {
        archive_format
    };
    let member_path =
        single_file_archive_member_path_from_path(archive_path, archive_format, label.as_str())?;
    read_archive_member(archive_path, archive_format, &member_path)
}

/// 从内存中的压缩包字节读取唯一普通文件成员。
///
/// 业务意图：
/// - 外层压缩包里的 `thread_xxx.zip` 这类嵌套压缩包不能落盘解压，也不能写临时文件。
/// - 这里直接基于外层成员的原始字节构造 reader，若内层只有一个文件，就继续读取该文件字节交给编码检测。
///
/// 边界条件：
/// - ZIP、TAR.GZ、GZ 和 7Z 支持从内存 reader 读取；RAR 当前 `unrar` API 需要路径，嵌套 RAR 暂时返回清晰错误。
pub(crate) fn read_single_file_archive_from_bytes(
    archive_bytes: &[u8],
    archive_format: ArchiveFormat,
    label: &str,
    context_label: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    let archive_format = archive_format.resolve_from_bytes(archive_bytes);
    match archive_format {
        ArchiveFormat::Zip => read_single_file_zip_from_bytes(archive_bytes, label),
        ArchiveFormat::Tar => read_single_file_tar_from_bytes(archive_bytes, label),
        ArchiveFormat::TarGz => read_single_file_tar_gz_from_bytes(archive_bytes, label),
        ArchiveFormat::Gzip => read_single_gzip_payload_from_bytes(archive_bytes, label),
        ArchiveFormat::SevenZ => read_single_file_7z_from_bytes(archive_bytes, label),
        ArchiveFormat::Rar => Err(ArchiveReadError::new(format!(
            "{} {} 暂不支持直接从内存读取 RAR，请先选择外层解包后的 RAR 文件",
            context_label, label
        ))),
    }
}

/// 获取本地压缩包中唯一普通文件成员的安全路径。
///
/// 业务意图：
/// - 本地压缩包可以先扫描目录项确认只有一个文件，再复用现有按成员读取的流式实现。
pub(crate) fn single_file_archive_member_path_from_path(
    archive_path: &Path,
    archive_format: ArchiveFormat,
    label: &str,
) -> Result<String, ArchiveReadError> {
    match archive_format {
        ArchiveFormat::Zip => single_file_zip_member_path(archive_path, label),
        ArchiveFormat::Rar => single_file_rar_member_path(archive_path, label),
        ArchiveFormat::Tar => single_file_tar_member_path(archive_path, label),
        ArchiveFormat::TarGz => single_file_tar_gz_member_path(archive_path, label),
        ArchiveFormat::Gzip => single_file_gzip_member_path(archive_path, label),
        ArchiveFormat::SevenZ => single_file_7z_member_path(archive_path, label),
    }
}

/// 从 ZIP 文件目录中找出唯一普通文件成员路径。
fn single_file_zip_member_path(
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

/// 从 RAR 文件目录中找出唯一普通文件成员路径。
fn single_file_rar_member_path(
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

/// 从 TAR 文件目录中找出唯一普通文件成员路径。
///
/// 业务意图：
/// - 支持 `.tar` 文件，以及扩展名误写成 `.tar.gz` 但真实内容为纯 tar 的日志包。
/// - 只返回成员路径，不读取正文；真正读取仍走按成员匹配的流式路径。
fn single_file_tar_member_path(
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

/// 从 TAR.GZ 文件目录中找出唯一普通文件成员路径。
fn single_file_tar_gz_member_path(
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

/// 从 GZIP 文件中返回唯一虚拟成员路径。
///
/// 业务意图：
/// - `.gz` 只包含一个压缩后的日志流，没有真实目录；为了复用压缩包成员读取、tab 去重和分页物化，需要生成稳定虚拟成员路径。
/// - 这里先探测 gzip 层可读性，避免把损坏文件伪装成日志文本。
///
/// 边界条件：
/// - 空 gzip 文件可以作为空日志打开；只要解码器没有返回错误，就认为压缩层有效。
fn single_file_gzip_member_path(
    archive_path: &Path,
    label: &str,
) -> Result<String, ArchiveReadError> {
    if single_gzip_payload_is_readable_from_path(archive_path) {
        return Ok(single_gzip_member_path_for_archive(archive_path));
    }

    Err(ArchiveReadError::new(format!(
        "无法读取 GZIP 日志 {}：文件不是有效 gzip 内容或已损坏",
        label
    )))
}

/// 从 7Z 文件目录中找出唯一普通文件成员路径。
fn single_file_7z_member_path(
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

/// 记录候选唯一成员；出现第二个文件时立即返回多文件错误。
///
/// 业务意图：
/// - 单文件压缩包直接打开只在“恰好一个普通文件”时成立，两个及以上文件必须让用户选择具体文件。
fn remember_single_archive_member(
    single_member: &mut Option<String>,
    candidate: String,
    label: &str,
) -> Result<(), ArchiveReadError> {
    if single_member.replace(candidate).is_some() {
        return Err(ArchiveReadError::new(format!(
            "{} 内包含多个文件，请展开后选择具体日志文件",
            label
        )));
    }
    Ok(())
}

/// 校验扫描结果是否恰好包含一个文件成员。
fn require_single_archive_member(
    single_member: Option<String>,
    label: &str,
) -> Result<String, ArchiveReadError> {
    single_member.ok_or_else(|| {
        ArchiveReadError::new(format!(
            "{} 内没有可打开的普通文件，无法直接显示日志内容",
            label
        ))
    })
}

/// 从内存 ZIP 字节中读取唯一普通文件。
///
/// 业务意图：
/// - 外层压缩包中的 ZIP 成员已经以字节流形式读入内存；这里继续用 `ZipArchive` 解析，不写临时文件。
fn read_single_file_zip_from_bytes(
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

/// 从内存 TAR 字节中读取唯一普通文件。
///
/// 业务意图：
/// - 外层压缩包中的纯 tar 成员可以直接基于内存字节扫描，不需要落盘。
/// - 只有确认内部恰好一个普通文件时才返回正文，避免多文件包误打开第一个文件。
fn read_single_file_tar_from_bytes(
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

/// 从内存 TAR.GZ 字节中读取唯一普通文件。
///
/// 业务意图：
/// - TAR.GZ 是顺序格式，读取第一个普通文件后仍需继续遍历条目，确认不存在第二个文件。
fn read_single_file_tar_gz_from_bytes(
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

/// 从内存 7Z 字节中读取唯一普通文件。
///
/// 业务意图：
/// - `sevenz-rust` 支持 `Read + Seek` reader，因此可以用内存 cursor 处理外层压缩包中的 7Z 成员。
fn read_single_file_7z_from_bytes(
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

/// 从 ZIP 压缩包中读取成员。
///
/// 边界条件：
/// - 不使用 `by_name`，而是遍历并按安全归一化路径匹配，兼容压缩包内部使用反斜杠的情况。
fn read_zip_member(archive_path: &Path, member_path: &str) -> Result<Vec<u8>, ArchiveReadError> {
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

/// 从 RAR 压缩包中读取成员。
///
/// 边界条件：
/// - 加密成员需要密码规则，当前没有密码输入和缓存策略，因此直接返回可读错误。
fn read_rar_member(archive_path: &Path, member_path: &str) -> Result<Vec<u8>, ArchiveReadError> {
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

/// 从 TAR 压缩包中读取成员。
///
/// 业务意图：
/// - 支持纯 `.tar` 归档和扩展名误写成 `.tar.gz` 的纯 tar 日志包。
/// - 读取时顺序跳过非目标条目，找到目标后才把正文读入内存或触发大小上限错误。
fn read_tar_member(archive_path: &Path, member_path: &str) -> Result<Vec<u8>, ArchiveReadError> {
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

/// 从 tar.gz 或 tgz 压缩包中读取成员。
///
/// 业务意图：
/// - gzip 层和 tar 条目遍历都走 reader，不把成员写入临时目录。
fn read_tar_gz_member(archive_path: &Path, member_path: &str) -> Result<Vec<u8>, ArchiveReadError> {
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

/// 从 GZIP 压缩日志中读取唯一虚拟成员。
///
/// 业务意图：
/// - GZIP 不是多文件归档，目录树中的内部文件节点是应用生成的虚拟成员。
/// - 明确校验虚拟成员前缀可以避免调用方把任意路径误当成 gzip 内部文件读取。
fn read_gzip_member(archive_path: &Path, member_path: &str) -> Result<Vec<u8>, ArchiveReadError> {
    if is_single_gzip_member_path(member_path) {
        return read_single_gzip_payload_from_path(archive_path, member_path);
    }

    Err(ArchiveReadError::new(format!(
        "GZIP 日志中未找到虚拟成员：{}",
        member_path
    )))
}

/// 从 7Z 压缩包中读取成员。
///
/// 边界条件：
/// - 7Z 固实压缩可能需要顺序解码前置条目；这里仍通过 `for_each_entries` 流式遍历，不写临时目录。
fn read_7z_member(archive_path: &Path, member_path: &str) -> Result<Vec<u8>, ArchiveReadError> {
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

/// 从内存 ZIP 字节中读取指定成员。
fn read_zip_member_from_bytes(
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

/// 从内存 TAR 字节中读取指定成员。
///
/// 业务意图：
/// - 多文件内层 TAR 在左侧树展开后，用户点击具体文件时需要从内存中的内层 tar 字节定位该成员。
fn read_tar_member_from_bytes(
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

/// 从内存 TAR.GZ 字节中读取指定成员。
fn read_tar_gz_member_from_bytes(
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

/// 从内存 GZIP 字节中读取唯一虚拟成员。
///
/// 业务意图：
/// - 外层压缩包或嵌套压缩包中的 `.gz` 成员会先被读取成字节，再在这里解压成日志正文。
/// - 成员路径必须是加载层生成的虚拟路径，避免把多文件归档成员路径错误套用到单文件 gzip。
fn read_gzip_member_from_bytes(
    archive_bytes: &[u8],
    member_path: &str,
    label: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    if is_single_gzip_member_path(member_path) {
        return read_single_gzip_payload_from_bytes(archive_bytes, member_path);
    }

    Err(ArchiveReadError::new(format!(
        "嵌套 GZIP {} 中未找到虚拟成员：{}",
        label, member_path
    )))
}

/// 从本地 gzip 文件中按“单个 gzip 日志”读取解压内容。
///
/// 业务意图：
/// - 兼容标准 `.gz` 日志，以及扩展名是 `.tar.gz` 但实际没有 tar 目录、只包含一个 gzip 日志流的文件。
/// - 读取结果仍受 200MB 内存模式上限保护；超过阈值的文件会在分页路径中走物化读取。
fn read_single_gzip_payload_from_path(
    archive_path: &Path,
    member_path: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    let file = File::open(archive_path).map_err(|error| {
        ArchiveReadError::new(format!(
            "无法打开 GZIP 日志 {}：{}",
            archive_path.display(),
            error
        ))
    })?;
    let decoder = GzDecoder::new(BufReader::new(file));
    let label = single_gzip_member_display_name(member_path);
    read_reader_to_vec_with_limit(decoder, None, &label)
}

/// 从内存字节中按“单个 gzip 日志”读取解压内容。
///
/// 业务意图：
/// - 外层压缩包内可能包含单文件 gzip 日志并使用 `.gz` 或 `.tar.gz` 命名；该路径不能依赖本地文件 seek。
/// - 使用 `Cursor` 保持读取逻辑纯内存、无临时文件副作用。
fn read_single_gzip_payload_from_bytes(
    archive_bytes: &[u8],
    label: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    let decoder = GzDecoder::new(Cursor::new(archive_bytes));
    let display_label = if is_single_gzip_member_path(label) {
        single_gzip_member_display_name(label)
    } else {
        label.to_string()
    };
    read_reader_to_vec_with_limit(decoder, None, &display_label)
}

/// 判断本地 gzip 层是否可以开始解压。
///
/// 业务意图：
/// - 单文件 gzip 只应处理“gzip 有效”的文件，不能把损坏压缩包误判为普通日志。
fn single_gzip_payload_is_readable_from_path(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut decoder = GzDecoder::new(BufReader::new(file));
    let mut probe = [0_u8; 1];
    decoder.read(&mut probe).is_ok()
}

/// 排空 TAR 条目正文，推进顺序流到下一个条目头。
///
/// 业务意图：
/// - TAR.GZ 是顺序格式；读取目录、查找唯一文件或查找指定成员时，即使当前条目不是目标，也必须消费正文。
/// - 如果不排空，后续条目读取会把文件内容误识别为 tar 头，表现为“条目读取失败”或“路径读取失败”。
///
/// 边界条件：
/// - 该函数只把内容丢到 `io::sink()`，不改变内存上限策略，也不缓存日志正文。
fn drain_tar_entry<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    label: &str,
) -> Result<(), ArchiveReadError> {
    io::copy(entry, &mut io::sink())
        .map(|_| ())
        .map_err(|error| {
            ArchiveReadError::new(format!("跳过 TAR.GZ 条目 {} 失败：{}", label, error))
        })
}

/// 从内存 7Z 字节中读取指定成员。
fn read_7z_member_from_bytes(
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
fn drain_7z_entry_reader(reader: &mut dyn Read) -> Result<(), sevenz_rust::Error> {
    io::copy(reader, &mut io::sink())?;
    Ok(())
}

/// 从任意 reader 读取字节并应用 200MB 上限。
///
/// 业务意图：
/// - 压缩包元数据可能不可信，因此即使已经根据声明大小判断过，也要在真实读取时再做一次保护。
fn read_reader_to_vec_with_limit<R: Read>(
    reader: R,
    expected_size: Option<u64>,
    label: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    if let Some(size) = expected_size {
        ensure_size_within_limit(size, label)?;
    }

    let mut limited_reader = reader.take(MAX_LOG_FILE_BYTES + 1);
    let mut bytes = Vec::new();
    limited_reader
        .read_to_end(&mut bytes)
        .map_err(|error| ArchiveReadError::new(format!("读取日志内容失败：{}", error)))?;
    ensure_buffer_within_limit(bytes.len(), label)?;
    Ok(bytes)
}

/// 检查声明大小是否超过上限。
fn ensure_size_within_limit(size: u64, label: &str) -> Result<(), ArchiveReadError> {
    if size > MAX_LOG_FILE_BYTES {
        return Err(ArchiveReadError::new(format!(
            "{} 超过 200MB 上限，当前第一版暂不打开超大日志",
            label
        )));
    }
    Ok(())
}

/// 检查实际读取到的缓冲区是否超过上限。
fn ensure_buffer_within_limit(size: usize, label: &str) -> Result<(), ArchiveReadError> {
    if size as u64 > MAX_LOG_FILE_BYTES {
        return Err(ArchiveReadError::new(format!(
            "{} 超过 200MB 上限，当前第一版暂不打开超大日志",
            label
        )));
    }
    Ok(())
}
