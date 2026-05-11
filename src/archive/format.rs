//! 压缩包格式识别子模块。
//!
//! 业务意图：
//! - 集中维护 ZIP/RAR/TAR/TAR.GZ/GZ/7Z 的扩展名识别、TAR.GZ 纯 TAR 纠偏和展示标签。
//! - 扫描、读取和物化入口共享同一枚举，避免同一种文件在不同链路中被识别为不同格式。
//!
//! 边界条件：
//! - 除 `.tar.gz/.tgz` 的轻量 tar header 纠偏外，不做魔数扫描；真正可读性仍由对应压缩库确认。

use std::{fs::File, io::Read, path::Path};

/// 当前阶段支持识别的压缩包格式。
///
/// 业务意图：
/// - 用户明确要求支持 zip、rar、tar.gz、gz 和 7z。
/// - 将格式识别集中在枚举上，避免文件扫描、UI 展示和测试各自硬编码扩展名。
///
/// 边界条件：
/// - 这里只基于文件名扩展名判断格式，除 `.tar.gz` 纠偏外不读取魔数；后续如需更强识别能力需要补充验收标准。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArchiveFormat {
    /// ZIP 压缩包。
    Zip,
    /// RAR 压缩包。
    Rar,
    /// 未压缩的 tar 归档。
    ///
    /// 业务意图：
    /// - 部分现场文件扩展名可能写成 `.tar.gz`，但真实内容是纯 tar；需要用内容探测纠正格式。
    /// - 单独支持 `.tar` 也能复用同一套目录树、分页和搜索链路。
    Tar,
    /// gzip 压缩的 tar 归档，包含 `.tar.gz` 和 `.tgz`。
    TarGz,
    /// 单个 gzip 压缩日志，包含 `.gz`。
    ///
    /// 业务意图：
    /// - GZIP 本身不是多文件归档，但用户会把日志压缩成 `app.log.gz` 后直接加载。
    /// - 将其建模为压缩格式可以复用目录树、tab 去重、搜索、另存为和大文件分页物化链路。
    Gzip,
    /// 7-Zip 压缩包。
    SevenZ,
}

impl ArchiveFormat {
    /// 根据路径文件名识别当前支持的压缩包格式。
    ///
    /// 业务意图：
    /// - 统一处理大小写扩展名和 `.tar.gz` 这种复合扩展名。
    /// - 普通日志文件返回 `None`，由加载层按单文件节点处理。
    ///
    /// 边界条件：
    /// - 文件名无法转为字符串时采用有损转换，仅用于扩展名判断，不影响真实路径打开。
    pub fn from_path(path: &Path) -> Option<Self> {
        let file_name = path.file_name()?.to_string_lossy().to_ascii_lowercase();

        if file_name.ends_with(".tar.gz") || file_name.ends_with(".tgz") {
            return Some(Self::TarGz);
        }

        if file_name.ends_with(".gz") {
            return Some(Self::Gzip);
        }

        if file_name.ends_with(".tar") {
            return Some(Self::Tar);
        }

        if file_name.ends_with(".zip") {
            return Some(Self::Zip);
        }

        if file_name.ends_with(".rar") {
            return Some(Self::Rar);
        }

        if file_name.ends_with(".7z") {
            return Some(Self::SevenZ);
        }

        None
    }

    /// 根据路径和文件头识别当前支持的压缩包格式。
    ///
    /// 业务意图：
    /// - 用户提供的真实样本 `2日志.tar.gz` 扩展名是 `.tar.gz`，但文件头是纯 tar。
    /// - 本地文件可以读取少量头部字节，因此加载树和单文件打开应优先相信内容探测结果，避免错误进入 gzip 解码器。
    ///
    /// 边界条件：
    /// - 只有 `.tar.gz/.tgz` 需要纠偏；ZIP、RAR、GZ、7Z 仍按扩展名交给各自库返回明确错误。
    /// - 头部读取失败时回退扩展名判断，让原有错误展示路径继续工作。
    pub(crate) fn from_file(path: &Path) -> Option<Self> {
        let declared = Self::from_path(path)?;
        if declared == Self::TarGz && path_looks_like_plain_tar(path) {
            return Some(Self::Tar);
        }
        Some(declared)
    }

    /// 对已读入内存的嵌套压缩包做 `.tar.gz` 内容纠偏。
    ///
    /// 业务意图：
    /// - ZIP/RAR/7Z/TAR 内部也可能存在扩展名写错的 `.tar.gz` 条目。
    /// - 目录树展开和点击读取都需要在进入 gzip 解码前检查 tar 头，避免嵌套场景继续报 “TAR.GZ 条目读取失败”。
    pub(crate) fn resolve_from_bytes(self, bytes: &[u8]) -> Self {
        if self == Self::TarGz && bytes_look_like_plain_tar(bytes) {
            Self::Tar
        } else {
            self
        }
    }

    /// 返回适合展示在目录树元信息中的格式名称。
    ///
    /// 边界条件：
    /// - 文案只用于当前中文界面，不作为序列化或匹配逻辑使用。
    pub fn label(self) -> &'static str {
        match self {
            Self::Zip => "ZIP",
            Self::Rar => "RAR",
            Self::Tar => "TAR",
            Self::TarGz => "TAR.GZ",
            Self::Gzip => "GZ",
            Self::SevenZ => "7Z",
        }
    }
}

/// 判断本地文件头是否符合常见 POSIX/GNU tar 归档。
///
/// 业务意图：
/// - tar 文件在偏移 257 处通常有 `ustar` 标记；读取这个小范围即可区分“纯 tar”与“gzip 包裹的 tar”。
/// - 该函数只用于纠正扩展名误写，不用于安全校验；真正的目录解析仍交给 `tar` crate。
fn path_looks_like_plain_tar(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut header = [0_u8; 262];
    file.read_exact(&mut header).is_ok() && bytes_look_like_plain_tar(&header)
}

/// 判断内存字节是否符合常见 POSIX/GNU tar 归档头。
pub(crate) fn bytes_look_like_plain_tar(bytes: &[u8]) -> bool {
    bytes.len() >= 262 && &bytes[257..262] == b"ustar"
}
