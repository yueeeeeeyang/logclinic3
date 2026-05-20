//! 日志正文读取、编码识别和只读显示数据准备模块。
//!
//! 业务意图：
//! - 该模块负责把左侧目录树提供的 `LogFileSource` 读取成原始字节，并把字节解码成右侧查看器可渲染的行。
//! - 原始字节会由 UI tab 保留在内存中，用户切换编码时只重新解码，不重新读取文件或压缩包。
//! - 压缩包成员优先通过对应库的 reader 流式读取到内存；RAR 嵌套成员受底层 API 限制需要短暂写入临时文件，并在读取后立即清理。
//!
//! 关键约束：
//! - 单个 tab 的原始字节上限为 200MB，超限时立即返回友好错误，避免桌面 UI 因超大日志耗尽内存。
//! - 自动检测只接受 UTF-8、UTF-8 BOM、GBK、GB18030 和 Big5；检测到其它编码时要求用户手动选择。
//! - 自动检测不会把含替换字符的解码结果当作成功，避免把明显乱码伪装成正常打开。

use std::{
    error::Error,
    fmt::{self, Display},
    fs::{self, File},
    io::{self, BufReader, Write},
    path::Path,
    sync::Arc,
};

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::{BIG5, Encoding, GB18030, GBK, UTF_8};

use crate::highlighting::{
    HighlightMode, PrecomputedHighlights, SyntaxTheme, prepare_highlighting,
};
use crate::{
    archive::{
        ArchiveFormat, ArchiveReadError, is_single_gzip_member_path, read_archive_member,
        read_archive_member_from_bytes, read_single_file_archive_from_bytes,
        read_single_file_archive_from_path, stream_archive_member_from_bytes_to_writer,
        stream_archive_member_to_writer, stream_single_file_archive_from_bytes_to_writer,
        stream_single_file_archive_from_path_to_writer, write_temporary_nested_archive_bytes,
    },
    log_source::LogFileSource,
};

/// 单个日志 tab 允许缓存的最大原始字节数。
///
/// 业务意图：
/// - 用户已确认第一版采用 200MB 上限，在支持编码切换的同时保证 UI 响应和内存占用可控。
/// - 该限制同时适用于普通文件和压缩包内部成员，避免压缩包内超大文件绕过边界。
///
/// 边界条件：
/// - 上限只限制当前“整文件入内存查看”的第一版实现，不代表未来不能引入分块索引或 mmap。
pub const MAX_LOG_FILE_BYTES: u64 = 200 * 1024 * 1024;

/// UI 编码选择器中的选项。
///
/// 业务意图：
/// - “自动”表示由检测流程决定实际编码；其它选项表示用户手动指定编码。
/// - 手动指定编码时必须复用 tab 内保存的原始字节重新解码，满足“保留原始字节”的需求。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodingChoice {
    /// 默认自动检测。
    Auto,
    /// 用户显式指定某种编码。
    Manual(LogTextEncoding),
}

impl EncodingChoice {
    /// 返回 UI 控件展示文案。
    ///
    /// 边界条件：
    /// - 文案只用于当前中文界面，不作为序列化或持久化格式。
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "自动",
            Self::Manual(encoding) => encoding.label(),
        }
    }
}

/// 日志查看器第一版明确支持的文本编码。
///
/// 业务意图：
/// - 用户明确要求支持 UTF-8、UTF-8 BOM、GBK、GB18030 和 Big5。
/// - 枚举让 UI、检测流程和解码流程共享同一个受控集合，避免界面出现底层不能真正解码的选项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogTextEncoding {
    /// 不带 BOM 的 UTF-8。
    Utf8,
    /// 带 UTF-8 BOM 的文本；解码时会忽略开头 BOM。
    Utf8Bom,
    /// 简体中文常见 GBK 编码。
    Gbk,
    /// GB18030 编码，覆盖 GBK 无法表达的部分字符。
    Gb18030,
    /// 繁体中文常见 Big5 编码。
    Big5,
}

impl LogTextEncoding {
    /// 返回所有手动编码选项。
    ///
    /// 业务意图：
    /// - UI 编码切换控件按该顺序渲染，保证各处选项一致。
    pub const fn manual_options() -> [Self; 5] {
        [
            Self::Utf8,
            Self::Utf8Bom,
            Self::Gbk,
            Self::Gb18030,
            Self::Big5,
        ]
    }

    /// 返回 UI 展示文案。
    pub fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf8Bom => "UTF-8 BOM",
            Self::Gbk => "GBK",
            Self::Gb18030 => "GB18030",
            Self::Big5 => "Big5",
        }
    }

    /// 返回对应的 `encoding_rs` 解码器。
    ///
    /// 边界条件：
    /// - UTF-8 BOM 和 UTF-8 使用同一个底层 UTF-8 解码器；BOM 的处理在调用方按字节前缀完成。
    pub(crate) fn encoding(self) -> &'static Encoding {
        match self {
            Self::Utf8 | Self::Utf8Bom => UTF_8,
            Self::Gbk => GBK,
            Self::Gb18030 => GB18030,
            Self::Big5 => BIG5,
        }
    }
}

/// 已解码、可供右侧只读查看器渲染的日志文档。
///
/// 业务意图：
/// - UI 不直接处理编码库返回值，只消费行列表、实际编码和警告信息。
/// - 行列表按固定行高交给 GPUI `uniform_list` 渲染，支撑大文件滚动性能。
#[derive(Clone, Debug)]
pub struct DecodedLogDocument {
    /// 实际用于解码的编码。
    pub encoding: LogTextEncoding,
    /// 当前结果是否来自自动检测。
    pub detected_automatically: bool,
    /// 解码后的行文本。
    ///
    /// 边界条件：
    /// - 行尾 `\n` 会被移除，Windows 行尾中的 `\r` 也会被去掉，便于右侧按行渲染。
    /// - 行集合可能来自较大的内存模式日志，使用 `Arc` 共享给当前文件搜索任务，避免 UI 线程复制整份日志。
    pub lines: Arc<Vec<String>>,
    /// 字符数量最多的日志行下标，用于右侧虚拟列表测量横向内容宽度。
    ///
    /// 业务意图：
    /// - GPUI `uniform_list` 只测量一个代表行来推导横向滚动宽度，因此需要稳定选择最长候选行。
    /// - 该值在解码阶段一次性计算并随文档缓存，避免滚动、悬浮或拖动滚动条时反复扫描整份日志。
    ///
    /// 边界条件：
    /// - 空内容会被 `split_decoded_lines` 规范成一行空字符串，因此这里始终可以安全返回 0。
    /// - 字符数量不是精确像素宽度，但日志正文使用等宽字体时足以作为横向测量样本。
    pub longest_line_index: usize,
    /// 当前文档使用的高亮模式。
    ///
    /// 业务意图：
    /// - 高亮模式在解码阶段根据文件名和内容一次性识别，虚拟列表渲染当前行时不再重复推断。
    /// - 编码切换后会重新生成该字段，避免错误编码下的内容继续使用旧高亮结果。
    pub highlight_mode: HighlightMode,
    /// XML/properties 小文件的预计算高亮。
    ///
    /// 业务意图：
    /// - Tree-sitter 需要整文件语法树才能准确高亮标签、属性和 properties key/value。
    /// - 大文件会保持 `None` 并降级为轻量规则，避免打开和滚动性能受影响。
    pub precomputed_highlights: Option<Arc<PrecomputedHighlights>>,
    /// 解码过程中是否出现替换字符。
    ///
    /// 业务意图：
    /// - 手动选择错误编码时仍允许展示内容，但通过警告提示用户当前编码可能不正确。
    pub had_replacements: bool,
    /// 面向用户的中文提示。
    pub warning: Option<String>,
}

impl DecodedLogDocument {
    /// 返回当前文档行数。
    ///
    /// 业务意图：
    /// - UI 标题和虚拟列表都需要使用行数，但不应重复访问内部 Vec 的语义。
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
}

/// 日志内容读取或解码失败。
///
/// 业务意图：
/// - 失败文案直接面向 UI，保持中文且包含具体来源上下文。
/// - 错误类型保持轻量可克隆，方便跨后台任务返回到 GPUI 主线程。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogContentError {
    /// 中文错误说明。
    message: String,
}

impl LogContentError {
    /// 创建新的日志内容错误。
    ///
    /// 边界条件：
    /// - 当前不保留底层错误对象，避免跨线程生命周期和错误类型膨胀；底层错误信息会被格式化进中文文案。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for LogContentError {
    /// 将错误格式化为 UI 可直接展示的中文文本。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for LogContentError {}

impl From<ArchiveReadError> for LogContentError {
    /// 将压缩包功能域错误转换为日志正文错误。
    ///
    /// 业务意图：
    /// - UI 仍只处理 `LogContentError`，避免 archive 模块迁移改变外层错误流转。
    /// - archive 错误已经是中文用户可读文案，因此这里只保留原文，不重新包装前缀。
    fn from(error: ArchiveReadError) -> Self {
        Self::new(error.to_string())
    }
}

/// 读取日志来源的原始字节。
///
/// 业务意图：
/// - 读取阶段只负责拿到原始字节，不做编码检测，保证编码切换可以复用同一份字节。
/// - 普通文件和压缩包成员统一返回 `Arc<Vec<u8>>`，便于 UI tab 在后台任务和主线程之间共享。
///
/// 边界条件：
/// - 超过 200MB 的来源不会继续读取。
/// - 压缩包成员按安全归一化路径匹配，避免原始分隔符差异导致树上能看到但打开失败。
pub fn read_log_source_bytes(source: &LogFileSource) -> Result<Arc<Vec<u8>>, LogContentError> {
    if let Some(format) = archive_format_for_source(source) {
        let bytes = match source {
            LogFileSource::LocalFile { path } => read_single_file_archive_from_path(path, format)?,
            LogFileSource::ArchiveMember {
                archive_path,
                archive_format,
                member_path,
            } => {
                let archive_bytes =
                    read_archive_member(archive_path, *archive_format, member_path)?;
                read_single_file_archive_from_bytes(
                    &archive_bytes,
                    format,
                    member_path,
                    "压缩包内嵌套压缩包",
                )?
            }
            LogFileSource::MaterializedArchiveMember {
                member_path,
                temp_path,
                ..
            } => read_single_file_archive_from_path(temp_path, format).map_err(|error| {
                LogContentError::new(format!("读取 7Z 物化成员 {} 失败：{}", member_path, error))
            })?,
            LogFileSource::NestedArchiveMember {
                outer_archive_path,
                outer_archive_format,
                archive_member_path,
                nested_archive_format,
                nested_member_path,
            } => {
                let nested_bytes = read_nested_archive_member_bytes(
                    outer_archive_path,
                    *outer_archive_format,
                    archive_member_path,
                    *nested_archive_format,
                    nested_member_path,
                )?;
                if format == ArchiveFormat::Rar {
                    let temp_path =
                        write_temporary_nested_archive_bytes(&nested_bytes, nested_member_path)?;
                    let result = read_single_file_archive_from_path(&temp_path, format);
                    let _ = fs::remove_file(&temp_path);
                    result?
                } else {
                    read_single_file_archive_from_bytes(
                        &nested_bytes,
                        format,
                        nested_member_path,
                        "嵌套压缩包内再次嵌套压缩包",
                    )?
                }
            }
        };

        return Ok(Arc::new(bytes));
    }

    let bytes = match source {
        LogFileSource::LocalFile { path } => read_local_file(path)?,
        LogFileSource::ArchiveMember {
            archive_path,
            archive_format,
            member_path,
        } => read_archive_member(archive_path, *archive_format, member_path)?,
        LogFileSource::MaterializedArchiveMember { temp_path, .. } => read_local_file(temp_path)?,
        LogFileSource::NestedArchiveMember {
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_archive_format,
            nested_member_path,
        } => read_nested_archive_member_bytes(
            outer_archive_path,
            *outer_archive_format,
            archive_member_path,
            *nested_archive_format,
            nested_member_path,
        )?,
    };

    Ok(Arc::new(bytes))
}

/// 将日志来源的原始字节直接写入目标 writer。
///
/// 业务意图：
/// - 插件分析、SQL 下钻等后台任务只需要顺序消费日志正文，不应沿用右侧日志 tab 的“整文件读入内存”模式。
/// - 该函数保持与 `read_log_source_bytes` 相同的来源语义：普通文件读自身，压缩包成员读成员内容，压缩包文件本身则读取内部唯一普通文件。
///
/// 关键约束：
/// - 该入口不缓存原始字节，也不应用 200MB tab 内存上限；调用方必须在后台线程使用，并通过进度或超时给用户反馈。
/// - 嵌套压缩包仍需要先取得内层压缩包字节才能解析目录；但目标日志成员会直接写入 writer，不再为插件生成临时日志文件。
pub fn stream_log_source_bytes_to_writer<W: Write + ?Sized>(
    source: &LogFileSource,
    writer: &mut W,
) -> Result<(), LogContentError> {
    if let Some(format) = archive_format_for_source(source) {
        match source {
            LogFileSource::LocalFile { path } => {
                return Ok(stream_single_file_archive_from_path_to_writer(
                    path, format, writer,
                )?);
            }
            LogFileSource::ArchiveMember {
                archive_path,
                archive_format,
                member_path,
            } => {
                let archive_bytes =
                    read_archive_member(archive_path, *archive_format, member_path)?;
                return Ok(stream_single_file_archive_from_bytes_to_writer(
                    &archive_bytes,
                    format,
                    member_path,
                    "压缩包内嵌套压缩包",
                    writer,
                )?);
            }
            LogFileSource::MaterializedArchiveMember {
                member_path,
                temp_path,
                ..
            } => {
                return stream_single_file_archive_from_path_to_writer(temp_path, format, writer)
                    .map_err(|error| {
                        LogContentError::new(format!(
                            "读取 7Z 物化成员 {} 失败：{}",
                            member_path, error
                        ))
                    });
            }
            LogFileSource::NestedArchiveMember {
                outer_archive_path,
                outer_archive_format,
                archive_member_path,
                nested_archive_format,
                nested_member_path,
            } => {
                let nested_bytes = read_nested_archive_member_bytes(
                    outer_archive_path,
                    *outer_archive_format,
                    archive_member_path,
                    *nested_archive_format,
                    nested_member_path,
                )?;
                if format == ArchiveFormat::Rar {
                    let temp_path =
                        write_temporary_nested_archive_bytes(&nested_bytes, nested_member_path)?;
                    let result =
                        stream_single_file_archive_from_path_to_writer(&temp_path, format, writer);
                    let _ = fs::remove_file(&temp_path);
                    return Ok(result?);
                }
                return Ok(stream_single_file_archive_from_bytes_to_writer(
                    &nested_bytes,
                    format,
                    nested_member_path,
                    "嵌套压缩包内再次嵌套压缩包",
                    writer,
                )?);
            }
        }
    }

    match source {
        LogFileSource::LocalFile { path } => stream_local_file_to_writer(path, writer),
        LogFileSource::ArchiveMember {
            archive_path,
            archive_format,
            member_path,
        } => Ok(stream_archive_member_to_writer(
            archive_path,
            *archive_format,
            member_path,
            writer,
        )?),
        LogFileSource::MaterializedArchiveMember { temp_path, .. } => {
            stream_local_file_to_writer(temp_path, writer)
        }
        LogFileSource::NestedArchiveMember {
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_archive_format,
            nested_member_path,
        } => stream_nested_archive_member_to_writer(
            outer_archive_path,
            *outer_archive_format,
            archive_member_path,
            *nested_archive_format,
            nested_member_path,
            writer,
        ),
    }
}

/// 读取外层压缩包中的内层压缩包成员。
///
/// 业务意图：
/// - 多文件内层压缩包在左侧树中作为目录展示，点击具体文件时需要保持“外层压缩包 -> 内层压缩包 -> 文件”的读取语义。
/// - RAR 无法从内存 reader 读取目录或成员，因此只对内层 RAR 写临时文件并在读取后立即删除。
fn read_nested_archive_member_bytes(
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    nested_archive_format: ArchiveFormat,
    nested_member_path: &str,
) -> Result<Vec<u8>, LogContentError> {
    let archive_bytes = read_archive_member(
        outer_archive_path,
        outer_archive_format,
        archive_member_path,
    )?;
    if nested_archive_format == ArchiveFormat::Rar {
        let temp_path = write_temporary_nested_archive_bytes(&archive_bytes, archive_member_path)?;
        let result = read_archive_member(&temp_path, nested_archive_format, nested_member_path);
        let _ = fs::remove_file(&temp_path);
        Ok(result?)
    } else {
        Ok(read_archive_member_from_bytes(
            &archive_bytes,
            nested_archive_format,
            nested_member_path,
            archive_member_path,
        )?)
    }
}

/// 将外层压缩包中的内层压缩包目标成员直接写入 writer。
///
/// 业务意图：
/// - 嵌套压缩包无法仅凭外层顺序流定位内层目录；需要先取得内层压缩包字节。
/// - 取得内层压缩包后，目标日志成员仍通过各格式 reader 写入 writer，避免再把目标日志正文整体读入内存。
fn stream_nested_archive_member_to_writer<W: Write + ?Sized>(
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    nested_archive_format: ArchiveFormat,
    nested_member_path: &str,
    writer: &mut W,
) -> Result<(), LogContentError> {
    let archive_bytes = read_archive_member(
        outer_archive_path,
        outer_archive_format,
        archive_member_path,
    )?;
    if nested_archive_format == ArchiveFormat::Rar {
        let temp_path = write_temporary_nested_archive_bytes(&archive_bytes, archive_member_path)?;
        let result = stream_archive_member_to_writer(
            &temp_path,
            nested_archive_format,
            nested_member_path,
            writer,
        );
        let _ = fs::remove_file(&temp_path);
        Ok(result?)
    } else {
        Ok(stream_archive_member_from_bytes_to_writer(
            &archive_bytes,
            nested_archive_format,
            nested_member_path,
            archive_member_path,
            writer,
        )?)
    }
}

/// 判断被点击的日志来源本身是否也是一个受支持压缩包。
///
/// 业务意图：
/// - 左侧树可能来自目录或外层压缩包，里面的 `thread_xxx.zip` 这类条目会被加载层当作普通文件节点展示。
/// - 点击这种文件时不能把 ZIP/RAR/GZ/7Z 二进制直接交给编码检测，而应该先按压缩包规则读取其内部唯一文件。
///
/// 边界条件：
/// - 这里只根据文件名扩展名判断格式，和加载层 `ArchiveFormat::from_path` 保持一致。
fn archive_format_for_source(source: &LogFileSource) -> Option<ArchiveFormat> {
    match source {
        LogFileSource::LocalFile { path } => ArchiveFormat::from_file(path),
        LogFileSource::ArchiveMember { member_path, .. } => {
            if is_single_gzip_member_path(member_path) {
                return None;
            }
            ArchiveFormat::from_path(Path::new(member_path))
        }
        LogFileSource::MaterializedArchiveMember {
            member_path,
            temp_path,
            ..
        } => {
            if is_single_gzip_member_path(member_path) {
                return None;
            }
            ArchiveFormat::from_file(temp_path)
                .or_else(|| ArchiveFormat::from_path(Path::new(member_path)))
        }
        LogFileSource::NestedArchiveMember {
            nested_member_path, ..
        } => ArchiveFormat::from_path(Path::new(nested_member_path)),
    }
}

/// 将原始字节按指定策略解码成日志文档。
///
/// 业务意图：
/// - 自动检测失败时返回错误，UI 保留原始字节并提示用户手动选择编码。
/// - 手动编码即使出现替换字符也返回文档和警告，让用户可以自行判断是否继续切换。
pub fn decode_log_bytes(
    raw_bytes: &[u8],
    choice: EncodingChoice,
    source_name: &str,
) -> Result<DecodedLogDocument, LogContentError> {
    match choice {
        EncodingChoice::Auto => {
            let encoding = detect_log_encoding(raw_bytes)?;
            decode_with_encoding(raw_bytes, encoding, true, source_name)
        }
        EncodingChoice::Manual(encoding) => {
            decode_with_encoding(raw_bytes, encoding, false, source_name)
        }
    }
}

/// 读取本地普通日志文件。
///
/// 业务意图：
/// - 先读取元数据判断大小，避免直接 `fs::read` 把超大文件一次性读入内存。
fn read_local_file(path: &Path) -> Result<Vec<u8>, LogContentError> {
    let metadata = fs::metadata(path).map_err(|error| {
        LogContentError::new(format!(
            "无法读取文件 {} 的元数据：{}",
            path.display(),
            error
        ))
    })?;
    ensure_size_within_limit(metadata.len(), &path.display().to_string())?;
    fs::read(path).map_err(|error| {
        LogContentError::new(format!("无法读取文件 {}：{}", path.display(), error))
    })
}

/// 将本地文件内容顺序复制到目标 writer。
///
/// 业务意图：
/// - 插件后台分析可以处理远大于右侧 tab 内存模式的日志文件，因此这里不使用 `fs::read`。
/// - 复制过程在后台线程执行，调用方负责把 writer 设计成按行消费并汇报进度。
fn stream_local_file_to_writer<W: Write + ?Sized>(
    path: &Path,
    writer: &mut W,
) -> Result<(), LogContentError> {
    let file = File::open(path).map_err(|error| {
        LogContentError::new(format!("无法打开日志文件 {}：{}", path.display(), error))
    })?;
    let mut reader = BufReader::new(file);
    io::copy(&mut reader, writer).map(|_| ()).map_err(|error| {
        LogContentError::new(format!(
            "流式读取日志文件 {} 失败：{}",
            path.display(),
            error
        ))
    })
}

/// 检查声明大小是否超过上限。
fn ensure_size_within_limit(size: u64, label: &str) -> Result<(), LogContentError> {
    if size > MAX_LOG_FILE_BYTES {
        return Err(LogContentError::new(format!(
            "{} 超过 200MB 上限，当前第一版暂不打开超大日志",
            label
        )));
    }
    Ok(())
}

/// 自动识别日志编码。
///
/// 业务意图：
/// - 优先使用确定性更强的 BOM 和 UTF-8 校验，再使用 chardetng 的统计检测。
/// - 自动检测只返回支持集合内的编码，并且必须可以无替换字符解码。
pub(crate) fn detect_log_encoding(raw_bytes: &[u8]) -> Result<LogTextEncoding, LogContentError> {
    if raw_bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Ok(LogTextEncoding::Utf8Bom);
    }

    if decode_lossy(raw_bytes, LogTextEncoding::Utf8).is_ok_and(|decoded| !decoded.had_errors) {
        return Ok(LogTextEncoding::Utf8);
    }

    let mut detector = EncodingDetector::new(Iso2022JpDetection::Deny);
    detector.feed(raw_bytes, true);
    let guessed = detector.guess(None, Utf8Detection::Allow);

    if let Some(candidate) = encoding_from_detector_guess(guessed) {
        // GBK 与 GB18030 在现代解码器里存在兼容重叠；一旦发现 GB18030 独有的四字节序列，
        // 自动检测必须优先选择 GB18030，否则状态栏会误导用户以为文件是更窄的 GBK 编码。
        if candidate == LogTextEncoding::Gbk
            && contains_gb18030_four_byte_sequence(raw_bytes)
            && decode_lossy(raw_bytes, LogTextEncoding::Gb18030)
                .is_ok_and(|decoded| !decoded.had_errors)
        {
            return Ok(LogTextEncoding::Gb18030);
        }

        if candidate == LogTextEncoding::Gbk
            && decode_lossy(raw_bytes, LogTextEncoding::Gbk).is_ok_and(|decoded| decoded.had_errors)
            && decode_lossy(raw_bytes, LogTextEncoding::Gb18030)
                .is_ok_and(|decoded| !decoded.had_errors)
        {
            return Ok(LogTextEncoding::Gb18030);
        }

        if decode_lossy(raw_bytes, candidate).is_ok_and(|decoded| !decoded.had_errors) {
            return Ok(candidate);
        }
    }

    for candidate in [
        LogTextEncoding::Gb18030,
        LogTextEncoding::Gbk,
        LogTextEncoding::Big5,
    ] {
        if decode_lossy(raw_bytes, candidate).is_ok_and(|decoded| !decoded.had_errors) {
            return Ok(candidate);
        }
    }

    Err(LogContentError::new(
        "无法可靠识别日志编码，请手动选择 UTF-8、GBK、GB18030 或 Big5 后重新解析",
    ))
}

/// 判断字节流中是否包含 GB18030 独有的四字节编码形态。
///
/// 业务意图：
/// - GB18030 兼容 GBK 的双字节区间，单靠“能否无损解码”无法可靠区分两者。
/// - 四字节序列形态 `81-FE 30-39 81-FE 30-39` 是 GB18030 扩展字符的强信号，
///   发现后应把自动识别结果提升为 GB18030。
///
/// 边界条件：
/// - 该函数只作为编码自动检测的启发式信号，不直接解码内容；最终仍需通过 GB18030 解码确认无替换字符。
fn contains_gb18030_four_byte_sequence(raw_bytes: &[u8]) -> bool {
    raw_bytes.windows(4).any(|window| {
        matches!(window[0], 0x81..=0xFE)
            && matches!(window[1], 0x30..=0x39)
            && matches!(window[2], 0x81..=0xFE)
            && matches!(window[3], 0x30..=0x39)
    })
}

/// 将 chardetng 的猜测结果收敛到本产品支持的编码集合。
///
/// 边界条件：
/// - chardetng 会返回更多 Web 编码；第一版只接受用户明确要求的中文日志相关编码。
fn encoding_from_detector_guess(encoding: &'static Encoding) -> Option<LogTextEncoding> {
    if encoding == UTF_8 {
        Some(LogTextEncoding::Utf8)
    } else if encoding == GBK {
        Some(LogTextEncoding::Gbk)
    } else if encoding == GB18030 {
        Some(LogTextEncoding::Gb18030)
    } else if encoding == BIG5 {
        Some(LogTextEncoding::Big5)
    } else {
        None
    }
}

/// 使用指定编码解码并构建日志文档。
///
/// 业务意图：
/// - 自动检测路径要求无替换字符；手动路径允许带警告返回，便于用户继续试其它编码。
fn decode_with_encoding(
    raw_bytes: &[u8],
    encoding: LogTextEncoding,
    detected_automatically: bool,
    source_name: &str,
) -> Result<DecodedLogDocument, LogContentError> {
    let decoded = decode_lossy(raw_bytes, encoding)?;
    if detected_automatically && decoded.had_errors {
        return Err(LogContentError::new(
            "自动识别出的编码无法无损解码，请手动切换编码重新解析",
        ));
    }

    let warning = decoded.had_errors.then(|| {
        format!(
            "当前按 {} 解码时出现替换字符，内容可能存在乱码，可尝试切换其它编码",
            encoding.label()
        )
    });

    let lines = split_decoded_lines(&decoded.text);
    let longest_line_index = longest_log_line_index(&lines);
    // 解码发生在后台任务中，无法读取当前窗口主题；这里使用明亮主题预计算作为启动期默认。
    // UI 渲染阶段会根据当前实际主题对可见行重新生成语法样式，避免暗色主题优化破坏明亮主题可读性。
    let highlight_plan = prepare_highlighting(
        source_name,
        &decoded.text,
        &lines,
        raw_bytes.len(),
        SyntaxTheme::Light,
    );

    Ok(DecodedLogDocument {
        encoding,
        detected_automatically,
        lines: Arc::new(lines),
        longest_line_index,
        highlight_mode: highlight_plan.mode,
        precomputed_highlights: highlight_plan.precomputed.map(Arc::new),
        had_replacements: decoded.had_errors,
        warning,
    })
}

/// 单次解码的中间结果。
///
/// 业务意图：
/// - 将解码文本和替换字符标记放在一起，避免检测流程和 UI 流程各自调用编码库导致行为不一致。
pub(crate) struct DecodeAttempt {
    /// 解码后的完整文本。
    pub(crate) text: String,
    /// 是否出现替换字符。
    pub(crate) had_errors: bool,
}

/// 使用指定编码解码原始字节。
///
/// 边界条件：
/// - UTF-8 BOM 选项会去掉开头 BOM；其它编码不做 BOM 特殊处理。
pub(crate) fn decode_lossy(
    raw_bytes: &[u8],
    encoding: LogTextEncoding,
) -> Result<DecodeAttempt, LogContentError> {
    let bytes =
        if encoding == LogTextEncoding::Utf8Bom && raw_bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            &raw_bytes[3..]
        } else {
            raw_bytes
        };
    let (text, had_errors) = encoding.encoding().decode_without_bom_handling(bytes);
    Ok(DecodeAttempt {
        text: text.into_owned(),
        had_errors,
    })
}

/// 将完整文本拆分成 UI 需要的行。
///
/// 业务意图：
/// - 右侧查看器使用固定行高虚拟列表，每个元素对应一行。
/// - 去掉换行符和 Windows 行尾中的 `\r`，避免行内出现不可见控制字符影响高亮和宽度计算。
pub(crate) fn split_decoded_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }

    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect()
}

/// 返回日志正文中字符数量最多的一行下标。
///
/// 业务意图：
/// - 横向滚动条宽度依赖虚拟列表测量到的最宽行；如果每次渲染时重新扫描所有行，大文件滚动会出现明显卡顿。
/// - 解码阶段本来就需要遍历文本拆行，在这里补充一次最长行计算可以把成本限制在打开或切换编码时。
///
/// 边界条件：
/// - 等宽字体下字符数和视觉宽度基本一致；包含宽字符的日志仍可通过横向滚动访问内容，后续如需像素级精度再引入文本测量缓存。
/// - 行列表为空时返回 0，调用方传给 `uniform_list` 时会被 GPUI 按 item 数量保护，不会越界。
pub(crate) fn longest_log_line_index(lines: &[String]) -> usize {
    lines
        .iter()
        .enumerate()
        .max_by_key(|(_index, line)| line.chars().count())
        .map(|(index, _line)| index)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    //! 日志内容模块单元测试。
    //!
    //! 业务意图：
    //! - 覆盖编码检测、手动编码切换和大小上限这些不依赖图形环境的核心规则。
    //! - UI tab 行为由主界面逻辑和手动验收覆盖，本模块只验证读字节和解码数据的正确性。

    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use std::fs::File;
    use std::io::{self, Cursor, Write};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tar::Builder as TarBuilder;

    /// 验证 UTF-8 BOM 会被自动识别并在解码时移除。
    #[test]
    fn 自动识别_utf8_bom() {
        let bytes = b"\xEF\xBB\xBFINFO \xE6\x97\xA5\xE5\xBF\x97".to_vec();
        let document = decode_log_bytes(&bytes, EncodingChoice::Auto, "access.log").unwrap();

        assert_eq!(document.encoding, LogTextEncoding::Utf8Bom);
        assert_eq!(document.lines[0], "INFO 日志");
    }

    /// 验证不带 BOM 的 UTF-8 文本会被自动识别为 UTF-8。
    #[test]
    fn 自动识别_utf8() {
        let bytes = "INFO 普通 UTF-8 日志".as_bytes().to_vec();
        let document = decode_log_bytes(&bytes, EncodingChoice::Auto, "access.log").unwrap();

        assert_eq!(document.encoding, LogTextEncoding::Utf8);
        assert_eq!(document.lines[0], "INFO 普通 UTF-8 日志");
    }

    /// 验证最长行下标会在解码阶段缓存，避免 UI 渲染阶段反复扫描整份日志。
    #[test]
    fn 解码阶段缓存最长行下标() {
        let bytes = "短行\nINFO 这是当前最长的一行\n中等长度"
            .as_bytes()
            .to_vec();
        let document = decode_log_bytes(&bytes, EncodingChoice::Auto, "access.log").unwrap();

        assert_eq!(document.longest_line_index, 1);
    }

    /// 验证 GBK 文本可以通过手动编码正确解码。
    #[test]
    fn 手动_gbk_解码() {
        let (bytes, _, _) = GBK.encode("ERROR 中文日志");
        let document = decode_log_bytes(
            &bytes,
            EncodingChoice::Manual(LogTextEncoding::Gbk),
            "access.log",
        )
        .unwrap();

        assert_eq!(document.lines[0], "ERROR 中文日志");
        assert!(!document.had_replacements);
    }

    /// 验证 GB18030 的四字节序列不会被误判为 GBK。
    #[test]
    fn 自动识别_gb18030_扩展字符() {
        let (bytes, _, _) = GB18030.encode("INFO 𠀀");
        let document = decode_log_bytes(&bytes, EncodingChoice::Auto, "access.log").unwrap();

        assert_eq!(document.encoding, LogTextEncoding::Gb18030);
        assert_eq!(document.lines[0], "INFO 𠀀");
    }

    /// 验证 Big5 文本可以通过手动编码正确解码。
    #[test]
    fn 手动_big5_解码() {
        let (bytes, _, _) = BIG5.encode("WARN 繁體日誌");
        let document = decode_log_bytes(
            &bytes,
            EncodingChoice::Manual(LogTextEncoding::Big5),
            "access.log",
        )
        .unwrap();

        assert_eq!(document.lines[0], "WARN 繁體日誌");
    }

    /// 验证同一份原始字节可以按不同编码重复解码，支撑 UI 中的手动编码切换。
    #[test]
    fn 手动编码切换复用原始字节重新解码() {
        let (bytes, _, _) = GBK.encode("ERROR 中文日志");
        let gbk_document = decode_log_bytes(
            &bytes,
            EncodingChoice::Manual(LogTextEncoding::Gbk),
            "access.log",
        )
        .unwrap();
        let utf8_document = decode_log_bytes(
            &bytes,
            EncodingChoice::Manual(LogTextEncoding::Utf8),
            "access.log",
        )
        .unwrap();

        assert_eq!(gbk_document.lines[0], "ERROR 中文日志");
        assert!(utf8_document.had_replacements);
        assert!(utf8_document.warning.is_some());
    }

    /// 验证普通文件读取会保留原始字节，供后续重新解码。
    #[test]
    fn 读取本地日志原始字节() -> Result<(), Box<dyn Error>> {
        let temp_dir = unique_temp_dir("logclinic3-content-test")?;
        let log_path = temp_dir.join("access.log");
        fs::write(&log_path, b"INFO ok")?;

        let bytes = read_log_source_bytes(&LogFileSource::LocalFile { path: log_path })?;

        assert_eq!(&bytes[..], b"INFO ok");
        fs::remove_dir_all(temp_dir)?;
        Ok(())
    }

    /// 验证压缩包内部成员可以按安全归一化路径读取原始字节。
    #[test]
    fn 读取_zip_压缩包成员原始字节() -> Result<(), Box<dyn Error>> {
        let temp_dir = unique_temp_dir("logclinic3-zip-content-test")?;
        let archive_path = temp_dir.join("logs.zip");
        let archive_file = File::create(&archive_path)?;
        let mut zip_writer = zip::ZipWriter::new(archive_file);

        zip_writer.start_file("logs\\access.log", zip::write::SimpleFileOptions::default())?;
        zip_writer.write_all(b"INFO zip")?;
        zip_writer.finish()?;

        let bytes = read_log_source_bytes(&LogFileSource::ArchiveMember {
            archive_path: archive_path.clone(),
            archive_format: ArchiveFormat::Zip,
            member_path: "logs/access.log".to_string(),
        })?;

        assert_eq!(&bytes[..], b"INFO zip");
        fs::remove_dir_all(temp_dir)?;
        Ok(())
    }

    /// 验证本地日志可以通过 writer 流式读取，供插件后台分析避免整文件缓存。
    ///
    /// 业务意图：
    /// - SQL 下钻需要处理可能很大的日志文件；本地文件路径必须通过顺序复制写出，而不是 `fs::read`。
    #[test]
    fn 流式读取本地日志原始字节() -> Result<(), Box<dyn Error>> {
        let temp_dir = unique_temp_dir("logclinic3-local-stream-content-test")?;
        let log_path = temp_dir.join("access.log");
        fs::write(&log_path, b"INFO one\nINFO two")?;
        let mut streamed = Vec::new();

        stream_log_source_bytes_to_writer(
            &LogFileSource::LocalFile { path: log_path },
            &mut streamed,
        )?;

        assert_eq!(streamed, b"INFO one\nINFO two");
        fs::remove_dir_all(temp_dir)?;
        Ok(())
    }

    /// 验证 ZIP 成员可以直接流式写入 writer，不需要先把目标日志正文读成 `Vec<u8>`。
    ///
    /// 业务意图：
    /// - 插件点击“显示SQL”时可能面对压缩包内日志；该路径应由压缩库 reader 直接推给宿主内容流。
    #[test]
    fn 流式读取_zip_压缩包成员原始字节() -> Result<(), Box<dyn Error>> {
        let temp_dir = unique_temp_dir("logclinic3-zip-stream-content-test")?;
        let archive_path = temp_dir.join("logs.zip");
        let archive_file = File::create(&archive_path)?;
        let mut zip_writer = zip::ZipWriter::new(archive_file);

        zip_writer.start_file("logs\\access.log", zip::write::SimpleFileOptions::default())?;
        zip_writer.write_all(b"INFO zip stream")?;
        zip_writer.finish()?;
        let mut streamed = Vec::new();

        stream_log_source_bytes_to_writer(
            &LogFileSource::ArchiveMember {
                archive_path: archive_path.clone(),
                archive_format: ArchiveFormat::Zip,
                member_path: "logs/access.log".to_string(),
            },
            &mut streamed,
        )?;

        assert_eq!(streamed, b"INFO zip stream");
        fs::remove_dir_all(temp_dir)?;
        Ok(())
    }

    /// 验证读取 TAR.GZ 后续成员前会跳过前置成员正文。
    ///
    /// 业务意图：
    /// - 用户在左侧树点击 TAR.GZ 内第二个及后续文件时，读取逻辑必须顺序排空前面的文件内容。
    /// - 否则 gzip/tar 流会停留在前一个文件正文位置，导致后续成员匹配失败或报条目读取错误。
    #[test]
    fn 读取_tar_gz_后续成员原始字节() -> Result<(), Box<dyn Error>> {
        let temp_dir = unique_temp_dir("logclinic3-tar-gz-content-test")?;
        let archive_path = temp_dir.join("logs.tar.gz");
        write_test_tar_gz(
            &archive_path,
            &[
                ("first.log", b"INFO first".as_slice()),
                ("second.log", b"INFO second".as_slice()),
            ],
        )?;

        let bytes = read_log_source_bytes(&LogFileSource::ArchiveMember {
            archive_path: archive_path.clone(),
            archive_format: ArchiveFormat::TarGz,
            member_path: "second.log".to_string(),
        })?;

        assert_eq!(&bytes[..], b"INFO second");
        fs::remove_dir_all(temp_dir)?;
        Ok(())
    }

    /// 验证扩展名为 `.tar.gz` 的单文件 gzip 日志可以直接读取正文。
    ///
    /// 业务意图：
    /// - 目录树 fallback 会生成虚拟成员路径；单文件直接打开也会先识别唯一虚拟成员，再按 gzip 解压读取。
    /// - 该测试覆盖读取链路，避免 UI 能看到节点但点击后仍报 TAR.GZ 条目读取失败。
    #[test]
    fn 读取_tar_gz_单文件gzip原始字节() -> Result<(), Box<dyn Error>> {
        let temp_dir = unique_temp_dir("logclinic3-single-gzip-content-test")?;
        let archive_path = temp_dir.join("single.tar.gz");
        write_test_gzip(&archive_path, b"INFO gzip only")?;

        let bytes = read_log_source_bytes(&LogFileSource::LocalFile {
            path: archive_path.clone(),
        })?;

        assert_eq!(&bytes[..], b"INFO gzip only");
        fs::remove_dir_all(temp_dir)?;
        Ok(())
    }

    /// 验证标准 `.gz` 日志文件可以直接读取解压后的原始字节。
    ///
    /// 业务意图：
    /// - 用户直接打开 `app.log.gz` 时应看到解压后的日志文本，而不是 gzip 二进制内容。
    /// - 该路径走一等 `ArchiveFormat::Gzip`，覆盖本地文件和目录树单文件快捷打开场景。
    #[test]
    fn 读取_gz_本地压缩日志原始字节() -> Result<(), Box<dyn Error>> {
        let temp_dir = unique_temp_dir("logclinic3-gzip-content-test")?;
        let archive_path = temp_dir.join("app.log.gz");
        write_test_gzip(&archive_path, b"INFO gzip log")?;

        let bytes = read_log_source_bytes(&LogFileSource::LocalFile {
            path: archive_path.clone(),
        })?;

        assert_eq!(&bytes[..], b"INFO gzip log");
        fs::remove_dir_all(temp_dir)?;
        Ok(())
    }

    /// 验证压缩包内部 `.gz` 成员会继续解压为日志正文。
    ///
    /// 业务意图：
    /// - 用户加载 ZIP/RAR/TAR/7Z 等外层包时，内部的 `*.gz` 不应作为二进制文件显示。
    /// - 点击外层包里的 `.gz` 文件节点时，读取链路会先取出 gzip 字节，再按单文件压缩日志解压。
    #[test]
    fn 读取_zip_压缩包内_gz_成员原始字节() -> Result<(), Box<dyn Error>> {
        let temp_dir = unique_temp_dir("logclinic3-nested-gzip-content-test")?;
        let archive_path = temp_dir.join("outer.zip");
        let archive_file = File::create(&archive_path)?;
        let mut zip_writer = zip::ZipWriter::new(archive_file);

        zip_writer.start_file("logs/app.log.gz", zip::write::SimpleFileOptions::default())?;
        zip_writer.write_all(&gzip_test_bytes(b"INFO gzip in zip")?)?;
        zip_writer.finish()?;

        let bytes = read_log_source_bytes(&LogFileSource::ArchiveMember {
            archive_path: archive_path.clone(),
            archive_format: ArchiveFormat::Zip,
            member_path: "logs/app.log.gz".to_string(),
        })?;

        assert_eq!(&bytes[..], b"INFO gzip in zip");
        fs::remove_dir_all(temp_dir)?;
        Ok(())
    }

    /// 验证损坏 `.gz` 会返回压缩读取错误。
    ///
    /// 业务意图：
    /// - 损坏 gzip 文件不能被当作普通日志文本读取，否则 UI 会显示乱码并误导用户切换编码。
    #[test]
    fn 读取损坏_gz_返回错误() -> Result<(), Box<dyn Error>> {
        let temp_dir = unique_temp_dir("logclinic3-bad-gzip-content-test")?;
        let archive_path = temp_dir.join("bad.log.gz");
        fs::write(&archive_path, b"not a gzip stream")?;

        let error = read_log_source_bytes(&LogFileSource::LocalFile {
            path: archive_path.clone(),
        })
        .unwrap_err();

        assert!(error.to_string().contains("GZIP"));
        fs::remove_dir_all(temp_dir)?;
        Ok(())
    }

    /// 验证扩展名误写为 `.tar.gz` 的纯 tar 文件会按真实 TAR 内容读取。
    ///
    /// 业务意图：
    /// - 用户现场样本是纯 tar 归档，但文件名带 `.tar.gz`。
    /// - 读取链路需要复用内容探测结果，避免单文件打开或分页物化继续走 gzip 解码。
    #[test]
    fn 读取_tar_扩展名误写为_tar_gz_原始字节() -> Result<(), Box<dyn Error>> {
        let temp_dir = unique_temp_dir("logclinic3-plain-tar-content-test")?;
        let archive_path = temp_dir.join("wrong-name.tar.gz");
        write_test_tar(&archive_path, &[("only.log", b"INFO plain tar".as_slice())])?;

        let bytes = read_log_source_bytes(&LogFileSource::LocalFile {
            path: archive_path.clone(),
        })?;

        assert_eq!(&bytes[..], b"INFO plain tar");
        fs::remove_dir_all(temp_dir)?;
        Ok(())
    }

    /// 验证外层压缩包里的单文件 ZIP 成员会继续读取内部唯一文件，而不是把 ZIP 二进制当作日志文本。
    #[test]
    fn 读取外层_zip_中的单文件_zip_成员原始字节() -> Result<(), Box<dyn Error>> {
        let temp_dir = unique_temp_dir("logclinic3-nested-zip-content-test")?;
        let archive_path = temp_dir.join("outer.zip");
        let inner_cursor = Cursor::new(Vec::new());
        let mut inner_writer = zip::ZipWriter::new(inner_cursor);

        inner_writer.start_file("thread.log", zip::write::SimpleFileOptions::default())?;
        inner_writer.write_all(b"INFO nested zip")?;
        let inner_zip_bytes = inner_writer.finish()?.into_inner();

        let archive_file = File::create(&archive_path)?;
        let mut zip_writer = zip::ZipWriter::new(archive_file);
        zip_writer.start_file(
            "thread_000209.zip",
            zip::write::SimpleFileOptions::default(),
        )?;
        zip_writer.write_all(&inner_zip_bytes)?;
        zip_writer.finish()?;

        let bytes = read_log_source_bytes(&LogFileSource::ArchiveMember {
            archive_path: archive_path.clone(),
            archive_format: ArchiveFormat::Zip,
            member_path: "thread_000209.zip".to_string(),
        })?;

        assert_eq!(&bytes[..], b"INFO nested zip");
        fs::remove_dir_all(temp_dir)?;
        Ok(())
    }

    /// 验证外层压缩包里的多文件 ZIP 成员不会随意打开第一个文件，避免用户看到错误日志。
    #[test]
    fn 多文件嵌套_zip_要求选择具体文件() -> Result<(), Box<dyn Error>> {
        let temp_dir = unique_temp_dir("logclinic3-multi-nested-zip-content-test")?;
        let archive_path = temp_dir.join("outer.zip");
        let inner_cursor = Cursor::new(Vec::new());
        let mut inner_writer = zip::ZipWriter::new(inner_cursor);

        inner_writer.start_file("first.log", zip::write::SimpleFileOptions::default())?;
        inner_writer.write_all(b"INFO first")?;
        inner_writer.start_file("second.log", zip::write::SimpleFileOptions::default())?;
        inner_writer.write_all(b"INFO second")?;
        let inner_zip_bytes = inner_writer.finish()?.into_inner();

        let archive_file = File::create(&archive_path)?;
        let mut zip_writer = zip::ZipWriter::new(archive_file);
        zip_writer.start_file("thread_multi.zip", zip::write::SimpleFileOptions::default())?;
        zip_writer.write_all(&inner_zip_bytes)?;
        zip_writer.finish()?;

        let error = read_log_source_bytes(&LogFileSource::ArchiveMember {
            archive_path: archive_path.clone(),
            archive_format: ArchiveFormat::Zip,
            member_path: "thread_multi.zip".to_string(),
        })
        .unwrap_err();

        assert!(error.to_string().contains("多个文件"));
        fs::remove_dir_all(temp_dir)?;
        Ok(())
    }

    /// 验证可以读取外层 ZIP 中多文件内层 ZIP 的指定成员。
    ///
    /// 业务意图：
    /// - 加载树把多文件内层压缩包展开成目录后，用户点击具体文件必须能准确读取内层成员，而不是继续对内层压缩包做“唯一文件”判断。
    #[test]
    fn 读取外层_zip_中多文件_zip_的指定成员() -> Result<(), Box<dyn Error>> {
        let temp_dir = unique_temp_dir("logclinic3-nested-specific-content-test")?;
        let archive_path = temp_dir.join("outer.zip");
        let inner_cursor = Cursor::new(Vec::new());
        let mut inner_writer = zip::ZipWriter::new(inner_cursor);

        inner_writer.start_file("first.log", zip::write::SimpleFileOptions::default())?;
        inner_writer.write_all(b"INFO first")?;
        inner_writer.start_file("second.log", zip::write::SimpleFileOptions::default())?;
        inner_writer.write_all(b"INFO second")?;
        let inner_zip_bytes = inner_writer.finish()?.into_inner();

        let archive_file = File::create(&archive_path)?;
        let mut zip_writer = zip::ZipWriter::new(archive_file);
        zip_writer.start_file("thread_multi.zip", zip::write::SimpleFileOptions::default())?;
        zip_writer.write_all(&inner_zip_bytes)?;
        zip_writer.finish()?;

        let bytes = read_log_source_bytes(&LogFileSource::NestedArchiveMember {
            outer_archive_path: archive_path.clone(),
            outer_archive_format: ArchiveFormat::Zip,
            archive_member_path: "thread_multi.zip".to_string(),
            nested_archive_format: ArchiveFormat::Zip,
            nested_member_path: "second.log".to_string(),
        })?;

        assert_eq!(&bytes[..], b"INFO second");
        fs::remove_dir_all(temp_dir)?;
        Ok(())
    }

    /// 验证嵌套 ZIP 指定成员也可以通过 writer 流式输出目标日志正文。
    ///
    /// 业务意图：
    /// - 外层包中的内层多文件压缩包需要先定位内层压缩包，但具体日志成员不应再整体读入内存后才交给插件。
    #[test]
    fn 流式读取外层_zip_中多文件_zip_的指定成员() -> Result<(), Box<dyn Error>> {
        let temp_dir = unique_temp_dir("logclinic3-nested-stream-content-test")?;
        let archive_path = temp_dir.join("outer.zip");
        let inner_cursor = Cursor::new(Vec::new());
        let mut inner_writer = zip::ZipWriter::new(inner_cursor);

        inner_writer.start_file("first.log", zip::write::SimpleFileOptions::default())?;
        inner_writer.write_all(b"INFO first")?;
        inner_writer.start_file("second.log", zip::write::SimpleFileOptions::default())?;
        inner_writer.write_all(b"INFO second stream")?;
        let inner_zip_bytes = inner_writer.finish()?.into_inner();

        let archive_file = File::create(&archive_path)?;
        let mut zip_writer = zip::ZipWriter::new(archive_file);
        zip_writer.start_file("thread_multi.zip", zip::write::SimpleFileOptions::default())?;
        zip_writer.write_all(&inner_zip_bytes)?;
        zip_writer.finish()?;
        let mut streamed = Vec::new();

        stream_log_source_bytes_to_writer(
            &LogFileSource::NestedArchiveMember {
                outer_archive_path: archive_path.clone(),
                outer_archive_format: ArchiveFormat::Zip,
                archive_member_path: "thread_multi.zip".to_string(),
                nested_archive_format: ArchiveFormat::Zip,
                nested_member_path: "second.log".to_string(),
            },
            &mut streamed,
        )?;

        assert_eq!(streamed, b"INFO second stream");
        fs::remove_dir_all(temp_dir)?;
        Ok(())
    }

    /// 验证大小上限错误不会继续进入解码流程。
    #[test]
    fn 大小上限拒绝超大日志() {
        let error = ensure_size_within_limit(MAX_LOG_FILE_BYTES + 1, "huge.log").unwrap_err();

        assert!(error.to_string().contains("200MB"));
    }

    /// 创建当前测试进程独占的临时目录。
    ///
    /// 业务意图：
    /// - 不引入额外临时目录依赖，保持当前工程依赖面稳定。
    fn unique_temp_dir(prefix: &str) -> io::Result<std::path::PathBuf> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let path =
            std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos));
        fs::create_dir_all(&path)?;
        Ok(path)
    }

    /// 写入测试用 TAR.GZ 文件。
    ///
    /// 业务意图：
    /// - TAR.GZ 顺序读取问题需要真实 gzip + tar 流才能覆盖，不能用普通内存字节替代。
    fn write_test_tar_gz(path: &Path, files: &[(&str, &[u8])]) -> io::Result<()> {
        let file = File::create(path)?;
        let encoder = GzEncoder::new(file, Compression::default());
        let mut builder = TarBuilder::new(encoder);

        for (name, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(name)?;
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, *bytes)?;
        }

        let encoder = builder.into_inner()?;
        encoder.finish()?;
        Ok(())
    }

    /// 写入测试用未压缩 TAR 文件。
    ///
    /// 业务意图：
    /// - 用真实 tar header 覆盖扩展名误写时的内容探测和读取路径。
    fn write_test_tar(path: &Path, files: &[(&str, &[u8])]) -> io::Result<()> {
        let file = File::create(path)?;
        let mut builder = TarBuilder::new(file);

        for (name, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(name)?;
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, *bytes)?;
        }

        builder.finish()?;
        Ok(())
    }

    /// 写入测试用单文件 gzip 日志。
    ///
    /// 业务意图：
    /// - 用真实 gzip 流覆盖 `.tar.gz` 非 tar 内容的兼容读取路径。
    fn write_test_gzip(path: &Path, bytes: &[u8]) -> io::Result<()> {
        let file = File::create(path)?;
        let mut encoder = GzEncoder::new(file, Compression::default());
        encoder.write_all(bytes)?;
        encoder.finish()?;
        Ok(())
    }

    /// 生成测试用 gzip 字节。
    ///
    /// 业务意图：
    /// - 压缩包内 `.gz` 成员测试需要把 gzip 流直接写入外层 ZIP，不经过临时文件。
    /// - 使用真实 `flate2` 编码器覆盖生产读取路径，避免手写固定字节导致测试和库行为脱节。
    fn gzip_test_bytes(bytes: &[u8]) -> io::Result<Vec<u8>> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(bytes)?;
        encoder.finish()
    }
}
