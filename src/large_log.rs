//! 大小文件统一打开入口模块。
//!
//! 业务意图：
//! - 小文件继续使用已有 `DecodedLogDocument`，保留完整语法高亮、快速计数和内存内搜索体验。
//! - 超过 30MB 的普通文件或压缩包成员进入分页模式，只建立行索引并按需读取可见文本。
//! - UI 层通过 `LogTabDocument` 消费统一文档模型，避免后续编码切换、搜索和渲染继续假设所有日志都在内存中。
//!
//! 关键约束：
//! - 本地普通大文件不复制，直接分页读取原路径；压缩包成员必须物化成临时文件才能 seek。
//! - 只有大小超过分页阈值时才切入分页模式，小文件行为尽量保持不变。
//! - 压缩包成员如果无法在打开前廉价获知解压后大小，会先按现有读取路径拿到字节；一旦超过 30MB 阈值即丢弃内存文档并改走物化分页。

use std::{fs, path::Path, sync::Arc};

use crate::{
    archive_materializer::{cleanup_materialized_file, materialize_source_for_paging},
    log_content::{
        DecodedLogDocument, EncodingChoice, LogContentError, decode_log_bytes,
        read_log_source_bytes,
    },
    log_loader::{ArchiveFormat, LogFileSource},
    paged_document::PagedLogDocument,
};

/// 超大日志分页阈值。
///
/// 业务意图：
/// - 用户要求日志超过 30MB 就切换到分页模式，降低打开中等偏大日志时的内存占用和 UI 压力。
/// - 该阈值只决定“浏览模式”；`log_content::MAX_LOG_FILE_BYTES` 仍作为内存读取硬上限，防止压缩包成员声明大小异常时一次性读爆内存。
pub const LARGE_LOG_THRESHOLD_BYTES: u64 = 30 * 1024 * 1024;

/// tab 可渲染的日志文档。
#[derive(Clone, Debug)]
pub enum LogTabDocument {
    /// 30MB 以内的小文件完整解码后保存在内存中。
    InMemory(DecodedLogDocument),
    /// 超大文件按行索引分页读取。
    Paged(PagedLogDocument),
}

impl LogTabDocument {
    /// 返回当前文档行数。
    pub fn line_count(&self) -> usize {
        match self {
            Self::InMemory(document) => document.line_count(),
            Self::Paged(document) => document.line_count(),
        }
    }

    /// 返回用于横向测量的最长候选行。
    pub fn longest_line_index(&self) -> usize {
        match self {
            Self::InMemory(document) => document.longest_line_index,
            Self::Paged(document) => document.longest_line_index,
        }
    }

    /// 返回当前实际编码。
    pub fn encoding_label(&self) -> &'static str {
        match self {
            Self::InMemory(document) => document.encoding.label(),
            Self::Paged(document) => document.encoding.label(),
        }
    }

    /// 返回当前编码是否来自自动检测。
    pub fn detected_automatically(&self) -> bool {
        match self {
            Self::InMemory(document) => document.detected_automatically,
            Self::Paged(document) => document.detected_automatically,
        }
    }

    /// 返回当前解码是否出现替换字符。
    pub fn had_replacements(&self) -> bool {
        match self {
            Self::InMemory(document) => document.had_replacements,
            Self::Paged(document) => document.had_replacements,
        }
    }

    /// 返回当前文档警告。
    pub fn warning(&self) -> Option<&str> {
        match self {
            Self::InMemory(document) => document.warning.as_deref(),
            Self::Paged(document) => document.warning.as_deref(),
        }
    }

    /// 返回工具条行数和分页索引状态文案。
    pub fn status_line_label(&self) -> String {
        match self {
            Self::InMemory(document) => format!("{} 行", document.line_count()),
            Self::Paged(document) => {
                let percent = if document.byte_len == 0 {
                    100.0
                } else {
                    (document.index_state.scanned_bytes as f64 / document.byte_len as f64 * 100.0)
                        .clamp(0.0, 100.0)
                };
                format!(
                    "{} 行 · 分页模式 · 已扫描 {:.0}%",
                    document.index_state.indexed_lines, percent
                )
            }
        }
    }
}

/// 日志打开结果。
pub enum LargeLogOpenResult {
    /// 小文件读取和解码成功。
    InMemoryReady {
        /// 原始字节，用于后续切换编码。
        raw_bytes: Arc<Vec<u8>>,
        /// 已解码文档。
        document: DecodedLogDocument,
    },
    /// 小文件原始字节读取成功，但自动检测失败。
    InMemoryDecodeFailed {
        /// 原始字节仍需保留，允许用户手动选择编码。
        raw_bytes: Arc<Vec<u8>>,
        /// 中文错误说明。
        message: String,
    },
    /// 超大文件分页打开成功。
    PagedReady {
        /// 分页文档。
        document: PagedLogDocument,
    },
}

/// 打开日志来源并自动选择内存或分页模式。
pub fn open_log_source_for_tab(
    source: LogFileSource,
    encoding_choice: EncodingChoice,
    source_name: &str,
) -> Result<LargeLogOpenResult, LogContentError> {
    if should_open_local_file_as_paged(&source)? {
        return open_source_as_paged(source, encoding_choice, source_name)
            .map(|document| LargeLogOpenResult::PagedReady { document });
    }

    match read_log_source_bytes(&source) {
        Ok(raw_bytes) => {
            // 压缩包成员、单文件压缩包和嵌套压缩包有时只有读取后才能拿到真实解压大小。
            // 一旦超过 30MB 产品阈值，就立即改用分页物化管线，避免最终 tab 持有整份日志文本和原始字节。
            if raw_bytes.len() as u64 > LARGE_LOG_THRESHOLD_BYTES {
                return open_source_as_paged(source, encoding_choice, source_name)
                    .map(|document| LargeLogOpenResult::PagedReady { document });
            }

            match decode_log_bytes(&raw_bytes, encoding_choice, source_name) {
                Ok(document) => Ok(LargeLogOpenResult::InMemoryReady {
                    raw_bytes,
                    document,
                }),
                Err(error) => Ok(LargeLogOpenResult::InMemoryDecodeFailed {
                    raw_bytes,
                    message: error.to_string(),
                }),
            }
        }
        Err(error) if should_retry_as_paged(&source, &error) => {
            open_source_as_paged(source, encoding_choice, source_name)
                .map(|document| LargeLogOpenResult::PagedReady { document })
        }
        Err(error) => Err(error),
    }
}

/// 打开分页文档。
pub fn open_source_as_paged(
    source: LogFileSource,
    encoding_choice: EncodingChoice,
    source_name: &str,
) -> Result<PagedLogDocument, LogContentError> {
    let materialized = materialize_source_for_paging(&source)?;
    match PagedLogDocument::open(materialized.clone(), encoding_choice, source_name) {
        Ok(document) => Ok(document),
        Err(error) => {
            if matches!(
                materialized.original_source,
                LogFileSource::ArchiveMember { .. } | LogFileSource::NestedArchiveMember { .. }
            ) || materialized.temp_path
                != match &materialized.original_source {
                    LogFileSource::LocalFile { path } => path.clone(),
                    LogFileSource::MaterializedArchiveMember { temp_path, .. } => temp_path.clone(),
                    LogFileSource::ArchiveMember { .. }
                    | LogFileSource::NestedArchiveMember { .. } => materialized.temp_path.clone(),
                }
            {
                cleanup_materialized_file(&materialized.temp_path);
            }
            Err(error)
        }
    }
}

/// 判断本地普通文件是否应直接进入分页模式。
fn should_open_local_file_as_paged(source: &LogFileSource) -> Result<bool, LogContentError> {
    let LogFileSource::LocalFile { path } = source else {
        return Ok(false);
    };
    if ArchiveFormat::from_file(path).is_some() {
        return Ok(false);
    }
    let len = fs::metadata(path)
        .map_err(|error| {
            LogContentError::new(format!(
                "无法读取日志文件 {} 的大小：{}",
                path.display(),
                error
            ))
        })?
        .len();
    Ok(len > LARGE_LOG_THRESHOLD_BYTES)
}

/// 判断内存读取失败是否应转入分页物化。
fn should_retry_as_paged(source: &LogFileSource, error: &LogContentError) -> bool {
    let message = error.to_string();
    if !message.contains("超过 200MB 上限") {
        return false;
    }

    match source {
        LogFileSource::LocalFile { path } => ArchiveFormat::from_file(path).is_some(),
        LogFileSource::MaterializedArchiveMember { member_path, .. } => {
            ArchiveFormat::from_path(Path::new(member_path)).is_some()
        }
        LogFileSource::ArchiveMember { .. } | LogFileSource::NestedArchiveMember { .. } => true,
    }
}

#[cfg(test)]
mod tests {
    //! 大小文件打开策略测试。
    //!
    //! 业务意图：
    //! - 分页阈值直接影响日志打开模式和内存占用，必须用边界测试锁定“超过 30MB 才分页”的规则。
    //! - 测试通过 `set_len` 创建稀疏文件，不写入真实 30MB 数据，避免单元测试拖慢或占用磁盘。

    use std::{env, fs, fs::File};

    use super::*;

    /// 构造唯一的分页阈值测试文件路径。
    ///
    /// 边界条件：
    /// - 路径包含进程 ID 和测试名称，避免并行测试互相覆盖。
    fn test_large_log_path(name: &str) -> std::path::PathBuf {
        env::temp_dir().join(format!(
            "logclinic-large-threshold-test-{}-{}.log",
            std::process::id(),
            name
        ))
    }

    /// 创建指定长度的本地日志测试文件。
    ///
    /// 业务意图：
    /// - 打开策略只依赖文件元数据长度，因此不需要写入真实日志内容。
    fn create_sized_test_file(name: &str, len: u64) -> std::path::PathBuf {
        let path = test_large_log_path(name);
        let _ = fs::remove_file(&path);
        let file = File::create(&path).expect("分页阈值测试应能创建临时文件");
        file.set_len(len).expect("分页阈值测试应能设置临时文件长度");
        path
    }

    /// 验证超过 30MB 的普通日志直接进入分页模式。
    #[test]
    fn 超过三十兆的普通日志会进入分页模式() {
        let path = create_sized_test_file("above", LARGE_LOG_THRESHOLD_BYTES + 1);
        let source = LogFileSource::LocalFile { path: path.clone() };

        assert!(
            should_open_local_file_as_paged(&source).expect("应能读取测试文件大小"),
            "超过 30MB 的普通文件应直接分页打开"
        );

        let _ = fs::remove_file(path);
    }

    /// 验证等于 30MB 的普通日志仍保留内存模式。
    #[test]
    fn 等于三十兆的普通日志仍使用内存模式() {
        let path = create_sized_test_file("equal", LARGE_LOG_THRESHOLD_BYTES);
        let source = LogFileSource::LocalFile { path: path.clone() };

        assert!(
            !should_open_local_file_as_paged(&source).expect("应能读取测试文件大小"),
            "阈值规则是严格超过 30MB 才分页"
        );

        let _ = fs::remove_file(path);
    }
}
