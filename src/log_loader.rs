//! 日志来源加载与目录树构建模块。
//!
//! 业务意图：
//! - 该模块只负责把用户选择的文件、目录或压缩包转换成左侧目录树需要的轻量结构。
//! - 当前阶段不读取日志正文、不做编码识别、不做搜索索引；但 7Z 会在加载阶段物化普通成员，避免后续点击反复顺序解压。
//! - UI 层只消费 `LoadedLogTree`，避免 GPUI 渲染代码直接依赖文件系统和压缩包格式细节。
//! - 加载层会为每个节点生成当前树内稳定 ID 和子节点标记，供 UI 实现展开、收起和虚拟列表渲染。
//!
//! 关键约束：
//! - 目录扫描必须完整递归，但不跟随符号链接，避免跨目录边界读取用户未明确选择的位置。
//! - ZIP/RAR/TAR.GZ 只读取目录项元数据；7Z 会写入 session 临时目录，加载结果必须携带清理路径，避免临时磁盘长期累积。
//! - 压缩包内部路径必须做安全归一化，绝对路径、盘符路径和 `..` 路径即使不落盘也不能作为正常树节点展示。

use std::{
    error::Error,
    fmt::{self, Display},
    fs::{self, File},
    io::{self, BufReader, BufWriter, Cursor, Read},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use flate2::read::GzDecoder;
use tar::Archive as TarArchive;
use walkdir::WalkDir;
use zip::ZipArchive;

/// 扫描嵌套压缩包目录时允许读入内存的最大压缩包成员大小。
///
/// 业务意图：
/// - 外层压缩包里的内层压缩包必须先拿到可 seek 的字节才能读取目录；限制大小可以避免加载树阶段因巨大内层压缩包占满内存。
/// - 超过该上限时仍保留内层压缩包本身作为普通文件节点，用户可以按既有分页/物化路径打开单文件压缩包。
const NESTED_ARCHIVE_SCAN_MAX_BYTES: u64 = 200 * 1024 * 1024;

/// 加载完成后提供给左侧目录树渲染的稳定数据结构。
///
/// 业务意图：
/// - `rows` 已经是按展示顺序扁平化后的树节点，UI 层无需再递归遍历文件系统或压缩包。
/// - `summary` 用于标题右侧展示加载结果摘要，避免 UI 层重复计算节点和错误数量。
///
/// 边界条件：
/// - 当前结构不保存真实文件句柄，也不缓存压缩包解码器；点击节点读取正文时会按 `LogFileSource` 重新打开。
/// - 展开状态属于 UI 会话状态，不写入该加载结果，避免业务数据和交互状态耦合。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedLogTree {
    /// 左侧树的标题补充信息。
    ///
    /// 业务意图：
    /// - 多个来源加载时使用统一摘要，让用户知道当前树来自真实选择而不是示例数据。
    /// - 摘要只表达节点规模和错误数量，不承诺日志条数或文件编码状态。
    pub summary: String,

    /// 按展示顺序扁平化后的目录树行。
    ///
    /// 边界条件：
    /// - 行内只包含展示所需的名称、层级、类型、元信息和可打开来源，不包含完整正文。
    /// - 可打开来源只出现在普通文件节点上，目录、压缩包根和错误节点不会伪装成可读取文件。
    pub rows: Vec<LogTreeRow>,

    /// 加载过程中收集到的非致命错误数量。
    ///
    /// 业务意图：
    /// - 权限失败、坏压缩包条目或不支持的特殊路径不应中断其它可读取节点。
    /// - UI 可以通过该字段决定是否展示额外的错误提示或诊断入口。
    pub error_count: usize,

    /// 当前加载结果创建的临时文件或目录。
    ///
    /// 业务意图：
    /// - 7Z 不适合按点击随机读取单个小文件，因此加载阶段会把内部普通成员物化到临时目录。
    /// - UI 在重新加载日志或应用退出时可以清理这些路径，避免临时磁盘长期累积。
    ///
    /// 边界条件：
    /// - 路径只属于当前进程 session，不跨启动复用；异常退出残留由启动期过期清理兜底。
    pub temporary_paths: Vec<PathBuf>,
}

/// 左侧目录树的一行真实加载节点。
///
/// 业务意图：
/// - 用统一结构表达普通文件、目录、压缩包、符号链接和错误节点，降低 UI 渲染分支复杂度。
/// - `id` 和 `has_children` 让 UI 可以只维护展开集合，而不用重新推导树结构或解析缩进层级。
/// - `depth` 由加载层计算，UI 层只根据层级缩进，不需要知道真实路径父子关系。
///
/// 边界条件：
/// - `id` 只在一次加载结果内部稳定，不跨加载、不跨进程持久化，后续不能把它当作文件来源 ID 使用。
/// - `source` 只存在于可打开的普通文件节点，目录或错误节点没有来源定位。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogTreeRow {
    /// 当前加载结果内部的稳定节点 ID。
    ///
    /// 业务意图：
    /// - UI 需要用稳定键维护展开/收起状态，不能使用行号，因为虚拟列表和折叠会改变可见行位置。
    /// - ID 由加载阶段按前序遍历分配，保证同一次加载中父节点、子节点和可见缓存都能引用同一节点。
    ///
    /// 边界条件：
    /// - ID 只对当前 `LoadedLogTree` 有意义；重新加载日志后即使路径相同也会重新分配。
    pub id: usize,

    /// 节点在目录树中的层级深度。
    ///
    /// 边界条件：
    /// - 根节点深度为 0，子节点逐级递增。
    /// - 深度只服务于当前扁平展示，不代表未来持久化模型。
    pub depth: usize,

    /// 节点显示名称。
    ///
    /// 业务意图：
    /// - 目录扫描使用文件名，压缩包内部使用安全归一化后的路径片段。
    /// - 发生错误时也会提供可读名称，帮助用户定位问题来源。
    pub label: String,

    /// 节点类型。
    ///
    /// 业务意图：
    /// - UI 用该字段选择图标和颜色，不应从文件名扩展名推断显示类型。
    pub kind: LogTreeEntryKind,

    /// 当前节点是否拥有子节点。
    ///
    /// 业务意图：
    /// - UI 根据该字段决定是否显示展开箭头，以及点击时是否切换展开状态。
    /// - 使用加载层计算结果比 UI 根据后续行深度推断更可靠，也减少虚拟列表滚动时的重复计算。
    pub has_children: bool,

    /// 节点补充信息。
    ///
    /// 边界条件：
    /// - 文件节点通常展示大小；目录节点展示递归包含的文件数量；错误节点展示简短错误类别。
    /// - 这里不展示绝对路径，避免窄面板被长路径挤压。
    pub meta: Option<String>,

    /// 错误节点或降级节点的详细说明。
    ///
    /// 业务意图：
    /// - 当前 UI 只展示简短元信息，后续可以把该字段接入悬浮提示或状态面板。
    /// - 非错误节点通常为 `None`。
    pub error_message: Option<String>,

    /// 当前节点对应的日志正文来源。
    ///
    /// 业务意图：
    /// - 文件节点点击后必须能准确知道该读取本地文件，还是压缩包内部成员。
    /// - 来源模型由加载层生成，UI 层不能从展示名称、缩进或父节点文本反推真实路径。
    ///
    /// 边界条件：
    /// - 只有 `LogTreeEntryKind::File` 节点可以携带来源；其它节点保持 `None`。
    /// - 来源只表示“如何重新读取原始字节”，不缓存文件句柄、解码结果或压缩包 reader。
    pub source: Option<LogFileSource>,
}

/// 目录树节点的业务类型。
///
/// 业务意图：
/// - 类型枚举让 UI 渲染保持稳定，不依赖扩展名、颜色或错误文案等易变展示细节。
/// - 压缩包作为独立类型展示，方便用户区分真实目录和归档文件内部结构。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogTreeEntryKind {
    /// 普通目录节点。
    Directory,
    /// 普通文件节点。
    File,
    /// 压缩包根节点。
    Archive,
    /// 符号链接节点。
    Symlink,
    /// 加载、权限或路径安全错误节点。
    Error,
}

/// 当前阶段支持识别的压缩包格式。
///
/// 业务意图：
/// - 用户明确要求支持 zip、rar、tar.gz 和 7z。
/// - 将格式识别集中在枚举上，避免文件扫描、UI 展示和测试各自硬编码扩展名。
///
/// 边界条件：
/// - 这里只基于文件名扩展名判断格式，不读取魔数；后续如需更强识别能力需要补充验收标准。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArchiveFormat {
    /// ZIP 压缩包。
    Zip,
    /// RAR 压缩包。
    Rar,
    /// gzip 压缩的 tar 归档，包含 `.tar.gz` 和 `.tgz`。
    TarGz,
    /// 7-Zip 压缩包。
    SevenZ,
}

/// 左侧目录树中文件节点的可打开来源。
///
/// 业务意图：
/// - 日志查看必须支持普通文件和压缩包内部文件，因此需要把两种读取入口统一成一个可复制的数据模型。
/// - 该结构只保存定位信息，不保存原始字节和解码文本，避免目录树扫描阶段提前读取大文件。
///
/// 边界条件：
/// - 本地文件来源不跟随符号链接；目录扫描阶段已经把符号链接作为不可打开节点展示。
/// - 压缩包成员路径使用安全归一化后的 `/` 分隔路径，不直接信任压缩包原始路径文本。
/// - 顶层 7Z 成员会在加载阶段物化到临时路径，但来源仍保留原始压缩包和内部成员路径，避免另存为、搜索范围等语义退化成本地临时文件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogFileSource {
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
    pub fn stable_key(&self) -> String {
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
    pub fn display_name(&self) -> String {
        match self {
            Self::LocalFile { path } => display_name_for_path(path),
            Self::ArchiveMember { member_path, .. } => member_path
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or(member_path)
                .to_string(),
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

impl ArchiveFormat {
    /// 根据路径文件名识别当前支持的压缩包格式。
    ///
    /// 业务意图：
    /// - 统一处理大小写扩展名和 `.tar.gz` 这种复合扩展名。
    /// - 普通日志文件返回 `None`，由加载层按单文件节点处理。
    ///
    /// 边界条件：
    /// - 文件名无法转为字符串时采用有损转换，仅用于扩展名判断，不影响真实路径打开。
    /// - 当前不把 `.tar` 视为支持格式，因为用户本轮明确要求的是 `.tar.gz`。
    pub fn from_path(path: &Path) -> Option<Self> {
        let file_name = path.file_name()?.to_string_lossy().to_ascii_lowercase();

        if file_name.ends_with(".tar.gz") || file_name.ends_with(".tgz") {
            return Some(Self::TarGz);
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

    /// 返回适合展示在目录树元信息中的格式名称。
    ///
    /// 边界条件：
    /// - 文案只用于当前中文界面，不作为序列化或匹配逻辑使用。
    pub fn label(self) -> &'static str {
        match self {
            Self::Zip => "ZIP",
            Self::Rar => "RAR",
            Self::TarGz => "TAR.GZ",
            Self::SevenZ => "7Z",
        }
    }
}

/// 日志加载阶段的致命错误。
///
/// 业务意图：
/// - 大多数单个节点错误会被转为目录树错误节点并继续加载。
/// - 只有整个加载流程无法形成结果时才返回该错误，例如应用层传入空路径以外的异常状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogLoadError {
    /// 面向开发者和 UI 的中文错误说明。
    message: String,
}

impl LogLoadError {
    /// 创建一个新的加载错误。
    ///
    /// 边界条件：
    /// - 该错误类型当前只保存文本，不包装底层错误对象，避免在跨线程 UI 更新时引入复杂生命周期。
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for LogLoadError {
    /// 将加载错误格式化为中文可读文本。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for LogLoadError {}

/// 加载用户选择的一个或多个日志来源。
///
/// 业务意图：
/// - 统一处理文件、目录和压缩包，生成左侧目录树可直接渲染的扁平行。
/// - 多个来源使用虚拟根节点包裹，避免不同来源的同名根目录混在一起。
///
/// 关键约束：
/// - 目录完整递归扫描，但不跟随符号链接。
/// - 压缩包只读取条目元数据，不实际解压。
/// - 单个来源失败会变成错误节点，其它来源继续加载。
///
/// 边界条件：
/// - 空选择通常来自用户取消对话框；这里返回空结果而不是错误，方便 UI 保持当前状态。
/// - 当前不限制节点数量，极大目录会完整扫描；后续如需上限必须由业务规则明确。
pub fn load_log_sources(paths: Vec<PathBuf>) -> Result<LoadedLogTree, LogLoadError> {
    if paths.is_empty() {
        return Ok(LoadedLogTree {
            summary: "未选择".to_string(),
            rows: Vec::new(),
            error_count: 0,
            temporary_paths: Vec::new(),
        });
    }

    let mut error_count = 0usize;
    let mut temporary_paths = Vec::new();
    let mut root = if paths.len() == 1 {
        load_single_source(&paths[0], &mut error_count, &mut temporary_paths)
    } else {
        let mut virtual_root = TreeNode::new(
            format!("已加载 {} 个来源", paths.len()),
            LogTreeEntryKind::Directory,
        );

        for path in &paths {
            virtual_root.children.push(load_single_source(
                path,
                &mut error_count,
                &mut temporary_paths,
            ));
        }

        virtual_root
    };

    root.sort_recursively();

    let mut rows = Vec::new();
    let mut next_row_id = 0usize;
    root.flatten_into(0, &mut next_row_id, &mut rows);

    let summary = if error_count == 0 {
        format!("{} 个节点", rows.len())
    } else {
        format!("{} 个节点，{} 个错误", rows.len(), error_count)
    };

    Ok(LoadedLogTree {
        summary,
        rows,
        error_count,
        temporary_paths,
    })
}

/// 加载单个用户选择的来源路径。
///
/// 业务意图：
/// - 按文件系统元数据区分目录、普通文件和压缩包。
/// - 单来源失败时仍返回一个错误节点，让左侧树能明确告诉用户哪个来源不可用。
///
/// 边界条件：
/// - 符号链接来源会作为符号链接节点展示，不跟随目标。
/// - `symlink_metadata` 失败通常代表路径不存在或权限不足，此时转为错误节点。
fn load_single_source(
    path: &Path,
    error_count: &mut usize,
    temporary_paths: &mut Vec<PathBuf>,
) -> TreeNode {
    let label = display_name_for_path(path);

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            *error_count += 1;
            return TreeNode::error(label, "读取失败", format!("无法读取来源元数据：{}", error));
        }
    };

    if metadata.file_type().is_symlink() {
        return TreeNode {
            label,
            kind: LogTreeEntryKind::Symlink,
            meta: Some("符号链接".to_string()),
            error_message: None,
            source: None,
            children: Vec::new(),
        };
    }

    if metadata.is_dir() {
        let mut root = TreeNode::new(label, LogTreeEntryKind::Directory);
        scan_directory(path, &mut root, error_count);
        return root;
    }

    if metadata.is_file() {
        if let Some(format) = ArchiveFormat::from_path(path) {
            let mut root = TreeNode {
                label,
                kind: LogTreeEntryKind::Archive,
                meta: Some(format.label().to_string()),
                error_message: None,
                source: None,
                children: Vec::new(),
            };

            if let Err(error) = scan_archive(path, format, &mut root, error_count, temporary_paths)
            {
                *error_count += 1;
                root.children.push(TreeNode::error(
                    "压缩包读取失败",
                    "读取失败",
                    error.to_string(),
                ));
            }

            return root;
        }

        return TreeNode {
            label,
            kind: LogTreeEntryKind::File,
            meta: Some(format_byte_size(metadata.len())),
            error_message: None,
            source: Some(LogFileSource::LocalFile {
                path: path.to_path_buf(),
            }),
            children: Vec::new(),
        };
    }

    *error_count += 1;
    TreeNode::error(label, "不支持", "当前来源不是普通文件、目录或符号链接")
}

/// 扫描普通目录并把结果写入树节点。
///
/// 业务意图：
/// - 用户选择目录后，需要完整展示目录内部文件结构，作为后续选择日志文件的基础。
/// - 使用 `walkdir` 保持跨平台递归行为一致，并显式关闭符号链接跟随。
///
/// 关键约束：
/// - 不跟随符号链接，避免扫描越过用户选择的目录边界。
/// - 遇到权限错误或读取失败时添加错误节点并继续处理其它可访问路径。
///
/// 边界条件：
/// - 当前不跳过隐藏文件、不按扩展名过滤，也不限制深度或节点数量。
fn scan_directory(root_path: &Path, root: &mut TreeNode, error_count: &mut usize) {
    for entry in WalkDir::new(root_path)
        .follow_links(false)
        .into_iter()
        .skip(1)
    {
        match entry {
            Ok(entry) => {
                let relative_path = match entry.path().strip_prefix(root_path) {
                    Ok(path) => path,
                    Err(error) => {
                        *error_count += 1;
                        root.add_error_child(
                            "路径归属异常",
                            "路径错误",
                            format!("目录项不在所选根目录下：{}", error),
                        );
                        continue;
                    }
                };

                let segments = path_components_for_tree(relative_path);
                if segments.is_empty() {
                    continue;
                }

                let file_type = entry.file_type();
                if file_type.is_dir() {
                    root.add_leaf_path(&segments, LogTreeEntryKind::Directory, None, None, None);
                } else if file_type.is_file() {
                    let size = entry.metadata().ok().map(|metadata| metadata.len());
                    root.add_leaf_path(
                        &segments,
                        LogTreeEntryKind::File,
                        size.map(format_byte_size),
                        Some(LogFileSource::LocalFile {
                            path: entry.path().to_path_buf(),
                        }),
                        None,
                    );
                } else if file_type.is_symlink() {
                    root.add_leaf_path(
                        &segments,
                        LogTreeEntryKind::Symlink,
                        Some("符号链接".to_string()),
                        None,
                        None,
                    );
                } else {
                    *error_count += 1;
                    root.add_leaf_path(
                        &segments,
                        LogTreeEntryKind::Error,
                        Some("不支持".to_string()),
                        None,
                        Some("当前目录项不是普通文件、目录或符号链接".to_string()),
                    );
                }
            }
            Err(error) => {
                *error_count += 1;
                let label = error
                    .path()
                    .and_then(Path::file_name)
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "目录项读取失败".to_string());
                root.add_error_child(label, "读取失败", error.to_string());
            }
        }
    }
}

/// 按压缩包格式分发扫描逻辑。
///
/// 业务意图：
/// - 格式相关 API 差异集中在本函数附近，调用方只关心压缩包根节点和错误处理。
/// - ZIP/RAR/TAR.GZ 默认读取条目列表；遇到需要路径型 API 的内层压缩包时，会先物化到临时目录再扫描。
/// - 7Z 为改善点击内部小文件的速度，会在这里顺序物化成员到临时目录。
fn scan_archive(
    path: &Path,
    format: ArchiveFormat,
    root: &mut TreeNode,
    error_count: &mut usize,
    temporary_paths: &mut Vec<PathBuf>,
) -> Result<(), LogLoadError> {
    match format {
        ArchiveFormat::Zip => scan_zip_archive(path, root, error_count, temporary_paths),
        ArchiveFormat::Rar => scan_rar_archive(path, root, error_count, temporary_paths),
        ArchiveFormat::TarGz => scan_tar_gz_archive(path, root, error_count, temporary_paths),
        ArchiveFormat::SevenZ => scan_7z_archive(path, root, error_count, temporary_paths),
    }
}

/// 扫描 ZIP 压缩包目录项。
///
/// 业务意图：
/// - `zip` crate 支持按索引读取条目元数据，适合当前只构建目录树的需求。
/// - 读取条目名称、目录标记和未压缩大小即可，不读取正文内容。
///
/// 边界条件：
/// - ZIP 条目名可能包含不安全路径，必须交给 `add_archive_entry` 统一校验。
fn scan_zip_archive(
    path: &Path,
    root: &mut TreeNode,
    error_count: &mut usize,
    temporary_paths: &mut Vec<PathBuf>,
) -> Result<(), LogLoadError> {
    let file = File::open(path).map_err(|error| {
        LogLoadError::new(format!("无法打开 ZIP 压缩包 {}：{}", path.display(), error))
    })?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|error| LogLoadError::new(format!("无法读取 ZIP 目录：{}", error)))?;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| {
            LogLoadError::new(format!("无法读取 ZIP 条目 {}：{}", index, error))
        })?;
        let raw_name = entry.name().to_string();
        let is_directory = entry.is_dir();
        let size = entry.size();
        if !is_directory
            && let Some(nested_format) = ArchiveFormat::from_path(Path::new(&raw_name))
            && let Some(nested_tree) = read_nested_archive_tree_from_reader(
                &mut entry,
                Some(size),
                nested_format,
                path,
                ArchiveFormat::Zip,
                &raw_name,
                error_count,
                temporary_paths,
            )
        {
            add_nested_archive_tree(root, &raw_name, nested_tree, error_count);
            continue;
        }

        add_archive_entry(
            root,
            path,
            ArchiveFormat::Zip,
            &raw_name,
            is_directory,
            Some(size),
            error_count,
        );
    }

    Ok(())
}

/// 扫描 RAR 压缩包目录项。
///
/// 业务意图：
/// - `unrar` crate 封装 RARLAB unrar 库的列表能力，可以在不解压文件的前提下读取条目元数据。
/// - 普通条目只使用列表模式；当条目本身是压缩包时，为了判断是否应作为目录展开，会把该条目物化到临时文件再扫描。
///
/// 边界条件：
/// - 加密文件没有密码规则，当前以错误节点展示，后续需要用户确认密码输入和缓存策略后再支持。
/// - `unrar` 依赖底层 unrar 实现，跨平台构建如遇工具链问题需要在对应平台单独验收。
fn scan_rar_archive(
    path: &Path,
    root: &mut TreeNode,
    error_count: &mut usize,
    temporary_paths: &mut Vec<PathBuf>,
) -> Result<(), LogLoadError> {
    let archive = unrar::Archive::new(path)
        .open_for_listing()
        .map_err(|error| LogLoadError::new(format!("无法打开 RAR 目录：{}", error)))?;
    let mut materialized_root: Option<PathBuf> = None;

    for entry in archive {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                *error_count += 1;
                root.add_error_child("RAR 条目读取失败", "读取失败", error.to_string());
                continue;
            }
        };

        let raw_name = entry.filename.to_string_lossy();
        if entry.is_encrypted() {
            *error_count += 1;
            root.add_archive_error_entry(&raw_name, "加密条目", "加密 RAR 条目暂不支持读取");
            continue;
        }

        if let Some(nested_format) =
            nested_archive_scan_format(&raw_name, false, entry.unpacked_size)
        {
            let Some(nested_member_path) =
                normalized_rar_nested_archive_member_path(&raw_name, false, entry.unpacked_size)
            else {
                *error_count += 1;
                root.add_archive_error_entry(&raw_name, "非法路径", "压缩包成员路径非法");
                continue;
            };
            match materialize_rar_member_for_nested_scan(
                path,
                &nested_member_path,
                &mut materialized_root,
                temporary_paths,
            ) {
                Ok(temp_path) => {
                    if let Some(nested_tree) = read_nested_archive_tree_from_path(
                        &temp_path,
                        nested_format,
                        path,
                        ArchiveFormat::Rar,
                        &nested_member_path,
                        error_count,
                        temporary_paths,
                    ) {
                        add_nested_archive_tree(
                            root,
                            &nested_member_path,
                            nested_tree,
                            error_count,
                        );
                        continue;
                    }
                }
                Err(error) => {
                    *error_count += 1;
                    root.add_archive_error_entry(
                        &raw_name,
                        "内层压缩包读取失败",
                        error.to_string(),
                    );
                    continue;
                }
            }
        }

        add_archive_entry(
            root,
            path,
            ArchiveFormat::Rar,
            &raw_name,
            entry.is_directory(),
            Some(entry.unpacked_size),
            error_count,
        );
    }

    Ok(())
}

/// 扫描 tar.gz 或 tgz 压缩包目录项。
///
/// 业务意图：
/// - `flate2` 负责 gzip 解码，`tar` 负责遍历 tar 头部条目，两者组合能流式读取归档目录。
/// - 当前读取条目头和路径，不把条目内容写入磁盘。
///
/// 边界条件：
/// - tar 路径可能包含平台分隔符或危险片段，必须交给 `add_archive_entry` 校验。
fn scan_tar_gz_archive(
    path: &Path,
    root: &mut TreeNode,
    error_count: &mut usize,
    temporary_paths: &mut Vec<PathBuf>,
) -> Result<(), LogLoadError> {
    let file = File::open(path).map_err(|error| {
        LogLoadError::new(format!(
            "无法打开 TAR.GZ 压缩包 {}：{}",
            path.display(),
            error
        ))
    })?;
    let decoder = GzDecoder::new(BufReader::new(file));
    let mut archive = TarArchive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| LogLoadError::new(format!("无法读取 TAR.GZ 目录：{}", error)))?;

    for entry in entries {
        let mut entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                *error_count += 1;
                root.add_error_child("TAR.GZ 条目读取失败", "读取失败", error.to_string());
                continue;
            }
        };

        let entry_path = match entry.path() {
            Ok(path) => path,
            Err(error) => {
                *error_count += 1;
                root.add_error_child("TAR.GZ 路径读取失败", "路径错误", error.to_string());
                continue;
            }
        };
        let raw_name = entry_path.to_string_lossy().to_string();
        let is_directory = entry.header().entry_type().is_dir();
        let size = entry.size();
        if !is_directory
            && let Some(nested_format) = ArchiveFormat::from_path(Path::new(&raw_name))
            && let Some(nested_tree) = read_nested_archive_tree_from_reader(
                &mut entry,
                Some(size),
                nested_format,
                path,
                ArchiveFormat::TarGz,
                &raw_name,
                error_count,
                temporary_paths,
            )
        {
            add_nested_archive_tree(root, &raw_name, nested_tree, error_count);
            continue;
        }

        add_archive_entry(
            root,
            path,
            ArchiveFormat::TarGz,
            &raw_name,
            is_directory,
            Some(size),
            error_count,
        );
    }

    Ok(())
}

/// 扫描 7z 压缩包目录项。
///
/// 业务意图：
/// - 7Z，尤其 solid 7Z，随机打开内部小文件时必须顺序解压前置条目，会导致每次点击都等待很久。
/// - 这里在加载目录树阶段把普通成员顺序物化到 session 临时目录，后续左侧树点击直接读取本地临时文件。
///
/// 边界条件：
/// - 加密 7z 或损坏头部会返回读取错误，当前展示为压缩包根节点下的错误节点。
/// - 物化会占用磁盘空间；路径记录在 `temporary_paths` 中，由 UI 在重新加载或启动期过期清理时释放。
fn scan_7z_archive(
    path: &Path,
    root: &mut TreeNode,
    error_count: &mut usize,
    temporary_paths: &mut Vec<PathBuf>,
) -> Result<(), LogLoadError> {
    let mut reader = sevenz_rust::SevenZReader::open(path, sevenz_rust::Password::empty())
        .map_err(|error| LogLoadError::new(format!("无法读取 7Z 内容：{}", error)))?;
    let materialized_root = sevenz_materialized_root(path)?;
    fs::create_dir_all(&materialized_root).map_err(|error| {
        LogLoadError::new(format!(
            "无法创建 7Z 临时物化目录 {}：{}",
            materialized_root.display(),
            error
        ))
    })?;
    temporary_paths.push(materialized_root.clone());

    let scan_result = reader
        .for_each_entries(|entry, entry_reader| {
            let raw_name = entry.name().to_string();
            if entry.is_directory() {
                add_archive_directory_entry(root, &raw_name, error_count);
                return Ok(true);
            }

            let segments = match split_archive_entry_path(&raw_name) {
                Ok(segments) => segments,
                Err(reason) => {
                    // 7Z 条目即使路径非法也必须消费当前 reader；尤其是 solid archive，
                    // 不 drain 会破坏后续条目的顺序解压状态。该条目作为错误节点展示，其它合法文件继续加载。
                    *error_count += 1;
                    root.add_archive_error_entry(&raw_name, "非法路径", reason);
                    io::copy(entry_reader, &mut io::sink()).map_err(sevenz_rust::Error::io)?;
                    return Ok(true);
                }
            };
            let temp_path = materialized_7z_member_path(&materialized_root, &segments);
            if let Some(parent) = temp_path.parent() {
                fs::create_dir_all(parent).map_err(sevenz_rust::Error::io)?;
            }
            let mut writer =
                BufWriter::new(File::create(&temp_path).map_err(sevenz_rust::Error::io)?);
            io::copy(entry_reader, &mut writer).map_err(sevenz_rust::Error::io)?;

            if let Some(nested_format) = nested_archive_scan_format(&raw_name, false, entry.size)
                && let Some(nested_tree) = read_nested_archive_tree_from_path(
                    &temp_path,
                    nested_format,
                    path,
                    ArchiveFormat::SevenZ,
                    &raw_name,
                    error_count,
                    temporary_paths,
                )
            {
                add_nested_archive_tree(root, &raw_name, nested_tree, error_count);
                return Ok(true);
            }

            add_materialized_7z_file_entry(root, path, &segments, entry.size, temp_path);
            Ok(true)
        })
        .map_err(|error| LogLoadError::new(format!("读取 7Z 内容失败：{}", error)));
    if scan_result.is_err() {
        let _ = fs::remove_dir_all(&materialized_root);
    }
    scan_result?;

    Ok(())
}

/// 判断压缩包条目是否适合在加载树阶段尝试扫描为内层压缩包。
///
/// 业务意图：
/// - ZIP、TAR.GZ 和 7Z 内层压缩包可以基于内存字节读取目录，因此小文件候选项可以直接展开成目录。
/// - RAR 当前不能从内存安全扫描，超大内层压缩包也不能在构建目录树时读入内存，两类都保留为普通文件节点。
fn nested_archive_scan_format(
    raw_name: &str,
    is_directory: bool,
    size: u64,
) -> Option<ArchiveFormat> {
    if is_directory || size > NESTED_ARCHIVE_SCAN_MAX_BYTES {
        return None;
    }
    ArchiveFormat::from_path(Path::new(raw_name))
}

/// 为扫描内层压缩包创建会话级临时根目录。
///
/// 业务意图：
/// - RAR 的目录扫描和读取 API 都需要真实文件路径，无法直接从内存 reader 扫描。
/// - 外层 RAR 或 ZIP/TAR.GZ 中的内层 RAR 需要先物化为临时文件，再复用 `scan_archive`。
///
/// 边界条件：
/// - 临时目录加入 `temporary_paths`，由 UI 在重新加载或退出时清理。
/// - 目录名包含时间戳和来源标签，避免同一进程多次加载同名归档互相覆盖。
fn nested_archive_scan_materialized_root(label: &str) -> Result<PathBuf, LogLoadError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| LogLoadError::new(format!("无法生成内层压缩包临时时间戳：{}", error)))?
        .as_nanos();
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
    Ok(std::env::temp_dir()
        .join("LogClinic")
        .join("large-log-cache")
        .join(format!("session-{}", std::process::id()))
        .join(format!("nested-archive-{}-{}", nanos, safe_label)))
}

/// 计算物化后的内层压缩包文件路径。
fn materialized_nested_archive_path(root: &Path, raw_name: &str) -> Result<PathBuf, LogLoadError> {
    let segments = split_archive_entry_path(raw_name)
        .map_err(|reason| LogLoadError::new(format!("内层压缩包路径非法，无法物化：{}", reason)))?;
    let mut path = root.to_path_buf();
    for segment in segments {
        path.push(segment);
    }
    Ok(path)
}

/// 返回 RAR 嵌套压缩包扫描时使用的安全成员路径。
///
/// 业务意图：
/// - RAR 条目名可能来自 Windows 压缩工具并使用反斜杠；物化、读取和挂载树必须使用同一个 `/` 分隔路径。
/// - 该函数把“识别为内层压缩包”和“路径安全归一化”放在同一处，避免不同调用点使用原始路径导致匹配失败。
fn normalized_rar_nested_archive_member_path(
    raw_name: &str,
    is_directory: bool,
    size: u64,
) -> Option<String> {
    nested_archive_scan_format(raw_name, is_directory, size)?;
    normalize_archive_member_path(raw_name).ok()
}

/// 将 RAR 成员物化为本地文件，供内层压缩包目录扫描使用。
///
/// 业务意图：
/// - `unrar` 的列表接口只能告诉我们条目名和大小，不能直接把条目 reader 交给 ZIP/7Z/TAR 扫描。
/// - 只在条目扩展名已经确认是受支持压缩包时调用，避免对普通日志额外解压。
fn materialize_rar_member_for_nested_scan(
    archive_path: &Path,
    member_path: &str,
    materialized_root: &mut Option<PathBuf>,
    temporary_paths: &mut Vec<PathBuf>,
) -> Result<PathBuf, LogLoadError> {
    if materialized_root.is_none() {
        let label = archive_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "archive.rar".to_string());
        let root = nested_archive_scan_materialized_root(&label)?;
        fs::create_dir_all(&root).map_err(|error| {
            LogLoadError::new(format!(
                "无法创建内层压缩包临时目录 {}：{}",
                root.display(),
                error
            ))
        })?;
        temporary_paths.push(root.clone());
        *materialized_root = Some(root);
    }

    let root = materialized_root
        .as_ref()
        .ok_or_else(|| LogLoadError::new("内部错误：内层压缩包临时目录未初始化"))?;
    let temp_path = materialized_nested_archive_path(root, member_path)?;
    if let Some(parent) = temp_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            LogLoadError::new(format!(
                "无法创建内层压缩包父目录 {}：{}",
                parent.display(),
                error
            ))
        })?;
    }

    let mut archive = unrar::Archive::new(archive_path)
        .open_for_processing()
        .map_err(|error| LogLoadError::new(format!("无法打开 RAR 内容：{}", error)))?;
    while let Some(header) = archive
        .read_header()
        .map_err(|error| LogLoadError::new(format!("无法读取 RAR 条目：{}", error)))?
    {
        let raw_name = header.entry().filename.to_string_lossy();
        let normalized = match normalize_archive_member_path(&raw_name) {
            Ok(path) => path,
            Err(_) => {
                archive = header.skip().map_err(|error| {
                    LogLoadError::new(format!("无法跳过非法 RAR 条目：{}", error))
                })?;
                continue;
            }
        };
        if normalized != member_path {
            archive = header
                .skip()
                .map_err(|error| LogLoadError::new(format!("无法跳过 RAR 条目：{}", error)))?;
            continue;
        }
        if header.entry().is_encrypted() {
            return Err(LogLoadError::new(format!(
                "RAR 成员 {} 已加密，暂不支持展开为目录",
                member_path
            )));
        }
        if header.entry().is_directory() {
            return Err(LogLoadError::new(format!(
                "RAR 成员 {} 是目录，不能作为内层压缩包扫描",
                member_path
            )));
        }
        header
            .extract_to(&temp_path)
            .map_err(|error| LogLoadError::new(format!("无法物化 RAR 内层压缩包：{}", error)))?;
        return Ok(temp_path);
    }

    Err(LogLoadError::new(format!(
        "RAR 压缩包中未找到内层压缩包 {}",
        member_path
    )))
}

/// 为当前顶层 7Z 创建会话级物化根目录。
///
/// 业务意图：
/// - 7Z 内部成员被转成本地临时文件后，左侧树后续点击无需再次顺序解压整个归档。
/// - 目录名包含进程 ID、时间戳和压缩包文件名，降低同一会话重复加载同名压缩包时的冲突概率。
fn sevenz_materialized_root(path: &Path) -> Result<PathBuf, LogLoadError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| LogLoadError::new(format!("无法生成 7Z 临时目录时间戳：{}", error)))?
        .as_nanos();
    let label = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive.7z".to_string())
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    Ok(std::env::temp_dir()
        .join("LogClinic")
        .join("large-log-cache")
        .join(format!("session-{}", std::process::id()))
        .join(format!("sevenz-tree-{}-{}", nanos, label)))
}

/// 计算 7Z 成员的临时物化路径。
///
/// 边界条件：
/// - 成员名必须先经过压缩包路径安全归一化，拒绝绝对路径、盘符和 `..`，避免写出临时根目录。
fn materialized_7z_member_path(root: &Path, segments: &[String]) -> PathBuf {
    let mut path = root.to_path_buf();
    for segment in segments {
        path.push(segment);
    }
    path
}

/// 添加已经物化到本地临时目录的 7Z 普通文件节点。
///
/// 业务意图：
/// - 物化后的文件读取走普通本地临时路径，但来源语义仍保留原始 7Z 成员路径。
/// - 另存为和搜索结果需要内部路径层级，不能把临时文件名当作用户选择的真实来源。
fn add_materialized_7z_file_entry(
    root: &mut TreeNode,
    archive_path: &Path,
    segments: &[String],
    size: u64,
    temp_path: PathBuf,
) {
    let member_path = join_archive_segments(segments);
    root.add_leaf_path(
        segments,
        LogTreeEntryKind::File,
        Some(format_byte_size(size)),
        Some(LogFileSource::MaterializedArchiveMember {
            archive_path: archive_path.to_path_buf(),
            archive_format: ArchiveFormat::SevenZ,
            member_path,
            temp_path,
        }),
        None,
    );
}

/// 添加 7Z 中显式出现的目录节点。
///
/// 边界条件：
/// - 一些 7Z 文件没有显式目录条目，只在文件路径中隐含目录；这种情况由 `add_leaf_path` 自动补齐。
fn add_archive_directory_entry(root: &mut TreeNode, raw_name: &str, error_count: &mut usize) {
    let segments = match split_archive_entry_path(raw_name) {
        Ok(segments) => segments,
        Err(reason) => {
            *error_count += 1;
            root.add_archive_error_entry(raw_name, "非法路径", reason);
            return;
        }
    };
    root.add_leaf_path(&segments, LogTreeEntryKind::Directory, None, None, None);
}

/// 从已经物化成本地文件的内层压缩包读取目录树。
///
/// 业务意图：
/// - 顶层 7Z 成员已经落到临时目录后，扫描内层压缩包不再需要再次从顶层 7Z 顺序解压。
/// - 如果内层压缩包包含多个普通文件，继续作为目录挂载；单文件压缩包仍按既有规则作为文件本身打开。
fn read_nested_archive_tree_from_path(
    archive_path: &Path,
    nested_format: ArchiveFormat,
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    error_count: &mut usize,
    temporary_paths: &mut Vec<PathBuf>,
) -> Option<TreeNode> {
    let segments = split_archive_entry_path(archive_member_path).ok()?;
    let label = segments.last()?.clone();
    let mut nested_root = TreeNode::new(label, LogTreeEntryKind::Directory);
    scan_archive(
        archive_path,
        nested_format,
        &mut nested_root,
        error_count,
        temporary_paths,
    )
    .ok()?;

    if nested_root.descendant_file_count() <= 1 {
        return None;
    }

    if outer_archive_format != ArchiveFormat::SevenZ {
        rewrite_nested_local_sources(
            &mut nested_root,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_format,
        );
    }
    Some(nested_root)
}

/// 将内层压缩包目录树中的普通来源改写为嵌套来源。
///
/// 业务意图：
/// - ZIP/TAR.GZ 内层压缩包扫描会产生 `ArchiveMember`，需要保留“外层压缩包 + 内层压缩包路径 + 内层成员路径”的语义。
/// - 7Z 内层压缩包会先物化为本地文件，此时保留 `LocalFile` 来源即可避免再次随机读取 7Z。
fn rewrite_nested_local_sources(
    node: &mut TreeNode,
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    nested_archive_format: ArchiveFormat,
) {
    if let Some(LogFileSource::ArchiveMember { member_path, .. }) = node.source.clone() {
        node.source = Some(LogFileSource::NestedArchiveMember {
            outer_archive_path: outer_archive_path.to_path_buf(),
            outer_archive_format,
            archive_member_path: archive_member_path.to_string(),
            nested_archive_format,
            nested_member_path: member_path,
        });
    }

    for child in &mut node.children {
        rewrite_nested_local_sources(
            child,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_archive_format,
        );
    }
}

/// 尝试从外层压缩包条目中读取内层压缩包目录树。
///
/// 业务意图：
/// - 外层压缩包内如果包含多文件压缩包，用户希望把该内层压缩包当作目录展开并选择具体文件。
/// - 只有内层压缩包包含两个及以上普通文件时才替换成目录；单文件内层压缩包继续保留“当作文件本身打开”的既有行为。
///
/// 边界条件：
/// - 加载树阶段不能为巨大内层压缩包无上限占用内存，超过 `NESTED_ARCHIVE_SCAN_MAX_BYTES` 时直接回退为普通文件节点。
/// - RAR 内层压缩包需要路径型 API，因此会先写入临时文件，再复用普通压缩包扫描逻辑。
fn read_nested_archive_tree_from_reader(
    reader: &mut dyn Read,
    declared_size: Option<u64>,
    nested_format: ArchiveFormat,
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    error_count: &mut usize,
    temporary_paths: &mut Vec<PathBuf>,
) -> Option<TreeNode> {
    let size = declared_size?;
    if size > NESTED_ARCHIVE_SCAN_MAX_BYTES {
        return None;
    }

    let mut bytes = Vec::with_capacity(size.min(1024 * 1024) as usize);
    let mut limited_reader = reader.take(NESTED_ARCHIVE_SCAN_MAX_BYTES + 1);
    if limited_reader.read_to_end(&mut bytes).is_err()
        || bytes.len() as u64 > NESTED_ARCHIVE_SCAN_MAX_BYTES
    {
        return None;
    }

    if nested_format == ArchiveFormat::Rar {
        let temp_path =
            materialize_nested_archive_bytes_for_scan(&bytes, archive_member_path, temporary_paths)
                .ok()?;
        return read_nested_archive_tree_from_path(
            &temp_path,
            nested_format,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            error_count,
            temporary_paths,
        );
    }

    let segments = split_archive_entry_path(archive_member_path).ok()?;
    let label = segments.last()?.clone();
    let mut nested_root = TreeNode::new(label, LogTreeEntryKind::Directory);
    match nested_format {
        ArchiveFormat::Zip => scan_nested_zip_archive(
            &bytes,
            &mut nested_root,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_format,
            error_count,
        ),
        ArchiveFormat::TarGz => scan_nested_tar_gz_archive(
            &bytes,
            &mut nested_root,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_format,
            error_count,
        ),
        ArchiveFormat::SevenZ => scan_nested_7z_archive(
            &bytes,
            &mut nested_root,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_format,
            error_count,
        ),
        ArchiveFormat::Rar => return None,
    }
    .ok()?;

    (nested_root.descendant_file_count() > 1).then_some(nested_root)
}

/// 将内存中的内层压缩包字节写入临时文件。
///
/// 业务意图：
/// - RAR 只能从路径读取目录；当 RAR 位于 ZIP/TAR.GZ 这类 reader 型外层压缩包中时，需要先落盘。
/// - 只写入小于扫描阈值的内层压缩包，避免目录构建阶段消耗大量磁盘和内存。
fn materialize_nested_archive_bytes_for_scan(
    bytes: &[u8],
    archive_member_path: &str,
    temporary_paths: &mut Vec<PathBuf>,
) -> Result<PathBuf, LogLoadError> {
    let root = nested_archive_scan_materialized_root(archive_member_path)?;
    fs::create_dir_all(&root).map_err(|error| {
        LogLoadError::new(format!(
            "无法创建内存内层压缩包临时目录 {}：{}",
            root.display(),
            error
        ))
    })?;
    temporary_paths.push(root.clone());
    let temp_path = materialized_nested_archive_path(&root, archive_member_path)?;
    if let Some(parent) = temp_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            LogLoadError::new(format!(
                "无法创建内存内层压缩包父目录 {}：{}",
                parent.display(),
                error
            ))
        })?;
    }
    fs::write(&temp_path, bytes).map_err(|error| {
        LogLoadError::new(format!(
            "无法写入内存内层压缩包临时文件 {}：{}",
            temp_path.display(),
            error
        ))
    })?;
    Ok(temp_path)
}

/// 扫描内存中的 ZIP 内层压缩包。
fn scan_nested_zip_archive(
    archive_bytes: &[u8],
    root: &mut TreeNode,
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    nested_archive_format: ArchiveFormat,
    error_count: &mut usize,
) -> Result<(), LogLoadError> {
    let mut archive = ZipArchive::new(Cursor::new(archive_bytes))
        .map_err(|error| LogLoadError::new(format!("无法读取嵌套 ZIP 目录：{}", error)))?;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| {
            LogLoadError::new(format!("无法读取嵌套 ZIP 条目 {}：{}", index, error))
        })?;
        add_nested_archive_entry(
            root,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_archive_format,
            entry.name(),
            entry.is_dir(),
            Some(entry.size()),
            error_count,
        );
    }
    Ok(())
}

/// 扫描内存中的 TAR.GZ 内层压缩包。
fn scan_nested_tar_gz_archive(
    archive_bytes: &[u8],
    root: &mut TreeNode,
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    nested_archive_format: ArchiveFormat,
    error_count: &mut usize,
) -> Result<(), LogLoadError> {
    let decoder = GzDecoder::new(Cursor::new(archive_bytes));
    let mut archive = TarArchive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| LogLoadError::new(format!("无法读取嵌套 TAR.GZ 目录：{}", error)))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| LogLoadError::new(format!("无法读取嵌套 TAR.GZ 条目：{}", error)))?;
        let entry_path = entry.path().map_err(|error| {
            LogLoadError::new(format!("无法读取嵌套 TAR.GZ 条目路径：{}", error))
        })?;
        let raw_name = entry_path.to_string_lossy();
        add_nested_archive_entry(
            root,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_archive_format,
            raw_name.as_ref(),
            entry.header().entry_type().is_dir(),
            Some(entry.size()),
            error_count,
        );
    }
    Ok(())
}

/// 扫描内存中的 7Z 内层压缩包。
fn scan_nested_7z_archive(
    archive_bytes: &[u8],
    root: &mut TreeNode,
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    nested_archive_format: ArchiveFormat,
    error_count: &mut usize,
) -> Result<(), LogLoadError> {
    let reader = sevenz_rust::SevenZReader::new(
        Cursor::new(archive_bytes),
        archive_bytes.len() as u64,
        sevenz_rust::Password::empty(),
    )
    .map_err(|error| LogLoadError::new(format!("无法读取嵌套 7Z 目录：{}", error)))?;
    for entry in &reader.archive().files {
        add_nested_archive_entry(
            root,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_archive_format,
            entry.name(),
            entry.is_directory(),
            Some(entry.size),
            error_count,
        );
    }
    Ok(())
}

/// 把已扫描出的内层压缩包目录树挂到外层压缩包目录树中。
fn add_nested_archive_tree(
    root: &mut TreeNode,
    archive_member_path: &str,
    nested_tree: TreeNode,
    error_count: &mut usize,
) {
    let segments = match split_archive_entry_path(archive_member_path) {
        Ok(segments) => segments,
        Err(reason) => {
            *error_count += 1;
            root.add_archive_error_entry(archive_member_path, "非法路径", reason);
            return;
        }
    };
    root.add_subtree_path(&segments, nested_tree);
}

/// 把压缩包条目添加到目录树。
///
/// 业务意图：
/// - 所有格式都必须经过同一个路径安全检查，避免不同压缩包实现出现展示边界差异。
/// - 条目目录会自动补齐隐式父目录，使压缩包内没有显式目录项时仍能形成目录树。
///
/// 边界条件：
/// - 空路径、绝对路径、盘符路径和包含 `..` 的路径会转成错误节点。
/// - 目录条目不展示大小；文件条目展示未压缩大小。
fn add_archive_entry(
    root: &mut TreeNode,
    archive_path: &Path,
    archive_format: ArchiveFormat,
    raw_name: &str,
    is_directory: bool,
    size: Option<u64>,
    error_count: &mut usize,
) {
    let segments = match split_archive_entry_path(raw_name) {
        Ok(segments) => segments,
        Err(reason) => {
            *error_count += 1;
            root.add_archive_error_entry(raw_name, "非法路径", reason);
            return;
        }
    };

    let kind = if is_directory {
        LogTreeEntryKind::Directory
    } else {
        LogTreeEntryKind::File
    };
    let meta = if is_directory {
        None
    } else {
        size.map(format_byte_size)
    };

    let source = if is_directory {
        None
    } else {
        Some(LogFileSource::ArchiveMember {
            archive_path: archive_path.to_path_buf(),
            archive_format,
            member_path: join_archive_segments(&segments),
        })
    };

    root.add_leaf_path(&segments, kind, meta, source, None);
}

/// 把内层压缩包条目添加到内层目录树。
///
/// 业务意图：
/// - 内层压缩包被当作目录显示，但其子文件仍需要保留“外层压缩包 + 内层压缩包成员 + 内层文件成员”的完整读取来源。
fn add_nested_archive_entry(
    root: &mut TreeNode,
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    nested_archive_format: ArchiveFormat,
    raw_name: &str,
    is_directory: bool,
    size: Option<u64>,
    error_count: &mut usize,
) {
    let segments = match split_archive_entry_path(raw_name) {
        Ok(segments) => segments,
        Err(reason) => {
            *error_count += 1;
            root.add_archive_error_entry(raw_name, "非法路径", reason);
            return;
        }
    };

    let kind = if is_directory {
        LogTreeEntryKind::Directory
    } else {
        LogTreeEntryKind::File
    };
    let meta = if is_directory {
        None
    } else {
        size.map(format_byte_size)
    };
    let source = if is_directory {
        None
    } else {
        Some(LogFileSource::NestedArchiveMember {
            outer_archive_path: outer_archive_path.to_path_buf(),
            outer_archive_format,
            archive_member_path: archive_member_path.to_string(),
            nested_archive_format,
            nested_member_path: join_archive_segments(&segments),
        })
    };

    root.add_leaf_path(&segments, kind, meta, source, None);
}

/// 将普通文件系统相对路径拆成目录树片段。
///
/// 业务意图：
/// - 目录扫描已经由操作系统解析真实路径，这里只做展示片段转换。
/// - 使用 `Component` 可以自然忽略当前目录片段，并避免把根前缀错误显示为节点。
///
/// 边界条件：
/// - 普通文件系统路径来自 `strip_prefix` 后的相对路径，理论上不应包含根目录或盘符前缀。
fn path_components_for_tree(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// 将压缩包内部路径拆成安全的目录树片段。
///
/// 业务意图：
/// - 压缩包路径是外部输入，即使当前不解压，也不能把危险路径伪装成正常目录树。
/// - 统一将反斜杠归一化为斜杠，兼容 Windows 上创建的压缩包。
///
/// 安全边界：
/// - 拒绝绝对路径，避免 `/var/log/a.log` 这类条目看起来像真实本机路径。
/// - 拒绝盘符路径，避免 `C:\logs\a.log` 在 Windows 语境下产生歧义。
/// - 拒绝 `..`，避免后续接入读取或解压能力时遗留路径穿越风险。
/// - 拒绝空片段和 NUL 字符，避免展示异常或底层 API 解析差异。
fn split_archive_entry_path(raw_name: &str) -> Result<Vec<String>, String> {
    let normalized = raw_name.replace('\\', "/");
    let trimmed = normalized.trim_matches('/');

    if normalized.starts_with('/') {
        return Err("压缩包条目是绝对路径".to_string());
    }

    if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
        return Err("压缩包条目包含 Windows 盘符路径".to_string());
    }

    if trimmed.is_empty() {
        return Err("压缩包条目路径为空".to_string());
    }

    let mut segments = Vec::new();
    for segment in trimmed.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }

        if segment == ".." {
            return Err("压缩包条目包含上级目录片段".to_string());
        }

        if segment.contains('\0') {
            return Err("压缩包条目包含非法 NUL 字符".to_string());
        }

        segments.push(segment.to_string());
    }

    if segments.is_empty() {
        return Err("压缩包条目路径为空".to_string());
    }

    Ok(segments)
}

/// 将压缩包内部路径归一化为安全、稳定、跨平台的成员路径。
///
/// 业务意图：
/// - 日志目录树和日志内容读取必须使用同一套路径安全规则，否则树中可见的条目可能无法被点击打开。
/// - 归一化结果统一使用 `/` 分隔，便于作为 `LogFileSource::ArchiveMember::member_path` 的稳定定位。
///
/// 边界条件：
/// - 该函数只接受安全相对路径；绝对路径、盘符路径、上级目录和 NUL 字符都会返回错误。
/// - 返回值用于匹配压缩包条目，不会直接拼接到本地文件系统路径，因此不会触发实际解压写入。
pub fn normalize_archive_member_path(raw_name: &str) -> Result<String, String> {
    split_archive_entry_path(raw_name).map(|segments| join_archive_segments(&segments))
}

/// 将已经安全拆分的压缩包路径片段重新连接成成员路径。
///
/// 业务意图：
/// - 集中使用 `/` 作为压缩包内部路径分隔符，避免 ZIP、RAR、TAR 和 7Z 各自保留不同原始分隔符。
fn join_archive_segments(segments: &[String]) -> String {
    segments.join("/")
}

/// 为本地路径生成目录树根节点显示名。
///
/// 业务意图：
/// - 优先展示文件名或目录名，让左侧树在默认 300px 宽度下更易扫描。
/// - 如果路径没有文件名，则回退到完整路径字符串，保证错误节点仍可定位来源。
fn display_name_for_path(path: &Path) -> String {
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

/// 将字节数格式化成适合目录树元信息展示的短文本。
///
/// 业务意图：
/// - 目录树右侧空间有限，需要使用紧凑单位表达文件大小。
/// - 该格式只用于展示，不用于排序或精确计算。
///
/// 边界条件：
/// - 小于 1KB 使用字节；KB 以上保留整数，避免过长小数影响窄面板布局。
fn format_byte_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    let bytes_f64 = bytes as f64;
    if bytes_f64 >= GB {
        format!("{:.0} GB", bytes_f64 / GB)
    } else if bytes_f64 >= MB {
        format!("{:.0} MB", bytes_f64 / MB)
    } else if bytes_f64 >= KB {
        format!("{:.0} KB", bytes_f64 / KB)
    } else {
        format!("{} B", bytes)
    }
}

/// 构建目录树时使用的内部节点。
///
/// 业务意图：
/// - 加载阶段需要递归插入和排序，直接使用扁平行会导致隐式目录补齐逻辑复杂。
/// - 内部节点允许先构建树，再统一排序和扁平化给 UI。
///
/// 边界条件：
/// - 该结构不暴露给 UI，后续可以替换为更高效的 arena 或索引结构。
#[derive(Clone, Debug, PartialEq, Eq)]
struct TreeNode {
    /// 节点显示名称。
    label: String,
    /// 节点类型。
    kind: LogTreeEntryKind,
    /// 右侧元信息。
    meta: Option<String>,
    /// 错误详细信息。
    error_message: Option<String>,
    /// 文件节点的可打开来源。
    ///
    /// 业务意图：
    /// - 构建树时就保存来源，扁平化后 UI 才能直接打开对应日志正文。
    /// - 中间目录和错误节点没有正文来源，保持为 `None`。
    source: Option<LogFileSource>,
    /// 子节点列表。
    children: Vec<TreeNode>,
}

impl TreeNode {
    /// 创建一个普通内部节点。
    ///
    /// 边界条件：
    /// - 新节点默认没有元信息、错误信息和子节点，调用方可按来源类型继续补充。
    fn new(label: impl Into<String>, kind: LogTreeEntryKind) -> Self {
        Self {
            label: label.into(),
            kind,
            meta: None,
            error_message: None,
            source: None,
            children: Vec::new(),
        }
    }

    /// 创建一个错误节点。
    ///
    /// 业务意图：
    /// - 用树节点承载错误，而不是只返回全局错误，保证用户能看到具体失败位置。
    fn error(
        label: impl Into<String>,
        meta: impl Into<String>,
        error_message: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            kind: LogTreeEntryKind::Error,
            meta: Some(meta.into()),
            error_message: Some(error_message.into()),
            source: None,
            children: Vec::new(),
        }
    }

    /// 添加一个普通错误子节点。
    ///
    /// 业务意图：
    /// - 目录扫描或压缩包扫描遇到局部失败时，用该方法把错误放在当前来源根节点下。
    fn add_error_child(
        &mut self,
        label: impl Into<String>,
        meta: impl Into<String>,
        error_message: impl Into<String>,
    ) {
        self.children
            .push(Self::error(label.into(), meta.into(), error_message.into()));
    }

    /// 添加一个与压缩包内部路径相关的错误节点。
    ///
    /// 业务意图：
    /// - 非法路径或加密条目仍应显示原始条目名，帮助用户判断压缩包内容问题。
    /// - 错误节点放在压缩包根下，不参与正常路径层级构建。
    fn add_archive_error_entry(
        &mut self,
        raw_name: &str,
        meta: impl Into<String>,
        error_message: impl Into<String>,
    ) {
        let label = if raw_name.trim().is_empty() {
            "空压缩包路径".to_string()
        } else {
            format!("非法条目：{}", raw_name)
        };
        self.add_error_child(label, meta, error_message);
    }

    /// 根据路径片段添加叶子节点，并自动补齐中间目录。
    ///
    /// 业务意图：
    /// - 目录扫描和压缩包扫描都可能先看到文件路径，父目录节点需要自动出现。
    /// - 同名同类型节点会复用，避免压缩包显式目录项和文件路径隐式目录产生重复目录。
    ///
    /// 边界条件：
    /// - 如果同一目录下同时存在同名文件和目录，当前会保留两个不同类型节点。
    /// - 重复文件路径会更新已有节点的元信息，避免目录树出现完全相同的重复行。
    fn add_leaf_path(
        &mut self,
        segments: &[String],
        kind: LogTreeEntryKind,
        meta: Option<String>,
        source: Option<LogFileSource>,
        error_message: Option<String>,
    ) {
        if segments.is_empty() {
            return;
        }

        let mut current = self;
        for segment in &segments[..segments.len() - 1] {
            current = current.get_or_insert_child(segment, LogTreeEntryKind::Directory);
        }

        let leaf_label = &segments[segments.len() - 1];
        let leaf = current.get_or_insert_child(leaf_label, kind);
        leaf.meta = meta;
        leaf.source = source;
        leaf.error_message = error_message;
    }

    /// 在指定路径位置挂载一棵已经构建好的子树。
    ///
    /// 业务意图：
    /// - 外层压缩包中的多文件内层压缩包需要显示为一个可展开目录，子树来自内层压缩包目录扫描结果。
    ///
    /// 边界条件：
    /// - 如果外层压缩包同时存在同名目录，复用该目录节点并追加内层子节点；这种冲突来自压缩包本身，加载层不静默丢弃任一侧内容。
    fn add_subtree_path(&mut self, segments: &[String], subtree: TreeNode) {
        if segments.is_empty() {
            return;
        }

        let mut current = self;
        for segment in &segments[..segments.len() - 1] {
            current = current.get_or_insert_child(segment, LogTreeEntryKind::Directory);
        }

        let leaf_label = &segments[segments.len() - 1];
        let leaf = current.get_or_insert_child(leaf_label, LogTreeEntryKind::Directory);
        leaf.meta = subtree.meta;
        leaf.source = None;
        leaf.error_message = subtree.error_message;
        leaf.children.extend(subtree.children);
    }

    /// 查找或插入一个同名同类型子节点。
    ///
    /// 业务意图：
    /// - 复用目录节点可以自然合并多条路径的公共前缀。
    /// - 文件和目录同名时不合并，避免错误隐藏真实结构冲突。
    fn get_or_insert_child(&mut self, label: &str, kind: LogTreeEntryKind) -> &mut TreeNode {
        if let Some(index) = self
            .children
            .iter()
            .position(|child| child.label == label && child.kind == kind)
        {
            return &mut self.children[index];
        }

        self.children.push(Self::new(label.to_string(), kind));
        let index = self.children.len() - 1;
        &mut self.children[index]
    }

    /// 对当前节点及其所有子节点排序。
    ///
    /// 业务意图：
    /// - 目录优先、文件其次、错误最后，符合日志目录扫描习惯。
    /// - 名称使用大小写不敏感排序，减少跨平台文件系统大小写差异带来的视觉跳动。
    fn sort_recursively(&mut self) {
        for child in &mut self.children {
            child.sort_recursively();
        }

        self.children.sort_by(|left, right| {
            let left_key = (sort_rank(left.kind), left.label.to_ascii_lowercase());
            let right_key = (sort_rank(right.kind), right.label.to_ascii_lowercase());
            left_key.cmp(&right_key)
        });
    }

    /// 将递归树节点扁平化为 UI 可直接渲染的行。
    ///
    /// 边界条件：
    /// - 当前函数仍输出完整树，展开/收起只在 UI 的可见行缓存中处理。
    /// - `next_row_id` 只服务于当前加载结果，不能跨加载复用。
    fn flatten_into(&self, depth: usize, next_row_id: &mut usize, rows: &mut Vec<LogTreeRow>) {
        let id = *next_row_id;
        *next_row_id += 1;

        rows.push(LogTreeRow {
            id,
            depth,
            label: self.label.clone(),
            kind: self.kind,
            has_children: !self.children.is_empty(),
            meta: self.display_meta(),
            error_message: self.error_message.clone(),
            source: self.source.clone(),
        });

        for child in &self.children {
            child.flatten_into(depth + 1, next_row_id, rows);
        }
    }

    /// 返回当前节点适合目录树展示的补充信息。
    ///
    /// 业务意图：
    /// - 用户要求文件夹名称后展示文件夹中的文件数量，因此目录节点需要在扁平化时补齐该派生信息。
    /// - 文件数量使用递归统计，能表达该目录下所有可打开日志文件的规模，避免父目录只含子目录时显示为 0。
    ///
    /// 边界条件：
    /// - 压缩包根节点继续显示格式信息，避免 “ZIP/RAR/7Z” 格式提示被文件数覆盖。
    /// - 错误、符号链接和普通文件沿用构建阶段写入的元信息，不额外派生数量。
    fn display_meta(&self) -> Option<String> {
        match self.kind {
            LogTreeEntryKind::Directory => Some(format!("{} 个文件", self.descendant_file_count())),
            LogTreeEntryKind::Archive
            | LogTreeEntryKind::File
            | LogTreeEntryKind::Symlink
            | LogTreeEntryKind::Error => self.meta.clone(),
        }
    }

    /// 递归统计当前节点下可打开文件节点数量。
    ///
    /// 业务意图：
    /// - 目录树中的文件数量用于帮助用户快速判断目录规模，而不是统计目录项总数。
    /// - 只把 `LogTreeEntryKind::File` 计入数量，避免错误节点、符号链接或纯目录影响日志文件规模判断。
    ///
    /// 边界条件：
    /// - 当前目录本身不可能是文件节点时才调用；即使未来复用到其它节点，普通文件也会按 1 个文件处理。
    fn descendant_file_count(&self) -> usize {
        match self.kind {
            LogTreeEntryKind::File => 1,
            LogTreeEntryKind::Directory | LogTreeEntryKind::Archive => {
                self.children.iter().map(Self::descendant_file_count).sum()
            }
            LogTreeEntryKind::Symlink | LogTreeEntryKind::Error => 0,
        }
    }
}

/// 返回目录树排序时使用的类型优先级。
///
/// 业务意图：
/// - 目录和压缩包都可包含子节点，因此排在普通文件前，方便用户先浏览结构。
/// - 错误节点放在最后，避免局部失败打断主要文件结构扫描。
fn sort_rank(kind: LogTreeEntryKind) -> u8 {
    match kind {
        LogTreeEntryKind::Directory => 0,
        LogTreeEntryKind::Archive => 1,
        LogTreeEntryKind::File => 2,
        LogTreeEntryKind::Symlink => 3,
        LogTreeEntryKind::Error => 4,
    }
}

#[cfg(test)]
mod tests {
    //! 加载模块的单元测试。
    //!
    //! 业务意图：
    //! - 这些测试覆盖不依赖图形环境的纯业务规则，尤其是压缩包扩展名识别、路径安全和树排序。
    //! - UI 交互由后续桌面验收或自动化截图覆盖，本模块只验证数据结构构建规则。

    use super::*;
    use std::io::{self, Cursor, Write};
    use std::time::{SystemTime, UNIX_EPOCH};
    use zip::write::SimpleFileOptions;

    /// 验证复合压缩包扩展名和大小写扩展名都能识别。
    #[test]
    fn 识别支持的压缩包扩展名() {
        assert_eq!(
            ArchiveFormat::from_path(Path::new("logs.tar.gz")),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(
            ArchiveFormat::from_path(Path::new("logs.TGZ")),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(
            ArchiveFormat::from_path(Path::new("logs.zip")),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(
            ArchiveFormat::from_path(Path::new("logs.RAR")),
            Some(ArchiveFormat::Rar)
        );
        assert_eq!(
            ArchiveFormat::from_path(Path::new("logs.7z")),
            Some(ArchiveFormat::SevenZ)
        );
        assert_eq!(ArchiveFormat::from_path(Path::new("plain.log")), None);
    }

    /// 验证压缩包路径安全检查会拒绝危险路径。
    #[test]
    fn 压缩包路径安全检查拒绝危险路径() {
        assert!(split_archive_entry_path("/etc/passwd").is_err());
        assert!(split_archive_entry_path("C:\\logs\\a.log").is_err());
        assert!(split_archive_entry_path("../a.log").is_err());
        assert!(split_archive_entry_path("a/../../b.log").is_err());
        assert!(split_archive_entry_path("").is_err());
        assert!(split_archive_entry_path("a/\0/b.log").is_err());
    }

    /// 验证压缩包路径安全检查会归一化 Windows 分隔符。
    #[test]
    fn 压缩包路径安全检查支持反斜杠分隔符() {
        assert_eq!(
            split_archive_entry_path("service\\access.log").unwrap(),
            vec!["service".to_string(), "access.log".to_string()]
        );
    }

    /// 验证 RAR 内层压缩包候选路径会先归一化。
    ///
    /// 业务意图：
    /// - Windows 创建的 RAR 可能把内部路径写成 `dir\inner.zip`。
    /// - 后续物化、读取和挂载树都使用 `/` 分隔路径，避免和 RAR 处理阶段的归一化名称比较失败。
    #[test]
    fn rar_内层压缩包候选路径会归一化() {
        assert_eq!(
            normalized_rar_nested_archive_member_path("dir\\inner.zip", false, 1024),
            Some("dir/inner.zip".to_string())
        );
        assert_eq!(
            normalized_rar_nested_archive_member_path("../inner.zip", false, 1024),
            None
        );
    }

    /// 验证树构建会补齐隐式目录并保持目录优先排序。
    #[test]
    fn 树构建会补齐隐式目录并保持目录优先() {
        let mut root = TreeNode::new("root", LogTreeEntryKind::Directory);
        root.add_leaf_path(
            &["b.log".to_string()],
            LogTreeEntryKind::File,
            Some("1 KB".to_string()),
            Some(LogFileSource::LocalFile {
                path: PathBuf::from("b.log"),
            }),
            None,
        );
        root.add_leaf_path(
            &["api".to_string(), "access.log".to_string()],
            LogTreeEntryKind::File,
            Some("2 KB".to_string()),
            Some(LogFileSource::LocalFile {
                path: PathBuf::from("api/access.log"),
            }),
            None,
        );
        root.sort_recursively();

        let mut rows = Vec::new();
        let mut next_row_id = 0usize;
        root.flatten_into(0, &mut next_row_id, &mut rows);

        assert_eq!(rows[0].meta.as_deref(), Some("2 个文件"));
        assert_eq!(rows[1].label, "api");
        assert_eq!(rows[1].kind, LogTreeEntryKind::Directory);
        assert!(rows[1].has_children);
        assert_eq!(rows[1].id, 1);
        assert_eq!(rows[1].meta.as_deref(), Some("1 个文件"));
        assert_eq!(rows[2].label, "access.log");
        assert!(!rows[2].has_children);
        assert_eq!(rows[3].label, "b.log");
    }

    /// 验证目录扫描不会跟随符号链接。
    ///
    /// 边界条件：
    /// - Windows 创建符号链接通常需要额外权限，因此该测试只在 Unix 平台运行。
    #[cfg(unix)]
    #[test]
    fn 目录扫描不跟随符号链接() -> io::Result<()> {
        use std::os::unix::fs::symlink;

        let temp_root = unique_temp_dir("logclinic3-symlink-test")?;
        let real_dir = temp_root.join("real");
        let selected_dir = temp_root.join("selected");
        fs::create_dir_all(&real_dir)?;
        fs::create_dir_all(&selected_dir)?;
        fs::write(real_dir.join("outside.log"), b"outside")?;
        symlink(&real_dir, selected_dir.join("linked"))?;

        let loaded = load_log_sources(vec![selected_dir.clone()]).unwrap();
        let linked_row = loaded
            .rows
            .iter()
            .find(|row| row.label == "linked")
            .expect("目录树中应该显示符号链接节点");

        assert_eq!(linked_row.kind, LogTreeEntryKind::Symlink);
        assert!(
            loaded.rows.iter().all(|row| row.label != "outside.log"),
            "符号链接目标内部文件不应被扫描进所选目录树"
        );

        fs::remove_dir_all(temp_root)?;
        Ok(())
    }

    /// 验证外层 ZIP 中的多文件 ZIP 会作为目录展开。
    ///
    /// 业务意图：
    /// - 内层压缩包如果包含多个文件，不能继续走“单文件压缩包直接打开”规则，否则用户无法选择具体日志。
    /// - 展开后的子文件必须保存嵌套来源，后续打开正文时才能先定位外层成员，再读取内层成员。
    #[test]
    fn 外层_zip_中的多文件_zip_会作为目录展开() -> Result<(), Box<dyn Error>> {
        let temp_root = unique_temp_dir("logclinic3-nested-zip-tree-test")?;
        let outer_path = temp_root.join("outer.zip");

        let mut inner_bytes = Cursor::new(Vec::new());
        {
            let mut inner_writer = zip::ZipWriter::new(&mut inner_bytes);
            inner_writer.start_file("a.log", SimpleFileOptions::default())?;
            inner_writer.write_all(b"INFO a")?;
            inner_writer.start_file("dir/b.log", SimpleFileOptions::default())?;
            inner_writer.write_all(b"INFO b")?;
            inner_writer.finish()?;
        }

        {
            let outer_file = File::create(&outer_path)?;
            let mut outer_writer = zip::ZipWriter::new(outer_file);
            outer_writer.start_file("nested.zip", SimpleFileOptions::default())?;
            outer_writer.write_all(inner_bytes.get_ref())?;
            outer_writer.finish()?;
        }

        let loaded = load_log_sources(vec![outer_path.clone()])?;
        let nested_row = loaded
            .rows
            .iter()
            .find(|row| row.label == "nested.zip")
            .expect("内层多文件 ZIP 应显示为可展开目录");
        assert_eq!(nested_row.kind, LogTreeEntryKind::Directory);
        assert!(nested_row.has_children);

        let nested_sources = loaded
            .rows
            .iter()
            .filter_map(|row| row.source.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(nested_sources.len(), 2);
        assert!(nested_sources.iter().any(|source| matches!(
            source,
            LogFileSource::NestedArchiveMember {
                outer_archive_path,
                outer_archive_format: ArchiveFormat::Zip,
                archive_member_path,
                nested_archive_format: ArchiveFormat::Zip,
                nested_member_path,
            } if outer_archive_path == &outer_path
                && archive_member_path == "nested.zip"
                && nested_member_path == "a.log"
        )));
        assert!(nested_sources.iter().any(|source| matches!(
            source,
            LogFileSource::NestedArchiveMember {
                nested_member_path,
                ..
            } if nested_member_path == "dir/b.log"
        )));

        fs::remove_dir_all(temp_root)?;
        Ok(())
    }

    /// 创建当前测试专用的临时目录。
    ///
    /// 业务意图：
    /// - 避免引入额外测试依赖，只使用标准库在系统临时目录下创建唯一目录。
    /// - 目录名带纳秒时间戳，降低并发测试冲突概率。
    fn unique_temp_dir(prefix: &str) -> io::Result<PathBuf> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{}-{}", prefix, nanos));
        fs::create_dir_all(&path)?;
        Ok(path)
    }
}
