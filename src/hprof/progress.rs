//! HPROF 分析进度类型。
//!
//! 业务意图：
//! - 后台分析任务通过纯数据进度快照向 UI 报告当前阶段、细分工作量和已解析计数。
//! - 阶段枚举集中在这里，避免 parser、dominator 和 sidecar 各自定义中文阶段文案。
//!
//! 边界条件：
//! - 进度结构必须保持可克隆，不持有文件句柄或 UI 对象，确保后台线程和 GPUI 状态之间只传递轻量数据。

/// HPROF 分析阶段。
///
/// 业务意图：
/// - UI 需要展示详细解析进度；阶段枚举让后台任务可以稳定表达当前耗时集中在哪一步。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HprofAnalysisStage {
    /// 正在检查 sidecar 缓存是否可复用。
    CheckingCache,
    /// 正在从 sidecar 缓存恢复结果。
    LoadingCache,
    /// 校验用户选择的路径和文件头。
    Validating,
    /// 正在读取 HPROF header。
    ReadingHeader,
    /// 正在扫描顶层记录和 heap dump 子记录。
    ReadingRecords,
    /// 正在把类 ID、字符串表和字段布局解析成可展示类名。
    ResolvingClasses,
    /// 正在把原始 HPROF 字段引用转换成 MAT 兼容对象引用边。
    MaterializingReferences,
    /// 正在把对象引用关系转换成 dominator 算法使用的图结构。
    BuildingDominatorGraph,
    /// 正在运行 dominator 算法。
    ComputingDominatorTree,
    /// 正在构造 UI 可直接展示的树节点和 Top retained 列表。
    BuildingRows,
    /// 正在把完成结果写入 sidecar 索引。
    WritingIndex,
    /// 分析完成。
    Completed,
}

impl HprofAnalysisStage {
    /// 返回面向用户展示的中文阶段名。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::CheckingCache => "检查缓存",
            Self::LoadingCache => "加载缓存",
            Self::Validating => "校验文件",
            Self::ReadingHeader => "读取文件头",
            Self::ReadingRecords => "解析对象记录",
            Self::ResolvingClasses => "解析类/字段名",
            Self::MaterializingReferences => "构建引用边",
            Self::BuildingDominatorGraph => "构建引用图",
            Self::ComputingDominatorTree => "计算 Dominator Tree",
            Self::BuildingRows => "整理展示结果",
            Self::WritingIndex => "写入索引",
            Self::Completed => "完成",
        }
    }
}

/// HPROF 解析进度快照。
///
/// 业务意图：
/// - 后台线程不能直接操作 GPUI 状态，因此把进度压缩成可克隆的纯数据，由窗口定时拉取并渲染。
#[derive(Clone, Debug)]
pub(crate) struct HprofProgress {
    /// 当前阶段。
    pub(crate) stage: HprofAnalysisStage,
    /// 当前阶段的中文说明。
    pub(crate) message: String,
    /// 文件总字节数。
    pub(crate) total_bytes: u64,
    /// 已读取字节数。
    pub(crate) bytes_read: u64,
    /// 已解析顶层 HPROF 记录和 heap dump 子记录总数。
    pub(crate) record_count: u64,
    /// 已收集对象数量。
    pub(crate) object_count: usize,
    /// 已收集类数量。
    pub(crate) class_count: usize,
    /// 已收集 GC Root 数量。
    pub(crate) gc_root_count: usize,
    /// 已收集对象引用边数量。
    pub(crate) edge_count: usize,
    /// 当前阶段已完成的工作量。
    pub(crate) phase_done: u64,
    /// 当前阶段总工作量。
    pub(crate) phase_total: u64,
    /// 当前阶段工作量单位。
    pub(crate) phase_unit: &'static str,
    /// 当前阶段的细分说明。
    pub(crate) sub_message: String,
}

impl HprofProgress {
    /// 创建指定文件大小下的初始进度。
    ///
    /// 边界条件：
    /// - 文件大小读取失败时调用方会传入 0；UI 仍可显示阶段和计数，只是不展示可靠百分比。
    pub(crate) fn new(total_bytes: u64) -> Self {
        Self {
            stage: HprofAnalysisStage::Validating,
            message: "正在校验 HPROF 文件".to_string(),
            total_bytes,
            bytes_read: 0,
            record_count: 0,
            object_count: 0,
            class_count: 0,
            gc_root_count: 0,
            edge_count: 0,
            phase_done: 0,
            phase_total: 0,
            phase_unit: "",
            sub_message: String::new(),
        }
    }

    /// 返回 0.0 到 1.0 之间的字节进度比例。
    pub(crate) fn byte_ratio(&self) -> f32 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        (self.bytes_read as f32 / self.total_bytes as f32).clamp(0.0, 1.0)
    }

    /// 返回当前计算阶段 0.0 到 1.0 之间的进度比例。
    pub(crate) fn phase_ratio(&self) -> f32 {
        if self.phase_total == 0 {
            return 0.0;
        }
        (self.phase_done as f32 / self.phase_total as f32).clamp(0.0, 1.0)
    }
}
