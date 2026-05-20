//! 日志加载目录树类型。
//!
//! 业务意图：
//! - 将左侧目录树的加载结果、行模型和错误类型从扫描流程中拆出，避免 `log_loader.rs` 同时承载数据模型和文件系统遍历。
//! - 类型仍保持 crate 内现有导入路径不变，由父模块受控重导出，避免 UI 层调用点跟随内部拆分波动。
//!
//! 边界条件：
//! - 这些类型只表达加载阶段的轻量元数据，不持有文件句柄、压缩包 reader 或日志正文。
//! - 节点 ID 只在一次加载结果内部稳定，不能跨加载或跨进程持久化。

use std::{
    error::Error,
    fmt::{self, Display},
    path::PathBuf,
};

use crate::log_source::LogFileSource;

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

/// 日志来源加载过程中的进度快照。
///
/// 业务意图：
/// - 加载日志来源时，目录扫描、压缩包索引和 7Z 临时物化都在后台线程执行；UI 需要一个可跨线程克隆的轻量快照来展示“正在处理哪一项、已经完成多少项”。
/// - 该结构只描述加载阶段的总体进度，不持有文件句柄、压缩包 reader 或任何日志正文，避免把后台 I/O 状态泄漏给 GPUI 渲染层。
///
/// 边界条件：
/// - `total_sources` 可能为 0，例如用户取消路径选择后调用方仍传入空集合；百分比计算必须退化为 100%，避免除零。
/// - `processed_sources` 会被夹紧到总数以内；即使未来某个加载分支重复汇报进度，UI 也不会显示超过 100%。
/// - 单个目录内部文件数量在递归前未知，因此当前进度以用户选择的来源数量为主；节点数量只供逻辑和最终摘要使用，不进入加载浮层。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogLoadProgress {
    /// 用户本轮选择的来源总数。
    pub total_sources: usize,
    /// 已经完成扫描的来源数量。
    pub processed_sources: usize,
    /// 当前正在扫描的来源名称。
    ///
    /// 业务意图：
    /// - 只保存面向用户的短名称，不保存绝对路径，避免中央加载提示被长路径撑破。
    /// - `None` 表示还未开始扫描或正在整理最终目录树。
    pub current_source: Option<String>,
    /// 当前来源内部的扫描阶段。
    ///
    /// 业务意图：
    /// - 压缩包属于单个顶层来源，但内部可能有成百上千个成员；该字段让 UI 能提示正在读取目录、扫描成员或物化 7Z 条目。
    /// - 普通文件和普通目录没有稳定的内部总量时保持 `None`，避免制造不准确的进度信息。
    pub current_source_step: Option<String>,
    /// 当前来源内部正在处理的条目名称。
    ///
    /// 边界条件：
    /// - 该名称来自压缩包原始成员名，只用于展示，不能作为安全路径或日志来源使用。
    /// - 文案展示会截断过长条目，避免中央进度条被长路径撑破。
    pub current_entry: Option<String>,
    /// 当前来源内部已经完成的工作量。
    ///
    /// 业务意图：
    /// - ZIP/7Z 表示已扫描成员数量；TAR/RAR 表示顺序流已推进的条目数量。
    pub current_source_work_done: usize,
    /// 当前来源内部可预知的工作总量。
    ///
    /// 边界条件：
    /// - 顺序压缩格式无法低成本获取总条目数时为 `None`，进度条会使用保守的非线性估算，保证用户能看到推进但不会承诺准确百分比。
    pub current_source_work_total: Option<usize>,
    /// 当前已经整理出的节点数量估算。
    ///
    /// 边界条件：
    /// - 扫描过程中该值按来源根节点累计，最终完成后会用扁平树行数覆盖。
    pub discovered_nodes: usize,
    /// 当前已经发现的非致命错误数量。
    pub error_count: usize,
    /// 顶部或中央提示使用的中文状态文案。
    pub message: String,
}

impl LogLoadProgress {
    /// 创建加载进度初始快照。
    ///
    /// 业务意图：
    /// - 系统路径选择器返回后立即进入加载态，即使后台线程尚未开始扫描，也要让用户看到确定的总来源数和加载文案。
    pub fn new(total_sources: usize, message: impl Into<String>) -> Self {
        Self {
            total_sources,
            processed_sources: 0,
            current_source: None,
            current_source_step: None,
            current_entry: None,
            current_source_work_done: 0,
            current_source_work_total: None,
            discovered_nodes: 0,
            error_count: 0,
            message: message.into(),
        }
    }

    /// 返回进度条使用的完成比例。
    ///
    /// 边界条件：
    /// - 空来源按完成处理；已完成数量超过总数时夹紧到 1.0，避免 UI 宽度越界。
    pub fn fraction(&self) -> f32 {
        if self.total_sources == 0 {
            return 1.0;
        }
        if self.processed_sources >= self.total_sources {
            return 1.0;
        }

        let completed_sources = self.processed_sources.min(self.total_sources) as f32;
        let current_source_fraction = self.current_source_fraction();
        ((completed_sources + current_source_fraction) / self.total_sources as f32).clamp(0.0, 1.0)
    }

    /// 返回四舍五入后的百分比。
    ///
    /// 业务意图：
    /// - 中央加载提示需要比单纯动效更具体，百分比让用户知道本轮加载是否仍在推进。
    pub fn percent(&self) -> usize {
        (self.fraction() * 100.0).round().clamp(0.0, 100.0) as usize
    }

    /// 返回加载浮层底部使用的条目明细。
    ///
    /// 业务意图：
    /// - 底部行按需求只显示百分比和条目进度，不能再包含来源。
    pub fn entry_detail(&self) -> String {
        format!("条目：{}", self.entry_progress_text())
    }

    /// 返回加载浮层使用的条目进度文本。
    ///
    /// 业务意图：
    /// - ZIP/7Z 这类已知总数的压缩格式展示精确 `已处理 / 总数`。
    /// - TAR/RAR 等顺序格式无法低成本预知总数，只展示已扫描条目数。
    /// - 普通文件或目录没有压缩包条目概念时显示 `-`，避免把节点数或阶段文案混入加载浮层。
    fn entry_progress_text(&self) -> String {
        if let Some(total) = self.current_source_work_total.filter(|total| *total > 0) {
            return format!("{} / {}", self.current_source_work_done.min(total), total);
        }

        if self.current_source_work_done > 0 {
            return self.current_source_work_done.to_string();
        }

        "-".to_string()
    }

    /// 返回当前来源内部的完成比例。
    ///
    /// 业务意图：
    /// - 多来源加载按“已完成来源 + 当前来源内部进度”合成总体百分比，避免单个大压缩包扫描时进度条不动。
    ///
    /// 边界条件：
    /// - 已知总量时使用精确比例，并夹紧到 0.99，直到该来源真正完成才显示 100%。
    /// - 未知总量时采用随条目数递增但不会到 1.0 的估算曲线，让 TAR/RAR 这类顺序格式展示“仍在推进”，同时不伪造准确完成时间。
    fn current_source_fraction(&self) -> f32 {
        if let Some(total) = self.current_source_work_total
            && total > 0
        {
            return (self.current_source_work_done.min(total) as f32 / total as f32)
                .clamp(0.0, 0.99);
        }

        if self.current_source_work_done == 0 {
            return 0.0;
        }

        let done = self.current_source_work_done as f32;
        (done / (done + 32.0)).clamp(0.0, 0.95)
    }
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

/// 日志加载阶段的致命错误。
///
/// 业务意图：
/// - 大多数单个节点错误会被转为目录树错误节点并继续加载。
/// - 只有整个加载流程无法形成结果时才返回该错误，例如应用层传入空路径以外的异常状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogLoadError {
    /// 面向开发者和 UI 的中文错误说明。
    pub(super) message: String,
}

impl Display for LogLoadError {
    /// 将加载错误格式化为中文可读文本。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for LogLoadError {}
