//! 日志来源定位模型模块。
//!
//! 业务意图：
//! - 日志查看器需要用同一套来源模型表达本地文件、压缩包成员、已物化成员和嵌套压缩包成员。
//! - 该模块只保存“如何重新读取原始字节”的定位信息，不读取文件、不解码文本、不构建 UI 目录树。
//! - 把来源模型从 `log_loader` 中抽出后，加载、正文读取、搜索、分页和 UI tab 可以共享同一内部类型，避免形成反向依赖。
//!
//! 关键约束：
//! - 路径可能包含非 UTF-8 字节，真实定位始终保存 `PathBuf`，只有展示和进程内去重时才做有损文本转换。
//! - 压缩包成员路径必须使用上游安全归一化后的 `/` 分隔文本，不能把原始条目名直接写入来源模型。
//! - 该模块是 crate 内部模型，不新增公开 API，也不改变配置文件或用户可见行为。

use std::{fs, path::Path, path::PathBuf};

use crate::archive::{ArchiveFormat, is_single_gzip_member_path, single_gzip_member_display_name};

/// 左侧目录树中文件节点的可打开来源。
///
/// 业务意图：
/// - 日志查看必须支持普通文件和压缩包内部文件，因此需要把多种读取入口统一成一个可复制的数据模型。
/// - 该结构只保存定位信息，不保存原始字节和解码文本，避免目录树扫描阶段提前读取大文件。
///
/// 边界条件：
/// - 本地文件来源不跟随符号链接；目录扫描阶段已经把符号链接作为不可打开节点展示。
/// - 压缩包成员路径使用安全归一化后的 `/` 分隔路径，不直接信任压缩包原始路径文本。
/// - 顶层 7Z 成员会在加载阶段物化到临时路径，但来源仍保留原始压缩包和内部成员路径，避免另存为、搜索范围等语义退化成本地临时文件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LogFileSource {
    /// 普通文件系统中的日志文件。
    LocalFile {
        /// 文件系统真实路径。
        ///
        /// 业务意图：
        /// - 后续打开文件时必须直接使用该路径读取原始字节。
        /// - 路径可能包含非 UTF-8 字节，因此模型保存 `PathBuf`，展示时才做有损转换。
        path: PathBuf,
    },

    /// 压缩包内部的日志文件条目。
    ArchiveMember {
        /// 压缩包文件本身的路径。
        archive_path: PathBuf,
        /// 压缩包格式，用于分发到对应的流式读取实现。
        archive_format: ArchiveFormat,
        /// 压缩包内部安全归一化后的成员路径。
        member_path: String,
    },

    /// 已物化到本地临时目录的压缩包成员。
    MaterializedArchiveMember {
        /// 原始压缩包文件路径，用于 tab 去重、搜索范围展示和另存为层级语义。
        archive_path: PathBuf,
        /// 原始压缩包格式；当前主要用于 7Z 加载阶段物化后的成员来源。
        archive_format: ArchiveFormat,
        /// 压缩包内部安全归一化后的成员路径，必须保留目录层级。
        member_path: String,
        /// 该成员已经流式写出的本地临时文件路径，后续读取和分页浏览都直接走此路径，避免反复顺序解压 7Z。
        temp_path: PathBuf,
    },

    /// 外层压缩包内的内层压缩包成员。
    NestedArchiveMember {
        /// 外层压缩包文件本身的路径。
        outer_archive_path: PathBuf,
        /// 外层压缩包格式，用于读取内层压缩包文件字节。
        outer_archive_format: ArchiveFormat,
        /// 外层压缩包中内层压缩包文件的安全归一化路径。
        archive_member_path: String,
        /// 内层压缩包格式，用于读取具体日志文件。
        nested_archive_format: ArchiveFormat,
        /// 内层压缩包中具体日志文件的安全归一化路径。
        nested_member_path: String,
    },
}

impl LogFileSource {
    /// 返回当前来源用于 tab 去重的稳定键。
    ///
    /// 业务意图：
    /// - 用户重复点击同一个文件时应切换到已有 tab，而不是打开多个重复 tab。
    /// - 本地文件按规范化路径去重，压缩包成员按“压缩包路径 + 格式 + 成员路径”去重。
    ///
    /// 边界条件：
    /// - `canonicalize` 可能因为权限或文件瞬间被删除而失败，此时回退到原始路径展示文本，保证 UI 仍可继续工作。
    /// - 该键只服务当前进程内去重，不写入磁盘，也不作为跨平台持久 ID。
    pub(crate) fn stable_key(&self) -> String {
        match self {
            Self::LocalFile { path } => format!("local:{}", normalized_path_for_key(path)),
            Self::ArchiveMember {
                archive_path,
                archive_format,
                member_path,
            } => format!(
                "archive:{}:{}:{}",
                archive_format.label(),
                normalized_path_for_key(archive_path),
                member_path
            ),
            Self::MaterializedArchiveMember {
                archive_path,
                archive_format,
                member_path,
                ..
            } => format!(
                "materialized-archive:{}:{}:{}",
                archive_format.label(),
                normalized_path_for_key(archive_path),
                member_path
            ),
            Self::NestedArchiveMember {
                outer_archive_path,
                outer_archive_format,
                archive_member_path,
                nested_archive_format,
                nested_member_path,
            } => format!(
                "nested-archive:{}:{}:{}:{}:{}",
                outer_archive_format.label(),
                normalized_path_for_key(outer_archive_path),
                archive_member_path,
                nested_archive_format.label(),
                nested_member_path
            ),
        }
    }

    /// 返回适合 tab 标题和错误文案使用的短名称。
    ///
    /// 业务意图：
    /// - tab 空间有限，普通文件展示文件名，压缩包成员展示成员文件名。
    /// - 当路径没有普通文件名时回退到完整路径，避免出现空标题。
    pub(crate) fn display_name(&self) -> String {
        match self {
            Self::LocalFile { path } => display_name_for_path(path),
            Self::ArchiveMember {
                archive_format: ArchiveFormat::TarGz | ArchiveFormat::Gzip,
                member_path,
                ..
            } if is_single_gzip_member_path(member_path) => {
                single_gzip_member_display_name(member_path)
            }
            Self::ArchiveMember { member_path, .. } => member_path
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or(member_path)
                .to_string(),
            Self::MaterializedArchiveMember {
                archive_format: ArchiveFormat::TarGz | ArchiveFormat::Gzip,
                member_path,
                ..
            } if is_single_gzip_member_path(member_path) => {
                single_gzip_member_display_name(member_path)
            }
            Self::MaterializedArchiveMember { member_path, .. } => member_path
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or(member_path)
                .to_string(),
            Self::NestedArchiveMember {
                nested_member_path, ..
            } => nested_member_path
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or(nested_member_path)
                .to_string(),
        }
    }
}

/// 为本地路径生成目录树根节点显示名。
///
/// 业务意图：
/// - 优先展示文件名或目录名，让左侧树在默认 300px 宽度下更易扫描。
/// - 如果路径没有文件名，则回退到完整路径字符串，保证错误节点仍可定位来源。
pub(crate) fn display_name_for_path(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

/// 将文件系统路径转成用于当前进程内去重的稳定文本。
///
/// 业务意图：
/// - tab 去重需要把同一个文件的不同相对写法归并到同一个键。
/// - `canonicalize` 可以消除 `.`、`..` 和符号链接后的差异；如果失败，仍要回退到原始路径，避免路径瞬时不可用导致 UI 崩溃。
///
/// 边界条件：
/// - 该函数只用于当前进程内的比较和展示，不作为安全边界，也不写入持久配置。
/// - 路径可能不是合法 UTF-8，因此这里使用有损转换；真实读取仍使用 `PathBuf`。
fn normalized_path_for_key(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}
