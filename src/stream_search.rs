//! 分页日志流式搜索模块。
//!
//! 业务意图：
//! - 超大日志不能为了搜索把所有行解码进内存，因此分页文档按行读取、即时匹配、即时丢弃未命中文本。
//! - 搜索结果仍复用现有 `SearchResultItem`，保证搜索面板、点击跳转和高亮逻辑不需要区分内存/分页来源。
//! - 单次搜索最多收集 50,000 行结果，避免过宽关键字在大日志中生成百万级 UI 行。
//!
//! 关键约束：
//! - 搜索仍按单行执行，不做跨行组合；多关键字快搜按“任一关键字命中”处理。
//! - 普通文本不区分大小写时沿用现有 Unicode 小写折叠规则，正则模式完全由表达式自身控制。

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};

use crate::{
    line_index::LineIndexEntry,
    log_content::decode_lossy,
    paged_document::PagedLogDocument,
    search::{SearchMatcher, SearchOptions, SearchResultItem, source_location_label},
};

/// 单次搜索最多保留的结果行数。
pub const STREAM_SEARCH_RESULT_LIMIT: usize = 50_000;

/// 分页搜索结果。
#[derive(Clone, Debug)]
pub struct StreamSearchOutcome {
    /// 命中行结果。
    pub results: Vec<SearchResultItem>,
    /// 是否因为结果过多被截断。
    pub truncated: bool,
}

/// 在分页文档中搜索。
pub fn search_paged_document(
    document: &PagedLogDocument,
    options: &SearchOptions,
) -> StreamSearchOutcome {
    if options.is_empty_query() {
        return StreamSearchOutcome {
            results: Vec::new(),
            truncated: false,
        };
    }
    let Ok(matcher) = SearchMatcher::new(options) else {
        return StreamSearchOutcome {
            results: Vec::new(),
            truncated: false,
        };
    };

    let source = document.source.clone();
    let file_name = source.display_name();
    let source_key = source.stable_key();
    let location = source_location_label(&source);
    let mut results = Vec::new();

    let Ok(mut file) = File::open(&document.path) else {
        return StreamSearchOutcome {
            results,
            truncated: false,
        };
    };
    let _ = document.line_index.for_each(|line_index, entry| {
        let Some(line_text) = read_line_text(&mut file, document, entry) else {
            return Ok(true);
        };
        if let Some(match_range) = matcher.best_range(&line_text) {
            results.push(SearchResultItem {
                source: source.clone(),
                source_key: source_key.clone(),
                file_name: file_name.clone(),
                location: location.clone(),
                line_index,
                line_text,
                match_range,
            });
            if results.len() >= STREAM_SEARCH_RESULT_LIMIT {
                return Ok(false);
            }
        }
        Ok(true)
    });

    let truncated = results.len() >= STREAM_SEARCH_RESULT_LIMIT;
    StreamSearchOutcome { results, truncated }
}

/// 统计分页文档中查询词出现次数。
pub fn count_query_occurrences_paged(
    document: &PagedLogDocument,
    options: &SearchOptions,
) -> usize {
    if options.is_empty_query() {
        return 0;
    }
    let Ok(matcher) = SearchMatcher::new(options) else {
        return 0;
    };

    let mut count = 0_usize;
    let Ok(mut file) = File::open(&document.path) else {
        return 0;
    };
    let _ = document.line_index.for_each(|_, entry| {
        let Some(line_text) = read_line_text(&mut file, document, entry) else {
            return Ok(true);
        };
        count = count.saturating_add(matcher.count_in_line(&line_text));
        Ok(true)
    });
    count
}

/// 使用单个文件句柄按索引读取并解码一行。
///
/// 业务意图：
/// - 搜索和计数会顺序访问大量行，必须复用文件句柄，避免每行 `File::open` 带来的系统调用风暴。
fn read_line_text(
    file: &mut File,
    document: &PagedLogDocument,
    entry: LineIndexEntry,
) -> Option<String> {
    file.seek(SeekFrom::Start(entry.offset)).ok()?;
    let mut bytes = vec![0_u8; entry.byte_len as usize];
    file.read_exact(&mut bytes).ok()?;
    decode_lossy(&bytes, document.encoding)
        .ok()
        .map(|decoded| decoded.text)
}

#[cfg(test)]
mod tests {
    //! 分页搜索规则测试。
    //!
    //! 业务意图：
    //! - 超大日志分页路径不能和内存日志搜索出现语义差异，尤其是正则匹配范围和计数规则。

    use std::{
        env, fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::{
        archive_materializer::MaterializedLogSource,
        log_content::{EncodingChoice, LogTextEncoding},
        log_loader::LogFileSource,
        search::{SearchMatchMode, count_query_occurrences, search_lines},
    };

    /// 为测试创建唯一临时目录，避免并行测试互相覆盖分页文件。
    fn unique_test_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试时钟应可用")
            .as_nanos();
        env::temp_dir().join(format!(
            "logclinic-stream-search-{name}-{}-{nanos}",
            std::process::id()
        ))
    }

    /// 构造分页文档并返回需要清理的临时目录。
    ///
    /// 边界条件：
    /// - 测试写入普通 UTF-8 文件，目的是验证分页搜索逻辑，不覆盖编码探测异常。
    fn paged_document_with_content(name: &str, content: &str) -> (PathBuf, PagedLogDocument) {
        let dir = unique_test_dir(name);
        fs::create_dir_all(&dir).expect("测试目录应能创建");
        let path = dir.join("app.log");
        fs::write(&path, content).expect("测试日志应能写入");
        let byte_len = fs::metadata(&path).expect("测试日志元数据应可读").len();
        let source = LogFileSource::LocalFile { path: path.clone() };
        let materialized = MaterializedLogSource {
            original_source: source,
            temp_path: path,
            byte_len,
        };
        let document = PagedLogDocument::open(
            materialized,
            EncodingChoice::Manual(LogTextEncoding::Utf8),
            "app.log",
        )
        .expect("测试分页文档应能打开");
        (dir, document)
    }

    /// 分页正则搜索应与内存日志搜索返回同样的命中行和高亮片段。
    #[test]
    fn 分页正则搜索与内存搜索一致() {
        let content = "INFO id=42\nWARN id=100\nINFO id=7\n";
        let (dir, document) = paged_document_with_content("regex-search", content);
        let source = document.source.clone();
        let lines = content.lines().map(str::to_string).collect::<Vec<_>>();
        let options = SearchOptions::single_with_mode(r"id=\d+", false, SearchMatchMode::Regex);

        let paged = search_paged_document(&document, &options);
        let memory = search_lines(&source, &lines, &options);

        let paged_ranges = paged
            .results
            .iter()
            .map(|item| {
                (
                    item.line_index,
                    item.line_text[item.match_range.clone()].to_string(),
                )
            })
            .collect::<Vec<_>>();
        let memory_ranges = memory
            .iter()
            .map(|item| {
                (
                    item.line_index,
                    item.line_text[item.match_range.clone()].to_string(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(paged_ranges, memory_ranges);

        let _ = fs::remove_dir_all(dir);
    }

    /// 分页正则计数应与内存日志计数一致，避免“计数”和“搜索结果”在大文件上出现不同语义。
    #[test]
    fn 分页正则计数与内存计数一致() {
        let content = "INFO id=42 id=100\nWARN no id\nINFO id=7\n";
        let (dir, document) = paged_document_with_content("regex-count", content);
        let lines = content.lines().map(str::to_string).collect::<Vec<_>>();
        let options = SearchOptions::single_with_mode(r"id=\d+", true, SearchMatchMode::Regex);

        assert_eq!(
            count_query_occurrences_paged(&document, &options),
            count_query_occurrences(&lines, &options)
        );

        let _ = fs::remove_dir_all(dir);
    }
}
