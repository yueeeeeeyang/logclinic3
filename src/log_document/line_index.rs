//! 超大日志行索引模块。
//!
//! 业务意图：
//! - 超过内存阈值的日志不能再把完整正文解码成 `Vec<String>`，否则 10GB+ 文件会耗尽桌面客户端内存。
//! - 本模块只扫描字节换行位置，记录每一行在文件中的偏移和长度，后续浏览、跳转和搜索按行按需读取。
//! - 行索引不保存正文文本，因此编码切换只需要清空解码缓存，不需要重新扫描文件。
//!
//! 关键约束：
//! - 扫描按固定 8MiB 缓冲区进行，避免读取超大文件时产生大块临时分配。
//! - 同时支持 LF、CRLF 和 CR；压缩包物化后的临时文件也复用同一套规则。
//! - 当前阶段将索引保存在紧凑内存向量中，后续如果遇到上亿行日志，可继续把 segment 落到临时索引文件。

use std::{
    fs::File,
    io::{self, BufReader, Read},
    path::Path,
    sync::Arc,
};

/// 行索引扫描块大小。
///
/// 业务意图：
/// - 8MiB 足够摊薄系统调用开销，同时不会在后台扫描时造成明显内存峰值。
pub const LINE_INDEX_BLOCK_BYTES: usize = 8 * 1024 * 1024;

/// 单行在原始文件中的字节位置。
///
/// 业务意图：
/// - `offset` 指向行首，`byte_len` 不包含换行符，读取可见行时可以直接 seek 后读取指定长度。
/// - 长度使用 `u32` 可以覆盖 4GB 以内的单行；超过该范围的异常行会被截断报错，避免分配不可控缓冲区。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineIndexEntry {
    /// 行首字节偏移。
    pub offset: u64,
    /// 不含换行符的行字节长度。
    pub byte_len: u32,
}

/// 常驻内存的行索引条目集合。
///
/// 业务意图：
/// - 用户反馈落盘索引拖动滚动条仍然卡顿，因此这里恢复为全量常驻内存索引。
/// - 索引不保存正文文本，只保存行首偏移和行字节长度；内存占用与行数线性相关。
#[derive(Clone, Debug)]
pub struct LineIndexEntries {
    /// 全量行索引。
    entries: Arc<Vec<LineIndexEntry>>,
}

impl LineIndexEntries {
    /// 创建常驻内存索引集合。
    fn new(entries: Vec<LineIndexEntry>) -> Self {
        Self {
            entries: Arc::new(entries),
        }
    }

    /// 返回索引行数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 读取指定行的索引条目。
    ///
    /// 边界条件：
    /// - 行号越界返回 `None`，避免虚拟列表或搜索结果旧行号导致 panic。
    pub fn get(&self, index: usize) -> Option<LineIndexEntry> {
        self.entries.get(index).copied()
    }

    /// 返回全量索引副本，测试用于验证换行边界。
    #[cfg(test)]
    pub fn read_all(&self) -> Vec<LineIndexEntry> {
        self.entries.as_ref().clone()
    }

    /// 顺序遍历索引条目，调用方可以在回调中提前停止。
    pub fn for_each<F>(&self, mut callback: F) -> io::Result<()>
    where
        F: FnMut(usize, LineIndexEntry) -> io::Result<bool>,
    {
        for (index, entry) in self.entries.iter().copied().enumerate() {
            if !callback(index, entry)? {
                break;
            }
        }
        Ok(())
    }
}

/// 行索引整体状态。
///
/// 业务意图：
/// - UI 工具条需要展示已扫描进度；测试也需要验证扫描覆盖换行边界。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineIndexState {
    /// 已建立索引的行数。
    pub indexed_lines: usize,
    /// 已扫描字节数。
    pub scanned_bytes: u64,
    /// 文件总字节数。
    pub total_bytes: u64,
    /// 是否已经完成整份文件扫描。
    pub complete: bool,
}

/// 已完成扫描的行索引。
///
/// 边界条件：
/// - 空文件会规范成一行空文本，保持和小文件内存模式一致，避免 UI 中出现 0 行虚拟列表。
#[derive(Clone, Debug)]
pub struct LineIndex {
    /// 每行偏移与长度。
    pub entries: LineIndexEntries,
    /// 扫描完成后的状态快照。
    pub state: LineIndexState,
    /// 字符数最多的候选行下标，用于横向测量。
    pub longest_line_index: usize,
}

/// 扫描本地文件并生成行索引。
///
/// 业务意图：
/// - 该函数只在后台线程执行，避免阻塞 GPUI 主线程。
/// - 读取过程中不解码内容，保证 GBK、Big5 等变长编码也能复用同一份行索引。
pub fn build_line_index(path: &Path) -> io::Result<LineIndex> {
    let file = File::open(path)?;
    let total_bytes = file.metadata()?.len();
    let mut reader = BufReader::with_capacity(LINE_INDEX_BLOCK_BYTES, file);
    let mut buffer = vec![0_u8; LINE_INDEX_BLOCK_BYTES];
    let mut entries = Vec::new();
    let mut scanned_bytes = 0_u64;
    let mut line_start = 0_u64;
    let mut pending_cr: Option<u64> = None;
    let mut longest_line_index = 0_usize;
    let mut longest_line_len = 0_u64;

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }

        for byte in &buffer[..read] {
            let offset = scanned_bytes;
            scanned_bytes += 1;

            if let Some(cr_offset) = pending_cr.take() {
                if *byte == b'\n' {
                    push_line(
                        &mut entries,
                        line_start,
                        cr_offset,
                        &mut longest_line_index,
                        &mut longest_line_len,
                    )?;
                    line_start = offset + 1;
                    continue;
                }

                push_line(
                    &mut entries,
                    line_start,
                    cr_offset,
                    &mut longest_line_index,
                    &mut longest_line_len,
                )?;
                line_start = cr_offset + 1;
            }

            if *byte == b'\r' {
                pending_cr = Some(offset);
            } else if *byte == b'\n' {
                push_line(
                    &mut entries,
                    line_start,
                    offset,
                    &mut longest_line_index,
                    &mut longest_line_len,
                )?;
                line_start = offset + 1;
            }
        }
    }

    if let Some(cr_offset) = pending_cr {
        push_line(
            &mut entries,
            line_start,
            cr_offset,
            &mut longest_line_index,
            &mut longest_line_len,
        )?;
        line_start = cr_offset + 1;
    }

    if total_bytes == 0 {
        entries.push(LineIndexEntry {
            offset: 0,
            byte_len: 0,
        });
    } else if line_start < total_bytes {
        push_line(
            &mut entries,
            line_start,
            total_bytes,
            &mut longest_line_index,
            &mut longest_line_len,
        )?;
    } else if line_start == total_bytes {
        // 小文件模式使用 `split('\n')`，文件以换行结尾时会保留最后一行空文本。
        // 分页模式也补一条空行索引，保证行数、搜索定位和跳转语义与小文件一致。
        entries.push(LineIndexEntry {
            offset: total_bytes,
            byte_len: 0,
        });
    } else if entries.is_empty() {
        entries.push(LineIndexEntry {
            offset: 0,
            byte_len: 0,
        });
    }
    let indexed_lines = entries.len();

    Ok(LineIndex {
        state: LineIndexState {
            indexed_lines,
            scanned_bytes,
            total_bytes,
            complete: true,
        },
        entries: LineIndexEntries::new(entries),
        longest_line_index,
    })
}

/// 将一行边界写入索引。
///
/// 边界条件：
/// - `end` 是行结束换行符之前的位置，因此长度为 `end - start`。
/// - 如果单行长度超过 `u32::MAX`，说明该日志行本身已达到数 GB，当前 UI 无法合理渲染，直接返回 I/O 错误。
fn push_line(
    entries: &mut Vec<LineIndexEntry>,
    start: u64,
    end: u64,
    longest_line_index: &mut usize,
    longest_line_len: &mut u64,
) -> io::Result<()> {
    let byte_len = end.saturating_sub(start);
    let byte_len_u32 = u32::try_from(byte_len).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "单行日志超过 4GB，无法建立可浏览行索引",
        )
    })?;
    if byte_len > *longest_line_len {
        *longest_line_len = byte_len;
        *longest_line_index = entries.len();
    }
    entries.push(LineIndexEntry {
        offset: start,
        byte_len: byte_len_u32,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    /// 为行索引测试创建独立临时文件，避免不同测试之间路径冲突。
    fn write_temp_log(content: &[u8]) -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        path.push(format!("logclinic-line-index-{unique}.log"));
        fs::write(&path, content).expect("测试临时日志应能写入");
        path
    }

    #[test]
    fn indexes_common_newline_styles() {
        let path = write_temp_log(b"a\nbb\r\nccc\rdddd");
        let index = build_line_index(&path).expect("应能索引混合换行日志");
        let lengths: Vec<u32> = index
            .entries
            .read_all()
            .iter()
            .map(|entry| entry.byte_len)
            .collect();
        assert_eq!(lengths, vec![1, 2, 3, 4]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn indexes_empty_file_as_single_empty_line() {
        let path = write_temp_log(b"");
        let index = build_line_index(&path).expect("空文件也应能建立索引");
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries.get(0).unwrap().byte_len, 0);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn ignores_trailing_newline_extra_empty_line() {
        let path = write_temp_log(b"a\n");
        let index = build_line_index(&path).expect("尾随换行应按现有小文件规则处理");
        assert_eq!(index.entries.len(), 2);
        assert_eq!(index.entries.get(0).unwrap().byte_len, 1);
        assert_eq!(index.entries.get(1).unwrap().byte_len, 0);
        let _ = fs::remove_file(path);
    }
}
