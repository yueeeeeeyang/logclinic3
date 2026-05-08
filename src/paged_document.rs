//! 超大日志分页文档模块。
//!
//! 业务意图：
//! - 小日志继续走内存文档，超大日志走按行索引和按需解码，避免打开 10GB+ 日志时常驻内存随文件大小线性增长。
//! - UI 仍然按“第 N 行文本”渲染，因此本模块负责把行号转换为文件偏移、读取字节、按当前编码解码并缓存可见行。
//! - 编码切换只更新编码和清空缓存，不重建行索引，保证用户试错 GBK/GB18030/Big5 时成本可控。
//!
//! 关键约束：
//! - 只缓存近期读取的行文本，并以 64MB 文本容量作为软上限；超过后清空缓存，避免长期浏览造成内存攀升。
//! - 单行读取按行索引长度分配缓冲区，超长行由行索引阶段限制在 4GB 以内。
//! - 该模块不做文件追加监听；打开后按快照索引处理，外部修改文件可能导致偏移失效。

use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use crate::{
    archive_materializer::MaterializedLogSource,
    highlighting::{HighlightMode, detect_highlight_mode},
    line_index::{LineIndexEntries, LineIndexEntry, LineIndexState, build_line_index},
    log_content::{
        EncodingChoice, LogContentError, LogTextEncoding, decode_lossy, detect_log_encoding,
    },
    log_loader::LogFileSource,
};

/// 分页文档默认解码缓存上限。
///
/// 业务意图：
/// - 64MB 足够覆盖可见区域和附近滚动，同时不会让长时间浏览多个大文件时内存失控。
pub const PAGED_DECODE_CACHE_LIMIT_BYTES: usize = 64 * 1024 * 1024;

/// 自动检测编码时读取的样本上限。
///
/// 边界条件：
/// - 只读取头部样本，避免为了检测编码扫描完整 10GB 文件；极少数文件头部不具代表性时用户可手动切换编码。
const ENCODING_SAMPLE_BYTES: usize = 4 * 1024 * 1024;

/// 分页读取到的一行文本。
#[derive(Clone, Debug)]
pub struct PagedLine {
    /// 0 基行号。
    pub line_number: usize,
    /// 解码后的行文本。
    pub text: String,
    /// 行首原始字节偏移。
    pub byte_offset: u64,
    /// 解码时是否发生替换字符。
    pub had_replacements: bool,
}

/// 超大日志分页文档。
///
/// 业务意图：
/// - `source` 用于搜索结果和 tab 去重，`path` 用于实际 seek 读取。
/// - 压缩包成员会写入 `materialized_temp_path`，关闭 tab 时由 UI 统一清理。
#[derive(Clone, Debug)]
pub struct PagedLogDocument {
    /// 用户打开的原始来源。
    pub source: LogFileSource,
    /// 实际分页读取的本地文件路径。
    pub path: PathBuf,
    /// 关闭 tab 时需要清理的临时文件；普通本地大文件为 `None`。
    pub materialized_temp_path: Option<PathBuf>,
    /// 文件总字节数。
    pub byte_len: u64,
    /// 当前用于解码可见行的编码。
    pub encoding: LogTextEncoding,
    /// 当前编码是否来自自动检测。
    pub detected_automatically: bool,
    /// 行索引条目。
    pub line_index: LineIndexEntries,
    /// 行索引状态。
    pub index_state: LineIndexState,
    /// 横向测量候选行。
    pub longest_line_index: usize,
    /// 当前文档使用的轻量高亮模式。
    pub highlight_mode: HighlightMode,
    /// 分页读取过程中是否发现替换字符。
    pub had_replacements: bool,
    /// 面向用户的中文提示。
    pub warning: Option<String>,
    /// 可见行解码缓存。
    cache: Arc<Mutex<PagedLineCache>>,
    /// 复用的日志文件句柄。
    ///
    /// 业务意图：
    /// - 滚动渲染会按可见行频繁读取文件，若每行都重新 `File::open`，拖动滚动条会被系统调用和句柄创建拖慢。
    /// - 这里按分页文档共享一个文件句柄，读取时加锁并 seek 到目标行，保持实现简单且不会跨 tab 混用。
    file_handle: Arc<Mutex<Option<File>>>,
}

impl PagedLogDocument {
    /// 从已物化或本地文件创建分页文档。
    ///
    /// 业务意图：
    /// - 后台打开流程先确保 `path` 是可 seek 的普通文件，再进入行索引和编码样本检测。
    /// - 这里会完成整份文件索引；当前 UI 因 `uniform_list` 需要总行数，后续可在该结构上扩展增量索引状态。
    pub fn open(
        materialized: MaterializedLogSource,
        encoding_choice: EncodingChoice,
        source_name: &str,
    ) -> Result<Self, LogContentError> {
        let path = materialized.temp_path.clone();
        let line_index = build_line_index(&path).map_err(|error| {
            LogContentError::new(format!("无法为超大日志建立行索引：{}", error))
        })?;
        let encoding = resolve_paged_encoding(&path, encoding_choice)?;
        let detected_automatically = matches!(encoding_choice, EncodingChoice::Auto);
        let warning = detected_automatically.then(|| {
            format!(
                "超大日志已进入分页模式，当前按 {} 解码；编码切换会复用行索引",
                encoding.label()
            )
        });
        let sample_lines = read_sample_lines(&path, &line_index.entries, encoding)?;
        let highlight_mode = detect_highlight_mode(source_name, &sample_lines);
        let materialized_temp_path =
            is_temp_materialization(&materialized).then(|| materialized.temp_path.clone());

        Ok(Self {
            source: materialized.original_source,
            path,
            materialized_temp_path,
            byte_len: materialized.byte_len,
            encoding,
            detected_automatically,
            line_index: line_index.entries,
            index_state: line_index.state,
            longest_line_index: line_index.longest_line_index,
            highlight_mode,
            had_replacements: false,
            warning,
            cache: Arc::new(Mutex::new(PagedLineCache::default())),
            file_handle: Arc::new(Mutex::new(None)),
        })
    }

    /// 返回分页文档总行数。
    pub fn line_count(&self) -> usize {
        self.line_index.len()
    }

    /// 切换编码并清空可见行缓存。
    ///
    /// 业务意图：
    /// - 行索引基于原始换行字节，不依赖文本编码，因此编码切换不应重新扫描 10GB 文件。
    pub fn with_encoding(&self, encoding_choice: EncodingChoice) -> Result<Self, LogContentError> {
        let encoding = resolve_paged_encoding(&self.path, encoding_choice)?;
        let mut next = self.clone();
        next.encoding = encoding;
        next.detected_automatically = matches!(encoding_choice, EncodingChoice::Auto);
        next.warning = Some(format!(
            "超大日志分页模式已按 {} 重新解码可见行",
            encoding.label()
        ));
        next.had_replacements = false;
        next.cache = Arc::new(Mutex::new(PagedLineCache::default()));
        next.file_handle = Arc::new(Mutex::new(None));
        Ok(next)
    }

    /// 读取单行文本。
    ///
    /// 边界条件：
    /// - 行号越界返回 `None`，调用方通常来自虚拟列表范围，正常不会发生。
    /// - 读取失败会返回中文错误，UI 可把当前 tab 切到失败状态。
    pub fn read_line(&self, line_number: usize) -> Result<Option<PagedLine>, LogContentError> {
        if let Ok(cache) = self.cache.lock()
            && let Some(line) = cache.lines.get(&line_number)
        {
            return Ok(Some(line.clone()));
        }

        let Some(entry) = self.line_index.get(line_number) else {
            return Ok(None);
        };
        let line = self.read_line_by_entry(line_number, entry)?;

        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(line_number, line.clone());
        }
        Ok(Some(line))
    }

    /// 按已知索引条目读取单行文本。
    ///
    /// 业务意图：
    /// - 流式搜索已经顺序扫描到索引条目时，直接复用该条目，避免再次打开索引文件随机读取。
    pub fn read_line_by_entry(
        &self,
        line_number: usize,
        entry: LineIndexEntry,
    ) -> Result<PagedLine, LogContentError> {
        let bytes = self.read_line_bytes_with_shared_handle(line_number, entry)?;
        let decoded = decode_lossy(&bytes, self.encoding)?;
        Ok(PagedLine {
            line_number,
            text: decoded.text,
            byte_offset: entry.offset,
            had_replacements: decoded.had_errors,
        })
    }

    /// 使用分页文档共享文件句柄读取单行原始字节。
    ///
    /// 边界条件：
    /// - 如果上一次文件句柄因外部删除或权限变化失效，会返回可理解错误；用户可重新打开文件刷新快照。
    fn read_line_bytes_with_shared_handle(
        &self,
        line_number: usize,
        entry: LineIndexEntry,
    ) -> Result<Vec<u8>, LogContentError> {
        let mut handle_guard = self
            .file_handle
            .lock()
            .map_err(|_| LogContentError::new("超大日志文件句柄被其它任务占用，暂时无法读取"))?;
        if handle_guard.is_none() {
            *handle_guard = Some(File::open(&self.path).map_err(|error| {
                LogContentError::new(format!(
                    "无法读取超大日志 {}：{}",
                    self.path.display(),
                    error
                ))
            })?);
        }
        let file = handle_guard
            .as_mut()
            .ok_or_else(|| LogContentError::new("超大日志文件句柄初始化失败"))?;
        file.seek(SeekFrom::Start(entry.offset)).map_err(|error| {
            LogContentError::new(format!("无法定位超大日志行 {}：{}", line_number + 1, error))
        })?;
        let mut bytes = vec![0_u8; entry.byte_len as usize];
        file.read_exact(&mut bytes).map_err(|error| {
            LogContentError::new(format!("无法读取超大日志行 {}：{}", line_number + 1, error))
        })?;
        Ok(bytes)
    }
}

/// 可见行缓存。
#[derive(Debug, Default)]
struct PagedLineCache {
    /// 行号到解码文本的缓存。
    lines: HashMap<usize, PagedLine>,
    /// 当前缓存估算字节数。
    byte_size: usize,
}

impl PagedLineCache {
    /// 插入一行缓存，并在超过软上限时清空旧缓存。
    fn insert(&mut self, line_number: usize, line: PagedLine) {
        if self.byte_size > PAGED_DECODE_CACHE_LIMIT_BYTES {
            self.lines.clear();
            self.byte_size = 0;
        }
        self.byte_size = self.byte_size.saturating_add(line.text.len());
        self.lines.insert(line_number, line);
    }
}

/// 判断物化来源是否需要关闭 tab 时删除。
fn is_temp_materialization(materialized: &MaterializedLogSource) -> bool {
    match &materialized.original_source {
        LogFileSource::LocalFile { path } => path != &materialized.temp_path,
        LogFileSource::ArchiveMember { .. } => true,
    }
}

/// 解析分页文档当前编码。
fn resolve_paged_encoding(
    path: &Path,
    encoding_choice: EncodingChoice,
) -> Result<LogTextEncoding, LogContentError> {
    match encoding_choice {
        EncodingChoice::Manual(encoding) => Ok(encoding),
        EncodingChoice::Auto => {
            let sample = read_encoding_sample(path)?;
            detect_log_encoding(&sample)
        }
    }
}

/// 读取编码检测样本。
fn read_encoding_sample(path: &Path) -> Result<Vec<u8>, LogContentError> {
    let mut file = File::open(path).map_err(|error| {
        LogContentError::new(format!(
            "无法打开超大日志 {} 进行编码检测：{}",
            path.display(),
            error
        ))
    })?;
    let mut sample = vec![0_u8; ENCODING_SAMPLE_BYTES];
    let read = file.read(&mut sample).map_err(|error| {
        LogContentError::new(format!(
            "无法读取超大日志 {} 的编码样本：{}",
            path.display(),
            error
        ))
    })?;
    sample.truncate(read);
    Ok(sample)
}

/// 读取头部样本行用于高亮模式识别。
fn read_sample_lines(
    path: &Path,
    entries: &LineIndexEntries,
    encoding: LogTextEncoding,
) -> Result<Vec<String>, LogContentError> {
    let document = PagedLogDocument {
        source: LogFileSource::LocalFile {
            path: path.to_path_buf(),
        },
        path: path.to_path_buf(),
        materialized_temp_path: None,
        byte_len: 0,
        encoding,
        detected_automatically: true,
        line_index: entries.clone(),
        index_state: LineIndexState {
            indexed_lines: entries.len(),
            scanned_bytes: 0,
            total_bytes: 0,
            complete: true,
        },
        longest_line_index: 0,
        highlight_mode: HighlightMode::Plain,
        had_replacements: false,
        warning: None,
        cache: Arc::new(Mutex::new(PagedLineCache::default())),
        file_handle: Arc::new(Mutex::new(None)),
    };
    let mut lines = Vec::new();
    for index in 0..entries.len().min(200) {
        if let Some(line) = document.read_line(index)? {
            lines.push(line.text);
        }
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    /// 创建分页文档测试所需的本地临时日志。
    fn write_temp_log(content: &[u8]) -> PathBuf {
        let mut path = std::env::temp_dir();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        path.push(format!("logclinic-paged-document-{unique}.log"));
        fs::write(&path, content).expect("测试临时日志应能写入");
        path
    }

    #[test]
    fn reads_requested_line_without_full_document() {
        let path = write_temp_log(b"first\nsecond\nthird");
        let source = LogFileSource::LocalFile { path: path.clone() };
        let materialized = MaterializedLogSource {
            original_source: source,
            temp_path: path.clone(),
            byte_len: 18,
        };
        let document = PagedLogDocument::open(
            materialized,
            EncodingChoice::Manual(LogTextEncoding::Utf8),
            "sample.log",
        )
        .expect("分页文档应能打开");
        let line = document
            .read_line(1)
            .expect("读取行不应失败")
            .expect("第二行应存在");
        assert_eq!(line.text, "second");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn switching_encoding_keeps_line_index() {
        let path = write_temp_log(b"a\nb");
        let source = LogFileSource::LocalFile { path: path.clone() };
        let materialized = MaterializedLogSource {
            original_source: source,
            temp_path: path.clone(),
            byte_len: 3,
        };
        let document = PagedLogDocument::open(
            materialized,
            EncodingChoice::Manual(LogTextEncoding::Utf8),
            "sample.log",
        )
        .expect("分页文档应能打开");
        let switched = document
            .with_encoding(EncodingChoice::Manual(LogTextEncoding::Gbk))
            .expect("切换编码应只更新分页解码策略");
        assert_eq!(switched.line_count(), document.line_count());
        assert_eq!(switched.encoding, LogTextEncoding::Gbk);
        let _ = fs::remove_file(path);
    }
}
