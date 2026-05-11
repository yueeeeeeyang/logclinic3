//! HPROF 输入文件读取模块。
//!
//! 业务意图：
//! - 该模块集中处理 HPROF 文件到随机访问字节切片的准备逻辑，优先使用只读 mmap，失败时回退整文件读取。
//! - parser 只消费字节切片并推进二进制解析，避免解析器继续承载跨平台文件 I/O 策略。
//!
//! 跨平台约束：
//! - macOS 和 Windows 都可能因为权限、文件系统或安全软件导致 mmap 创建失败；回退 `read_to_end` 保证可恢复。
//! - 只创建只读映射，不写入源 dump，也不把切片生命周期暴露到 `HprofInputBytes` 之外。

use std::{fs::File, io::Read, path::Path};

use memmap2::Mmap;

use super::*;

/// HPROF 输入字节来源。
///
/// 业务意图：
/// - 大 dump 优先使用 mmap 避免把文件内容复制到用户态缓冲区；mmap 不可用时回退 `Vec<u8>`，保证跨平台可恢复。
pub(super) enum HprofInputBytes {
    /// 只读内存映射。
    Mapped(Mmap),
    /// 回退路径下的整文件字节。
    Owned(Vec<u8>),
}

impl HprofInputBytes {
    /// 打开 HPROF 文件并准备可随机访问字节切片。
    pub(super) fn open(path: &Path) -> Result<Self, HprofError> {
        let mut file = File::open(path).map_err(|error| {
            HprofError::Io(format!("无法打开 HPROF 文件 {}：{}", path.display(), error))
        })?;

        // SAFETY:
        // - 只创建只读 mmap，不通过该映射写入文件。
        // - 映射对象持有到解析结束，返回的切片不会逃逸出 `HprofInputBytes` 生命周期。
        // - Windows 上如果文件被其它进程截断，读取 mmap 可能失败；失败路径会在系统层面表现为 I/O/进程错误，
        //   这是 mmap 的平台限制。这里保留 `read_to_end` 回退用于 mmap 创建失败的普通权限或平台场景。
        if let Ok(mapped) = unsafe { Mmap::map(&file) }
            && !mapped.is_empty()
        {
            return Ok(Self::Mapped(mapped));
        }

        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|error| {
            HprofError::Io(format!(
                "读取 HPROF 文件 {} 失败：{}",
                path.display(),
                error
            ))
        })?;
        Ok(Self::Owned(bytes))
    }

    /// 返回输入字节切片。
    pub(super) fn as_slice(&self) -> &[u8] {
        match self {
            Self::Mapped(mapped) => mapped,
            Self::Owned(bytes) => bytes,
        }
    }
}
