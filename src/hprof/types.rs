//! HPROF 结果和 UI 展示数据类型。
//!
//! 业务意图：
//! - 该模块集中 dominator tree 完成结果、可见行、线程详情和 sidecar 摘要存储类型。
//! - `core` 负责分析编排和方法实现，`sidecar` 负责缓存读写，二者共享这里的 crate 内数据模型。
//!
//! 边界条件：
//! - 这些类型仍然只在 crate 内可见，不构成外部公开 API。
//! - 完成结果可能持有数百万对象摘要；字段保持轻量标量和延迟读取句柄，避免 UI 渲染阶段重新解析 HPROF。

use std::{fs::File, io::BufReader, path::PathBuf};

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use super::{HprofHeader, HprofObjectId, HprofObjectKind, HprofReferenceEdgeStats, HprofSizeModel};

/// Dominator tree 中单个可展示节点。
///
/// 业务意图：
/// - UI 不应在渲染阶段重新查对象图和 retained map，因此把展示所需字段提前固化。
#[derive(Clone, Debug)]
pub(crate) struct HprofDominatorNode {
    /// 对象 ID。
    pub(crate) object_id: HprofObjectId,
    /// 展示名称，优先为类名。
    pub(crate) name: String,
    /// shallow size。
    pub(crate) shallow_size: u64,
    /// retained size。
    pub(crate) retained_size: u64,
    /// retained size 占总 heap shallow size 的比例。
    pub(crate) retained_percent: f32,
    /// 直接支配的对象数量。
    pub(crate) direct_child_count: usize,
}

/// Dominator tree 可见行。
///
/// 业务意图：
/// - 独立窗口按“Top retained + 可展开子节点”展示，行模型需要额外携带展开深度。
#[derive(Clone, Debug)]
pub(crate) struct HprofDominatorRow {
    /// 行缩进深度。
    pub(crate) depth: usize,
    /// 节点数据。
    pub(crate) node: HprofDominatorNode,
}

/// HPROF 线程详情中的属性行。
///
/// 业务意图：
/// - MAT 的 Thread Details 使用“属性名 / 属性值”表格展示线程对象状态；UI 层只负责渲染，不重新解析对象字段。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HprofThreadProperty {
    /// 属性展示名。
    pub(crate) name: String,
    /// 已格式化的属性值。
    pub(crate) value: String,
}

/// HPROF 线程详情中的单个栈帧。
///
/// 业务意图：
/// - HPROF 的 `STACK_FRAME` 记录分散保存类名、方法名、签名、源文件和行号；结果层将其合成为 UI 可直接显示的栈行。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HprofThreadStackFrame {
    /// 栈帧所属类名。
    pub(crate) class_name: String,
    /// 方法名。
    pub(crate) method_name: String,
    /// JVM 方法签名。
    pub(crate) method_signature: String,
    /// 源文件展示名。
    pub(crate) source: String,
    /// HPROF 原始行号；负数表示 native、compiled 或未知来源。
    pub(crate) line_number: i32,
    /// 完整展示行，例如 `at java.lang.Thread.run()V (Thread.java:748)`。
    pub(crate) display: String,
}

/// HPROF 线程详情。
///
/// 业务意图：
/// - 右键菜单打开详情时不能重新读取 5GB 级 dump；所有线程属性和栈信息必须在分析完成结果中以纯数据形式保存。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HprofThreadDetails {
    /// 线程对象 ID。
    pub(crate) object_id: HprofObjectId,
    /// 线程对象类名，通常为 `java.lang.Thread`。
    pub(crate) class_name: String,
    /// 线程名；无法从 HPROF 记录恢复时为 `<unknown>`。
    pub(crate) thread_name: String,
    /// 属性表格。
    pub(crate) properties: Vec<HprofThreadProperty>,
    /// 已解析的线程栈帧。
    pub(crate) stack_frames: Vec<HprofThreadStackFrame>,
    /// 栈缺失或部分损坏时的中文说明。
    pub(crate) stack_message: Option<String>,
}

/// Dominator tree 的紧凑对象摘要。
///
/// 业务意图：
/// - 完成结果可能包含数百万对象，摘要只保存生成 UI 行必须的标量字段；类名和类型文案在可见行生成时再拼接。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct HprofDominatorObjectSummary {
    /// 对象 ID。
    pub(crate) object_id: HprofObjectId,
    /// 对象所属类 ID。
    pub(crate) class_id: HprofObjectId,
    /// 对象类型。
    pub(crate) kind: HprofObjectKind,
    /// shallow size。
    pub(crate) shallow_size: u64,
    /// retained size。
    pub(crate) retained_size: u64,
    /// retained size 占比。
    pub(crate) retained_percent: f32,
    /// 直接支配子节点数量。
    pub(crate) direct_child_count: usize,
}

/// Dominator 对象摘要的存储后端。
///
/// 业务意图：
/// - 首次解析时摘要来自内存构造结果；缓存命中时不应为了展示首屏重新读取全部对象摘要。
/// - UI 渲染 dominator tree 实际只需要 Top 节点和已展开节点的摘要，因此 sidecar 后端按 summary index 随用随读。
#[derive(Debug)]
pub(crate) enum HprofSummaryStorage {
    /// 首次解析完成后持有的内存摘要。
    Owned {
        /// 可达对象摘要，按 summary index 排列。
        summaries: Vec<HprofDominatorObjectSummary>,
    },
    /// sidecar 命中后的懒加载摘要。
    Sidecar {
        /// 可达对象摘要数量。
        count: usize,
        /// `objects.bin` 的复用读取器。
        ///
        /// 业务意图：
        /// - 缓存命中后 UI 会频繁重绘首屏和展开节点；持有同一个带缓冲的文件读取器可以避免每个可见行都重新 open 文件。
        /// - 该结果对象只在 UI 线程读取，使用 `RefCell` 是为了在不可变查询方法中进行按需 seek 和小缓存更新。
        reader: std::cell::RefCell<BufReader<File>>,
        /// 已读取摘要缓存，避免展开/重绘同一节点时反复 seek。
        cache: std::cell::RefCell<FxHashMap<usize, HprofDominatorObjectSummary>>,
        /// 已按对象 ID 查询过的下标缓存；只服务测试/少量查询，不在打开缓存时全量构建。
        #[cfg(test)]
        object_index_cache: std::cell::RefCell<FxHashMap<HprofObjectId, usize>>,
    },
}

/// HPROF dominator tree 完成结果。
///
/// 业务意图：
/// - 结果对象供 UI 长时间持有，必须包含摘要、Top retained 入口和按对象 ID 查询子节点所需的数据。
#[derive(Debug)]
pub(crate) struct HprofDominatorResult {
    /// 原始文件路径。
    pub(crate) file_path: PathBuf,
    /// HPROF header。
    pub(crate) header: HprofHeader,
    /// 对象总数。
    pub(crate) total_objects: usize,
    /// 类总数。
    pub(crate) total_classes: usize,
    /// GC Root 总数。
    pub(crate) gc_root_count: usize,
    /// 对象引用边总数。
    pub(crate) edge_count: usize,
    /// 原始非零对象引用边总数，不包含 MAT 兼容模式合成的 class loader 反向边。
    pub(crate) raw_edge_count: usize,
    /// `java.lang.ref.Reference.referent` 的引用强度统计。
    pub(crate) reference_edge_stats: HprofReferenceEdgeStats,
    /// MAT 兼容模式合成的 class loader 到 class object 强引用边数量。
    pub(crate) synthetic_class_loader_edge_count: usize,
    /// MAT 兼容模式合成的 bootstrap/system class root 数量。
    pub(crate) synthetic_bootstrap_class_root_count: usize,
    /// 所有对象 shallow size 总和。
    pub(crate) total_shallow_size: u64,
    /// 可达对象 shallow size 总和。
    pub(crate) reachable_shallow_size: u64,
    /// 可从 GC root 到达的对象数量。
    pub(crate) reachable_object_count: usize,
    /// 不可达对象数量。
    pub(crate) unreachable_object_count: usize,
    /// 不可达对象 shallow size 总和。
    pub(crate) unreachable_shallow_size: u64,
    /// 当前结果使用的 MAT 兼容 shallow size 估算模型。
    pub(crate) size_model: HprofSizeModel,
    /// sidecar 缓存状态说明。
    ///
    /// 业务意图：
    /// - UI 摘要需要明确告诉用户本次是首次解析还是命中磁盘索引，避免“秒开”时误以为没有执行分析。
    pub(crate) cache_status: Option<String>,
    /// Top retained 根行对象 ID。
    pub(crate) top_object_ids: Vec<HprofObjectId>,
    /// Top retained 根行 summary index。
    ///
    /// 业务意图：
    /// - 可见树渲染只需要 summary index；缓存命中时避免为了 `object_id -> summary_index` 全量建 HashMap。
    pub(crate) top_summary_indices: Vec<usize>,
    /// 可达对象摘要存储。
    pub(crate) summary_storage: HprofSummaryStorage,
    /// 结果展示需要的类名表。
    pub(crate) class_names: FxHashMap<HprofObjectId, String>,
    /// 按线程对象 ID 索引的线程详情。
    pub(crate) thread_details: FxHashMap<HprofObjectId, HprofThreadDetails>,
    /// 按摘要下标索引的直接子节点偏移。
    pub(crate) child_offsets: Vec<usize>,
    /// 按摘要下标连续存储的直接子节点摘要下标。
    pub(crate) child_summary_indices: Vec<usize>,
}
