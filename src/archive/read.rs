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

#[path = "read/gzip_reader.rs"]
mod gzip_reader;
#[path = "read/rar_reader.rs"]
mod rar_reader;
#[path = "read/sevenz_reader.rs"]
mod sevenz_reader;
#[path = "read/tar_gz_reader.rs"]
mod tar_gz_reader;
#[path = "read/tar_reader.rs"]
mod tar_reader;
#[path = "read/zip_reader.rs"]
mod zip_reader;
use self::gzip_reader::*;
use self::rar_reader::*;
use self::sevenz_reader::*;
use self::tar_gz_reader::*;
use self::tar_reader::*;
use self::zip_reader::*;

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
