//! JVM HPROF heap dump 解析和 dominator tree 计算。
//!
//! 业务意图：
//! - HPROF dump 可能来自生产环境，文件通常很大，解析必须放在 UI 线程之外，并通过进度快照给独立窗口展示。
//! - 本模块只包含纯二进制解析、对象图建模和 dominator 计算，不依赖 GPUI，便于单元测试覆盖边界情况。
//!
//! 跨平台约束：
//! - macOS 和 Windows 优先使用只读 mmap 加速大文件随机访问；mmap 创建失败时回退整文件读取，保证权限或平台差异下仍可恢复。
//! - 路径扩展名仅用于用户选择后的第一道校验；真正格式仍以 HPROF header 为准，防止 `.bin` 改名文件误判。
//!
//! 边界条件：
//! - 第一版不设置文件大小或对象数量上限，解析失败、内存不足前的 I/O 错误和不支持记录都转换为中文错误。
//! - dominator tree 默认采用 MAT 对象引用图语义：弱/软/虚引用的 `referent` 仍参与 dominator 计算，同时单独统计引用强度用于诊断。

use std::{
    collections::HashSet,
    error::Error,
    fmt::{self, Display},
    fs::File,
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use crc32fast::Hasher as Crc32Hasher;
use memmap2::Mmap;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

/// HPROF 对象 ID。
///
/// 业务意图：
/// - JVM HPROF 支持 4 字节或 8 字节对象 ID；内部统一提升为 `u64`，避免 UI 和算法层反复分支。
pub(crate) type HprofObjectId = u64;
/// dominator 内部连续节点编号。
///
/// 业务意图：
/// - 5GB dump 中对象和边数量很大，LT 算法会为每个节点维护多组数组；用 `u32` 替代 `usize` 可以把这些热数组内存减半。
type HprofNodeId = u32;
/// `u32` 节点数组中的无效哨兵值。
const HPROF_INVALID_NODE: HprofNodeId = HprofNodeId::MAX;

/// 第一版 dominator 结果默认展示的 retained size Top N。
///
/// 业务意图：
/// - 完整 dominator tree 可能包含数百万对象，首屏只展示 Top retained 节点，再允许用户展开直接支配子节点。
const HPROF_TOP_DOMINATOR_LIMIT: usize = 50;

/// HPROF 文件头允许的最大字符串长度。
///
/// 边界条件：
/// - 标准 header 通常是 `JAVA PROFILE 1.0.2`；超过 1 KiB 基本可以视为损坏或非 HPROF 文件。
const HPROF_HEADER_MAX_BYTES: usize = 1024;

/// 解析进度节流用的对象/子记录批量大小。
///
/// 业务意图：
/// - 大 dump 中对象数可能非常多，如果每个对象都通知 UI，会让锁竞争和窗口刷新吞掉解析性能。
const HPROF_PROGRESS_RECORD_INTERVAL: u64 = 2048;
/// Dominator 和图构建阶段进度节流用的节点/边批量大小。
///
/// 业务意图：
/// - 计算阶段需要可观察，但不能让每条边都跨线程更新进度；约 6.5 万的批量能在大图上保持 UI 有反馈且开销可控。
const HPROF_PROGRESS_WORK_INTERVAL: usize = 65_536;
/// MAT 兼容 size model 中判断 compressed oops 的默认堆大小上限。
///
/// 实现原因：
/// - HPROF header 不可靠携带 HotSpot 压缩指针配置；MAT 常见场景会在 32GiB 以下使用 4 字节普通对象引用。
const HPROF_COMPRESSED_OOPS_HEAP_LIMIT: u64 = 32 * 1024 * 1024 * 1024;
/// MAT 兼容 size model 的默认对象对齐。
const HPROF_MAT_OBJECT_ALIGNMENT: u64 = 8;
/// MAT 兼容 size model 的默认普通对象头大小。
const HPROF_MAT_OBJECT_HEADER_SIZE: u64 = 12;
/// MAT 兼容 size model 的默认数组对象头大小。
const HPROF_MAT_ARRAY_HEADER_SIZE: u64 = 16;
/// 用于 `Thread.name` fallback 的最大字符串数组长度。
///
/// 业务意图：
/// - START_THREAD 通常已经包含线程名；这里仅为少数缺失记录提供兜底，限制长度可以避免把业务大 byte array 作为字符串缓存。
const HPROF_THREAD_STRING_FALLBACK_MAX_ELEMENTS: u32 = 4096;
/// HPROF sidecar 缓存 schema 版本。
///
/// 业务意图：
/// - sidecar 是跨进程、跨版本复用的磁盘格式；任何二进制布局、MAT 语义或结果字段变化都必须 bump 版本，避免误读旧索引。
const HPROF_SIDECAR_SCHEMA_VERSION: u32 = 3;
/// HPROF MAT 兼容语义版本。
///
/// 业务意图：
/// - 即使磁盘字段布局不变，只要 Reference 语义、ClassLoader 合成边或 shallow size model 发生变化，也必须让旧缓存失效。
const HPROF_MAT_SEMANTICS_VERSION: u32 = 1;
/// 源 dump 首尾快速校验块大小。
const HPROF_SIDECAR_CHECKSUM_BYTES: u64 = 1024 * 1024;
/// 大 dump 缓存目录不可写时触发硬错误的阈值。
const HPROF_SIDECAR_REQUIRED_BYTES: u64 = 1024 * 1024 * 1024;
/// sidecar v3 写入缓冲区大小。
///
/// 业务意图：
/// - 5GB dump 的结果索引可能包含数百万对象和子节点记录；大缓冲可以显著降低 macOS/Windows 上的系统调用次数，
///   避免最后“写入索引”阶段因为碎片化小写入拖慢。
const HPROF_SIDECAR_WRITER_BUFFER_BYTES: usize = 32 * 1024 * 1024;
/// sidecar v3 批量编码记录数。
///
/// 边界条件：
/// - chunk 太小会回到频繁写入，太大又会拉高写索引阶段的瞬时内存；65,536 条与已有进度节流保持一致。
const HPROF_SIDECAR_RECORD_CHUNK: usize = 65_536;

/// HPROF 顶层记录：UTF8 字符串。
const TAG_STRING_IN_UTF8: u8 = 0x01;
/// HPROF 顶层记录：类加载信息。
const TAG_LOAD_CLASS: u8 = 0x02;
/// HPROF 顶层记录：线程栈帧。
const TAG_STACK_FRAME: u8 = 0x04;
/// HPROF 顶层记录：线程栈轨迹。
const TAG_STACK_TRACE: u8 = 0x05;
/// HPROF 顶层记录：线程启动信息。
const TAG_START_THREAD: u8 = 0x0A;
/// HPROF 顶层记录：线程结束信息。
const TAG_END_THREAD: u8 = 0x0B;
/// HPROF 顶层记录：完整 heap dump。
const TAG_HEAP_DUMP: u8 = 0x0C;
/// HPROF 顶层记录：分段 heap dump。
const TAG_HEAP_DUMP_SEGMENT: u8 = 0x1C;
/// HPROF 顶层记录：heap dump 结束标记。
const TAG_HEAP_DUMP_END: u8 = 0x2C;

/// HPROF 子记录：未知 GC Root。
const SUB_ROOT_UNKNOWN: u8 = 0xFF;
/// HPROF 子记录：JNI Global Root。
const SUB_ROOT_JNI_GLOBAL: u8 = 0x01;
/// HPROF 子记录：JNI Local Root。
const SUB_ROOT_JNI_LOCAL: u8 = 0x02;
/// HPROF 子记录：Java Frame Root。
const SUB_ROOT_JAVA_FRAME: u8 = 0x03;
/// HPROF 子记录：Native Stack Root。
const SUB_ROOT_NATIVE_STACK: u8 = 0x04;
/// HPROF 子记录：System Class Root。
const SUB_ROOT_STICKY_CLASS: u8 = 0x05;
/// HPROF 子记录：Thread Block Root。
const SUB_ROOT_THREAD_BLOCK: u8 = 0x06;
/// HPROF 子记录：Monitor Used Root。
const SUB_ROOT_MONITOR_USED: u8 = 0x07;
/// HPROF 子记录：Thread Object Root。
const SUB_ROOT_THREAD_OBJECT: u8 = 0x08;
/// HPROF 子记录：Class Dump。
const SUB_CLASS_DUMP: u8 = 0x20;
/// HPROF 子记录：Instance Dump。
const SUB_INSTANCE_DUMP: u8 = 0x21;
/// HPROF 子记录：Object Array Dump。
const SUB_OBJECT_ARRAY_DUMP: u8 = 0x22;
/// HPROF 子记录：Primitive Array Dump。
const SUB_PRIMITIVE_ARRAY_DUMP: u8 = 0x23;
/// HPROF 子记录：Heap Dump Info。
const SUB_HEAP_DUMP_INFO: u8 = 0xFE;
/// HPROF 子记录：Primitive Array No Data。
const SUB_PRIMITIVE_ARRAY_NODATA: u8 = 0xC3;

/// HPROF 字段类型：对象引用。
const FIELD_TYPE_OBJECT: u8 = 2;
/// HPROF 字段类型：boolean。
const FIELD_TYPE_BOOLEAN: u8 = 4;
/// HPROF 字段类型：char。
const FIELD_TYPE_CHAR: u8 = 5;
/// HPROF 字段类型：float。
const FIELD_TYPE_FLOAT: u8 = 6;
/// HPROF 字段类型：double。
const FIELD_TYPE_DOUBLE: u8 = 7;
/// HPROF 字段类型：byte。
const FIELD_TYPE_BYTE: u8 = 8;
/// HPROF 字段类型：short。
const FIELD_TYPE_SHORT: u8 = 9;
/// HPROF 字段类型：int。
const FIELD_TYPE_INT: u8 = 10;
/// HPROF 字段类型：long。
const FIELD_TYPE_LONG: u8 = 11;

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

/// HPROF header 信息。
///
/// 业务意图：
/// - header 中的格式标签和 ID 字节数会影响整份 dump 的后续解析，完成后也应在 UI 摘要中展示给用户核对。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HprofHeader {
    /// 原始格式标签，例如 `JAVA PROFILE 1.0.2`。
    pub(crate) label: String,
    /// 对象 ID 字节数，只支持 4 或 8。
    pub(crate) identifier_size: u8,
    /// HPROF header 中记录的时间戳毫秒值。
    pub(crate) timestamp_millis: u64,
}

/// MAT 兼容 shallow size 估算模型。
///
/// 业务意图：
/// - HPROF 记录中的 `instance_size` 不等价于 MAT 展示的 shallow heap；MAT 会按 JVM 对象头、数组头、引用宽度和对象对齐估算。
/// - 本结构把这些估算参数固化到结果中，UI 可以明确展示当前采用的语义，避免和原始 HPROF 字节长度混淆。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HprofSizeModel {
    /// 普通对象头大小，单位字节。
    pub(crate) object_header_size: u64,
    /// 数组对象头大小，单位字节。
    pub(crate) array_header_size: u64,
    /// 对象对齐粒度，单位字节。
    pub(crate) object_alignment: u64,
    /// JVM 堆内普通对象引用宽度，不一定等于 HPROF 文件中的对象 ID 宽度。
    pub(crate) reference_size: u8,
    /// 是否按 compressed oops 估算普通对象引用宽度。
    pub(crate) compressed_oops: bool,
}

impl HprofSizeModel {
    /// 根据 HPROF header 和文件规模选择默认 MAT 兼容模型。
    ///
    /// 边界条件：
    /// - HPROF ID size 为 4 时，堆内引用宽度按 4 字节处理。
    /// - HPROF ID size 为 8 且文件规模低于 32GiB 时，默认按 HotSpot compressed oops 的 4 字节引用估算。
    /// - 文件大小只是 dump 规模近似值；真实 JVM 参数无法从 HPROF 稳定恢复，后续可在 UI 增加手动覆盖。
    fn mat_compatible(header: &HprofHeader, source_bytes: u64) -> Self {
        let compressed_oops =
            header.identifier_size == 8 && source_bytes < HPROF_COMPRESSED_OOPS_HEAP_LIMIT;
        let reference_size = if compressed_oops || header.identifier_size == 4 {
            4
        } else {
            8
        };
        Self {
            object_header_size: HPROF_MAT_OBJECT_HEADER_SIZE,
            array_header_size: HPROF_MAT_ARRAY_HEADER_SIZE,
            object_alignment: HPROF_MAT_OBJECT_ALIGNMENT,
            reference_size,
            compressed_oops,
        }
    }

    /// 返回 UI 摘要中展示的 size model 文案。
    pub(crate) fn description(&self) -> String {
        let reference_mode = if self.compressed_oops {
            "compressed oops"
        } else {
            "普通引用"
        };
        format!(
            "MAT兼容 shallow: {reference_mode} / {}-byte align",
            self.object_alignment
        )
    }

    /// 按当前对象对齐粒度向上取整。
    fn align_size(&self, size: u64) -> u64 {
        align_size(size, self.object_alignment)
    }
}

/// HPROF 对象类型。
///
/// 业务意图：
/// - UI 展示对象行时需要区分类对象、普通实例、对象数组和基础类型数组，便于用户理解 retained size 来源。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum HprofObjectKind {
    /// Java Class 对象。
    Class,
    /// 普通实例对象。
    Instance,
    /// 对象数组。
    ObjectArray {
        /// 数组元素数量。
        length: u32,
    },
    /// 基础类型数组。
    PrimitiveArray {
        /// HPROF 基础类型编码。
        element_type: u8,
        /// 数组元素数量。
        length: u32,
    },
}

/// 单个 heap 对象。
///
/// 业务意图：
/// - dominator tree 只需要对象 ID、类 ID、shallow size 和出边；其它字段保留给 UI 展示和后续 GC path 扩展。
#[derive(Clone, Debug)]
pub(crate) struct HprofHeapObject {
    /// 对象 ID。
    pub(crate) id: HprofObjectId,
    /// 对象所属类 ID；Class 对象使用自身 ID。
    pub(crate) class_id: HprofObjectId,
    /// MAT 兼容 shallow size。
    ///
    /// 实现原因：
    /// - 该值使用 HotSpot 常见对象布局估算，不再直接等同于 HPROF `instance_size` 或数组元素原始字节数。
    pub(crate) shallow_size: u64,
    /// 对象类型。
    pub(crate) kind: HprofObjectKind,
}

/// 已解析的类信息。
///
/// 业务意图：
/// - instance dump 只保存原始字段字节，必须借助 class dump 的字段布局才能识别哪些字段是对象引用。
#[derive(Clone, Debug)]
pub(crate) struct HprofClassInfo {
    /// HPROF `CLASS_DUMP` 中记录的实例数据长度。
    ///
    /// 边界条件：
    /// - 该值用于字段布局缺失时的兜底 shallow 估算；正常 MAT 兼容路径会按字段类型和 size model 重新计算。
    pub(crate) instance_size: u64,
    /// 解析后的类名。
    pub(crate) name: Option<String>,
    /// 本类声明的实例字段。
    pub(crate) instance_fields: Vec<HprofFieldDescriptor>,
}

/// 字段描述。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HprofFieldDescriptor {
    /// 字段名。
    pub(crate) name: Option<String>,
    /// HPROF 字段类型编码。
    pub(crate) field_type: u8,
}

/// HPROF 对象引用在 MAT 兼容对象图中的强度。
///
/// 业务意图：
/// - MAT 的 dominator tree 基于对象引用图，`java.lang.ref.Reference.referent` 仍参与可达性和 retained 计算。
/// - 引用强度只用于结果诊断，帮助用户理解哪些 retained 结果来自 weak/soft/phantom/finalizer referent。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HprofReferenceStrength {
    /// 普通强引用。
    Strong,
    /// `WeakReference` 或其子类的 referent。
    WeakLike,
    /// `SoftReference` 或其子类的 referent。
    SoftLike,
    /// `PhantomReference` 或其子类的 referent。
    PhantomLike,
    /// `FinalReference` 或其子类的 referent。
    FinalizerLike,
}

/// `java.lang.ref.Reference.referent` 边的分类统计。
///
/// 业务意图：
/// - 默认 dominator tree 不再剪掉这些边以便对齐 MAT；但 UI 仍需要告诉用户结果中有多少 weak/soft/phantom/finalizer referent 参与了对象图。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HprofReferenceEdgeStats {
    /// `WeakReference` 或其子类的 referent 边数量。
    pub(crate) weak_like: usize,
    /// `SoftReference` 或其子类的 referent 边数量。
    pub(crate) soft_like: usize,
    /// `PhantomReference` 或其子类的 referent 边数量。
    pub(crate) phantom_like: usize,
    /// `FinalReference` 或其子类的 referent 边数量。
    pub(crate) finalizer_like: usize,
}

impl HprofReferenceEdgeStats {
    /// 按引用强度累加统计。
    fn add_strength(&mut self, strength: HprofReferenceStrength) {
        match strength {
            HprofReferenceStrength::Strong => {}
            HprofReferenceStrength::WeakLike => {
                self.weak_like = self.weak_like.saturating_add(1);
            }
            HprofReferenceStrength::SoftLike => {
                self.soft_like = self.soft_like.saturating_add(1);
            }
            HprofReferenceStrength::PhantomLike => {
                self.phantom_like = self.phantom_like.saturating_add(1);
            }
            HprofReferenceStrength::FinalizerLike => {
                self.finalizer_like = self.finalizer_like.saturating_add(1);
            }
        }
    }

    /// 合并另一份统计。
    fn merge(&mut self, other: Self) {
        self.weak_like = self.weak_like.saturating_add(other.weak_like);
        self.soft_like = self.soft_like.saturating_add(other.soft_like);
        self.phantom_like = self.phantom_like.saturating_add(other.phantom_like);
        self.finalizer_like = self.finalizer_like.saturating_add(other.finalizer_like);
    }

    /// 返回所有非普通强引用 referent 边数量。
    pub(crate) fn total(&self) -> usize {
        self.weak_like
            .saturating_add(self.soft_like)
            .saturating_add(self.phantom_like)
            .saturating_add(self.finalizer_like)
    }

    /// 返回 UI 摘要展示的分类文案。
    pub(crate) fn description(&self) -> String {
        format!(
            "{} (weak {}, soft {}, phantom {}, finalizer {})",
            self.total(),
            self.weak_like,
            self.soft_like,
            self.phantom_like,
            self.finalizer_like
        )
    }
}

/// GC Root 类型。
///
/// 业务意图：
/// - 第一版 dominator tree 只需要 Root 指向的对象 ID；Root 类型仍保留，便于后续 UI 展示 GC root 来源。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HprofGcRootKind {
    /// UNKNOWN root。
    Unknown,
    /// JNI Global root。
    JniGlobal,
    /// JNI Local root。
    JniLocal,
    /// Java Frame root。
    JavaFrame,
    /// Native Stack root。
    NativeStack,
    /// Sticky Class root。
    StickyClass,
    /// Thread Block root。
    ThreadBlock,
    /// Monitor Used root。
    MonitorUsed,
    /// Thread Object root。
    ThreadObject,
    /// MAT 兼容模式合成的 bootstrap/system class root。
    ///
    /// 业务意图：
    /// - HPROF `CLASS_DUMP.class_loader_id == 0` 表示由 bootstrap/system 类加载器管理；MAT 会把这类 class object 当作可达入口，
    ///   否则 class 的 static 字段会断开，缓存、单例等对象会被误判为不可达。
    SyntheticBootstrapClass,
}

/// GC Root 记录。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HprofGcRoot {
    /// root 指向的对象 ID。
    pub(crate) object_id: HprofObjectId,
    /// root 类型。
    pub(crate) kind: HprofGcRootKind,
}

/// 完整 HPROF 对象图。
///
/// 业务意图：
/// - 该结构是 parser 和 dominator 算法之间的边界，测试可直接构造它验证 dominator 规则。
#[derive(Clone, Debug)]
pub(crate) struct HprofObjectGraph {
    /// HPROF header。
    pub(crate) header: HprofHeader,
    /// 当前结果使用的 MAT 兼容 shallow size 估算模型。
    pub(crate) size_model: HprofSizeModel,
    /// 所有对象，按解析顺序连续存储。
    ///
    /// 业务意图：
    /// - dominator 算法和 retained size 聚合都会反复遍历对象；连续数组比按 ID 散列表遍历更少 cache miss。
    pub(crate) objects: Vec<HprofHeapObject>,
    /// 对象 ID 到连续对象下标的映射。
    ///
    /// 业务意图：
    /// - HPROF 引用仍以对象 ID 表达；解析和建图只在需要跳转时查映射，避免所有主流程都围绕 HashMap values 迭代。
    object_indices: FxHashMap<HprofObjectId, usize>,
    /// 每个对象当前引用段在 `reference_targets` 中的起始偏移。
    ///
    /// 业务意图：
    /// - 大 dump 中绝大多数对象引用数很少，如果每个对象都持有独立 `Vec`，数百万空 Vec 的结构体开销会非常高；
    ///   这里把所有引用统一放入连续池，按对象下标保存 offset/len，降低首次解析匿名堆峰值。
    reference_offsets: Vec<usize>,
    /// 每个对象当前引用段长度。
    reference_lengths: Vec<usize>,
    /// 所有对象引用目标 ID 的连续池。
    reference_targets: Vec<HprofObjectId>,
    /// 类元数据，按类对象 ID 索引。
    pub(crate) classes: FxHashMap<HprofObjectId, HprofClassInfo>,
    /// GC Root 列表。
    pub(crate) gc_roots: Vec<HprofGcRoot>,
    /// 已收集 MAT 兼容对象引用数量。
    edge_count: usize,
    /// `java.lang.ref.Reference.referent` 的引用强度统计。
    reference_edge_stats: HprofReferenceEdgeStats,
    /// MAT 兼容模式合成的 class loader 到 class object 强引用边数量。
    synthetic_class_loader_edge_count: usize,
    /// MAT 兼容模式合成的 bootstrap/system class root 数量。
    synthetic_bootstrap_class_root_count: usize,
    /// 已收集对象 shallow size 总和。
    total_shallow_size: u64,
    /// UTF8 字符串表。
    strings: FxHashMap<HprofObjectId, String>,
    /// LOAD_CLASS 记录，值为 `(class_object_id, class_name_string_id)`。
    loaded_classes: FxHashMap<u32, (HprofObjectId, HprofObjectId)>,
    /// START_THREAD 和 ROOT_THREAD_OBJECT 提供的线程对象到线程序号映射。
    ///
    /// 业务意图：
    /// - MAT Thread Details 需要从线程对象跳转到 `STACK_TRACE`；HPROF 中二者通过 thread serial 间接关联。
    thread_serial_by_object_id: FxHashMap<HprofObjectId, u32>,
    /// START_THREAD 记录，按线程序号索引。
    thread_starts: FxHashMap<u32, HprofThreadStartRecord>,
    /// ROOT_THREAD_OBJECT 记录，按线程对象 ID 索引。
    thread_object_roots: FxHashMap<HprofObjectId, HprofThreadObjectRootRecord>,
    /// STACK_TRACE 记录，按 stack trace serial 索引。
    stack_traces: FxHashMap<u32, HprofStackTraceRecord>,
    /// STACK_FRAME 记录，按 frame ID 索引。
    stack_frames: FxHashMap<HprofObjectId, HprofStackFrameRecord>,
    /// 解析实例字段后得到的线程对象属性原始值。
    thread_instances: FxHashMap<HprofObjectId, HprofThreadInstanceInfo>,
    /// 可从 `java.lang.String` 实例解析出的字符串值。
    ///
    /// 边界条件：
    /// - 仅用于线程名 fallback 等轻量展示；解析失败不影响 dominator tree 正确性。
    java_string_values: FxHashMap<HprofObjectId, String>,
}

impl HprofObjectGraph {
    /// 创建空对象图。
    fn new(header: HprofHeader, size_model: HprofSizeModel) -> Self {
        Self {
            header,
            size_model,
            objects: Vec::new(),
            object_indices: FxHashMap::default(),
            reference_offsets: Vec::new(),
            reference_lengths: Vec::new(),
            reference_targets: Vec::new(),
            classes: FxHashMap::default(),
            gc_roots: Vec::new(),
            edge_count: 0,
            reference_edge_stats: HprofReferenceEdgeStats::default(),
            synthetic_class_loader_edge_count: 0,
            synthetic_bootstrap_class_root_count: 0,
            total_shallow_size: 0,
            strings: FxHashMap::default(),
            loaded_classes: FxHashMap::default(),
            thread_serial_by_object_id: FxHashMap::default(),
            thread_starts: FxHashMap::default(),
            thread_object_roots: FxHashMap::default(),
            stack_traces: FxHashMap::default(),
            stack_frames: FxHashMap::default(),
            thread_instances: FxHashMap::default(),
            java_string_values: FxHashMap::default(),
        }
    }

    /// 插入或替换一个对象。
    ///
    /// 业务意图：
    /// - 真实 dump 理论上不应重复定义对象 ID，但损坏文件可能出现重复；这里按最后一次记录覆盖，并同步维护增量统计。
    fn insert_object(
        &mut self,
        object: HprofHeapObject,
        mut references: Vec<HprofObjectId>,
    ) -> Result<(), HprofError> {
        normalize_references(&mut references);
        if self.object_indices.contains_key(&object.id) {
            return Err(HprofError::InvalidFormat(format!(
                "HPROF 对象重复定义：0x{:x}",
                object.id
            )));
        }

        let index = self.objects.len();
        let reference_offset = self.reference_targets.len();
        let reference_len = references.len();
        self.reference_targets.extend(references);
        self.reference_offsets.push(reference_offset);
        self.reference_lengths.push(reference_len);
        self.edge_count = self.edge_count.saturating_add(reference_len);
        self.total_shallow_size = self.total_shallow_size.saturating_add(object.shallow_size);
        self.object_indices.insert(object.id, index);
        self.objects.push(object);
        Ok(())
    }

    /// 更新已存在对象的 shallow size 和对象引用列表。
    ///
    /// 业务意图：
    /// - MAT 兼容路径需要先解析类名和字段名，再判断哪些字段是弱/软/虚引用；因此对象会先以空引用插入，解析完成后统一补齐。
    fn update_object_payload(
        &mut self,
        object_id: HprofObjectId,
        shallow_size: u64,
        mut references: Vec<HprofObjectId>,
    ) {
        normalize_references(&mut references);
        if let Some(index) = self.object_indices.get(&object_id).copied() {
            let previous = &self.objects[index];
            self.edge_count = self
                .edge_count
                .saturating_sub(self.reference_lengths.get(index).copied().unwrap_or(0));
            self.total_shallow_size = self
                .total_shallow_size
                .saturating_sub(previous.shallow_size);
            let reference_offset = self.reference_targets.len();
            let reference_len = references.len();
            self.reference_targets.extend(references);
            if let Some(offset) = self.reference_offsets.get_mut(index) {
                *offset = reference_offset;
            }
            if let Some(length) = self.reference_lengths.get_mut(index) {
                *length = reference_len;
            }
            self.edge_count = self.edge_count.saturating_add(reference_len);
            self.total_shallow_size = self.total_shallow_size.saturating_add(shallow_size);
            self.objects[index].shallow_size = shallow_size;
        }
    }

    /// 返回指定对象下标的当前引用目标 ID 切片。
    fn references_for_index(&self, object_index: usize) -> &[HprofObjectId] {
        let start = self
            .reference_offsets
            .get(object_index)
            .copied()
            .unwrap_or(0);
        let len = self
            .reference_lengths
            .get(object_index)
            .copied()
            .unwrap_or(0);
        let end = start.saturating_add(len).min(self.reference_targets.len());
        &self.reference_targets[start..end]
    }

    /// 释放解析阶段保留的原始对象引用 ID 池。
    ///
    /// 业务意图：
    /// - `build_compact_adjacency` 已把对象引用转换成 `u32` 连续下标；后续 LT、retained 聚合和 UI 结果构造只需要对象摘要，
    ///   及时丢弃 `u64` 引用池可以避免大 dump 在 dominator 阶段同时保留两份边数据。
    fn clear_reference_pool(&mut self) {
        self.reference_offsets = Vec::new();
        self.reference_lengths = Vec::new();
        self.reference_targets = Vec::new();
    }

    /// 返回对象数量。
    fn object_count(&self) -> usize {
        self.objects.len()
    }

    /// 返回类数量。
    fn class_count(&self) -> usize {
        self.classes.len()
    }

    /// 按对象 ID 返回连续对象下标。
    fn object_index(&self, object_id: HprofObjectId) -> Option<usize> {
        self.object_indices.get(&object_id).copied()
    }

    /// 判断对象 ID 是否存在。
    fn contains_object_id(&self, object_id: HprofObjectId) -> bool {
        self.object_indices.contains_key(&object_id)
    }

    /// 返回对象引用边数量。
    pub(crate) fn edge_count(&self) -> usize {
        self.edge_count
    }

    /// 返回原始非零对象引用数量，不包含 MAT 兼容模式合成的 class loader 反向边。
    pub(crate) fn raw_edge_count(&self) -> usize {
        self.edge_count
            .saturating_sub(self.synthetic_class_loader_edge_count)
    }

    /// 返回 Reference referent 边分类统计。
    pub(crate) fn reference_edge_stats(&self) -> &HprofReferenceEdgeStats {
        &self.reference_edge_stats
    }

    /// 返回 MAT 兼容模式合成的 class loader 强引用边数量。
    pub(crate) fn synthetic_class_loader_edge_count(&self) -> usize {
        self.synthetic_class_loader_edge_count
    }

    /// 返回 MAT 兼容模式合成的 bootstrap/system class root 数量。
    pub(crate) fn synthetic_bootstrap_class_root_count(&self) -> usize {
        self.synthetic_bootstrap_class_root_count
    }

    /// 返回所有对象的 shallow size 总和。
    pub(crate) fn total_shallow_size(&self) -> u64 {
        self.total_shallow_size
    }

    /// 提取 UI 展示需要的类名表。
    fn class_names_for_result(&self) -> FxHashMap<HprofObjectId, String> {
        self.classes
            .iter()
            .filter_map(|(class_id, class_info)| {
                class_info
                    .name
                    .as_ref()
                    .map(|name| (*class_id, name.clone()))
            })
            .collect()
    }
}

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
struct HprofDominatorObjectSummary {
    /// 对象 ID。
    object_id: HprofObjectId,
    /// 对象所属类 ID。
    class_id: HprofObjectId,
    /// 对象类型。
    kind: HprofObjectKind,
    /// shallow size。
    shallow_size: u64,
    /// retained size。
    retained_size: u64,
    /// retained size 占比。
    retained_percent: f32,
    /// 直接支配子节点数量。
    direct_child_count: usize,
}

/// Dominator 对象摘要的存储后端。
///
/// 业务意图：
/// - 首次解析时摘要来自内存构造结果；缓存命中时不应为了展示首屏重新读取全部对象摘要。
/// - UI 渲染 dominator tree 实际只需要 Top 节点和已展开节点的摘要，因此 sidecar 后端按 summary index 随用随读。
#[derive(Debug)]
enum HprofSummaryStorage {
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
    top_summary_indices: Vec<usize>,
    /// 可达对象摘要存储。
    summary_storage: HprofSummaryStorage,
    /// 结果展示需要的类名表。
    class_names: FxHashMap<HprofObjectId, String>,
    /// 按线程对象 ID 索引的线程详情。
    thread_details: FxHashMap<HprofObjectId, HprofThreadDetails>,
    /// 按摘要下标索引的直接子节点偏移。
    child_offsets: Vec<usize>,
    /// 按摘要下标连续存储的直接子节点摘要下标。
    child_summary_indices: Vec<usize>,
}

impl HprofDominatorResult {
    /// 根据展开集合生成 UI 当前可见行。
    ///
    /// 边界条件：
    /// - Top retained 节点和展开子节点可能在树上重叠；第一版保留这种重复展示，避免隐藏用户明确展开的对象上下文。
    pub(crate) fn visible_rows(
        &self,
        expanded_node_ids: &HashSet<HprofObjectId>,
    ) -> Vec<HprofDominatorRow> {
        let mut rows = Vec::new();
        for summary_index in &self.top_summary_indices {
            self.append_visible_row_by_summary_index(
                *summary_index,
                0,
                expanded_node_ids,
                &mut rows,
            );
        }
        rows
    }

    /// 按对象 ID 生成可展示节点。
    ///
    /// 业务意图：
    /// - 测试和 UI 展开都只需要当前关注的节点；延迟拼接名称可以避免完成阶段为所有对象分配展示字符串。
    #[cfg(test)]
    pub(crate) fn node_for_object_id(
        &self,
        object_id: HprofObjectId,
    ) -> Option<HprofDominatorNode> {
        let summary_index = self.summary_index_for_object_id(object_id)?;
        let summary = self.summary_at_index(summary_index)?;
        self.build_node(&summary)
    }

    /// 返回指定对象的线程详情。
    ///
    /// 业务意图：
    /// - HPROF 分析窗口右键菜单只根据完成结果判断是否允许打开线程详情，避免 UI 再持有解析器内部状态。
    pub(crate) fn thread_details_for_object_id(
        &self,
        object_id: HprofObjectId,
    ) -> Option<&HprofThreadDetails> {
        self.thread_details.get(&object_id)
    }

    /// 判断指定对象是否有可展示线程详情。
    pub(crate) fn is_thread_object(&self, object_id: HprofObjectId) -> bool {
        self.thread_details.contains_key(&object_id)
    }

    /// 递归展开单个 summary index 节点。
    fn append_visible_row_by_summary_index(
        &self,
        summary_index: usize,
        depth: usize,
        expanded_node_ids: &HashSet<HprofObjectId>,
        rows: &mut Vec<HprofDominatorRow>,
    ) {
        let Some(summary) = self.summary_at_index(summary_index) else {
            return;
        };
        let object_id = summary.object_id;
        let Some(node) = self.build_node(&summary) else {
            return;
        };
        rows.push(HprofDominatorRow { depth, node });
        if !expanded_node_ids.contains(&object_id) {
            return;
        }
        let child_start = self.child_offsets.get(summary_index).copied().unwrap_or(0);
        let child_end = self
            .child_offsets
            .get(summary_index + 1)
            .copied()
            .unwrap_or(child_start);
        for child_summary_index in &self.child_summary_indices[child_start..child_end] {
            self.append_visible_row_by_summary_index(
                *child_summary_index,
                depth + 1,
                expanded_node_ids,
                rows,
            );
        }
    }

    /// 返回指定 summary index 的对象摘要。
    ///
    /// 边界条件：
    /// - sidecar 缓存可能损坏或被外部删除；这里返回 `None` 让 UI 少展示该行，而不是在渲染阶段 panic。
    fn summary_at_index(&self, summary_index: usize) -> Option<HprofDominatorObjectSummary> {
        match &self.summary_storage {
            HprofSummaryStorage::Owned { summaries } => summaries.get(summary_index).cloned(),
            HprofSummaryStorage::Sidecar {
                count,
                reader,
                cache,
                ..
            } => {
                if summary_index >= *count {
                    return None;
                }
                if let Some(summary) = cache.borrow().get(&summary_index).cloned() {
                    return Some(summary);
                }
                let summary = hprof_cache::read_object_summary_from_reader(
                    &mut *reader.borrow_mut(),
                    summary_index,
                    *count,
                )
                .ok()?;
                cache.borrow_mut().insert(summary_index, summary.clone());
                Some(summary)
            }
        }
    }

    /// 返回当前结果中的可达对象摘要数量。
    fn summary_count(&self) -> usize {
        match &self.summary_storage {
            HprofSummaryStorage::Owned { summaries } => summaries.len(),
            HprofSummaryStorage::Sidecar { count, .. } => *count,
        }
    }

    /// 按对象 ID 查找 summary index。
    ///
    /// 业务意图：
    /// - UI 主路径已经使用 summary index 展开树；该方法主要服务测试和少量外部查询。
    /// - sidecar 命中时不预建全量 HashMap，而是在确实需要按 ID 查询时线性扫描一次并缓存已命中的对象。
    #[cfg(test)]
    fn summary_index_for_object_id(&self, object_id: HprofObjectId) -> Option<usize> {
        match &self.summary_storage {
            HprofSummaryStorage::Owned { summaries } => summaries
                .iter()
                .position(|summary| summary.object_id == object_id),
            HprofSummaryStorage::Sidecar {
                count,
                reader,
                object_index_cache,
                ..
            } => {
                if let Some(index) = object_index_cache.borrow().get(&object_id).copied() {
                    return Some(index);
                }
                let mut reader = reader.borrow_mut();
                for summary_index in 0..*count {
                    let summary = hprof_cache::read_object_summary_from_reader(
                        &mut *reader,
                        summary_index,
                        *count,
                    )
                    .ok()?;
                    object_index_cache
                        .borrow_mut()
                        .insert(summary.object_id, summary_index);
                    if summary.object_id == object_id {
                        return Some(summary_index);
                    }
                }
                None
            }
        }
    }

    /// 根据紧凑摘要构造 UI 行节点。
    fn build_node(&self, summary: &HprofDominatorObjectSummary) -> Option<HprofDominatorNode> {
        Some(HprofDominatorNode {
            object_id: summary.object_id,
            name: self.object_display_name(summary),
            shallow_size: summary.shallow_size,
            retained_size: summary.retained_size,
            retained_percent: summary.retained_percent,
            direct_child_count: summary.direct_child_count,
        })
    }

    /// 生成对象展示名。
    fn object_display_name(&self, summary: &HprofDominatorObjectSummary) -> String {
        match &summary.kind {
            HprofObjectKind::Class => self
                .class_names
                .get(&summary.object_id)
                .map(|name| format!("Class {name}"))
                .unwrap_or_else(|| format!("Class 0x{:x}", summary.object_id)),
            HprofObjectKind::PrimitiveArray { element_type, .. } => {
                format!("{}[]", primitive_type_label(*element_type))
            }
            HprofObjectKind::ObjectArray { .. } | HprofObjectKind::Instance => self
                .class_names
                .get(&summary.class_id)
                .map(|class_name| {
                    if let Some(details) = self.thread_details.get(&summary.object_id) {
                        format!("{class_name} {}", details.thread_name)
                    } else {
                        class_name.clone()
                    }
                })
                .unwrap_or_else(|| format!("<unknown class 0x{:x}>", summary.class_id)),
        }
    }
}

/// HPROF sidecar 缓存读写。
///
/// 业务意图：
/// - 大 dump 的 dominator 计算成本很高；完成后把 UI 所需的紧凑结果写到源文件同目录，后续打开可以跳过解析和 LT 计算。
/// - manifest 使用 JSON 方便人工诊断，主体数据使用稳定手写二进制，避免 serde 对大 Vec 产生额外中间分配。
mod hprof_cache {
    use super::*;

    const MANIFEST_FILE: &str = "manifest.json";
    const OBJECTS_FILE: &str = "objects.bin";
    const DOMINATOR_FILE: &str = "dominator.bin";
    const CLASSES_FILE: &str = "classes.bin";
    const THREADS_FILE: &str = "threads.bin";
    const STRINGS_FILE: &str = "strings.bin";
    const EDGES_OFFSETS_FILE: &str = "edges_offsets.bin";
    const EDGES_TARGETS_FILE: &str = "edges_targets.bin";
    const ROOT_INDICES_FILE: &str = "root_indices.bin";
    const MAGIC_OBJECTS: &[u8] = b"LCHP-objects-v3";
    const MAGIC_DOMINATOR: &[u8] = b"LCHP-dominator-v3";
    const MAGIC_CLASSES: &[u8] = b"LCHP-classes-v2";
    const MAGIC_THREADS: &[u8] = b"LCHP-threads-v2";
    const MAGIC_EMPTY: &[u8] = b"LCHP-empty-v2";
    const MAX_MAGIC_BYTES: usize = 256;
    const MAX_STRING_BYTES: usize = 16 * 1024 * 1024;
    /// `objects.bin` 固定摘要记录长度。
    ///
    /// 业务意图：
    /// - 缓存命中时需要按 summary index 随机读取可见行摘要；固定记录长度可直接 seek，避免第二次打开全量反序列化对象摘要。
    const OBJECT_SUMMARY_RECORD_BYTES: u64 = 50;

    /// sidecar manifest。
    ///
    /// 边界条件：
    /// - `state` 只有 `complete` 才允许命中；构建中或崩溃残留目录不参与缓存恢复。
    #[derive(Clone, Debug, Serialize, Deserialize)]
    struct HprofSidecarManifest {
        schema_version: u32,
        mat_semantics_version: u32,
        logclinic_version: String,
        state: String,
        source_file_name: String,
        source_len: u64,
        source_modified_millis: u128,
        head_crc32: u32,
        tail_crc32: u32,
        header: HprofHeader,
        size_model: HprofSizeModel,
        created_millis: u128,
    }

    /// 源 dump 快速身份信息。
    struct SourceIdentity {
        file_name: String,
        len: u64,
        modified_millis: u128,
        head_crc32: u32,
        tail_crc32: u32,
    }

    /// 尝试从 sidecar 缓存恢复结果。
    pub(super) fn try_load_cached_hprof_result<F>(
        path: &Path,
        progress: &mut HprofProgress,
        progress_reporter: &mut F,
        cancel_flag: &AtomicBool,
    ) -> Option<Result<HprofDominatorResult, HprofError>>
    where
        F: FnMut(HprofProgress),
    {
        progress.stage = HprofAnalysisStage::CheckingCache;
        progress.message = "正在检查 HPROF sidecar 缓存".to_string();
        progress.sub_message.clear();
        progress_reporter(progress.clone());

        let sidecar_dir = sidecar_dir_for_path(path);
        let manifest_path = sidecar_dir.join(MANIFEST_FILE);
        if !manifest_path.is_file() {
            progress.sub_message = "未发现可用 sidecar index，将重新解析".to_string();
            progress_reporter(progress.clone());
            return None;
        }

        let loaded = (|| {
            check_cancel(cancel_flag)?;
            let manifest = read_manifest(&manifest_path)?;
            let identity = source_identity(path)?;
            let invalid_reason = validate_manifest(path, &manifest, &identity);
            if let Some(reason) = invalid_reason {
                progress.sub_message = format!("sidecar index 失效：{reason}");
                progress_reporter(progress.clone());
                return Ok(None);
            }
            ensure_required_files(&sidecar_dir)?;
            progress.stage = HprofAnalysisStage::LoadingCache;
            progress.message = "正在加载 HPROF sidecar index".to_string();
            progress.total_bytes = sidecar_total_bytes(&sidecar_dir)?;
            progress.bytes_read = 0;
            progress.phase_done = 0;
            progress.phase_total = 1;
            progress.phase_unit = "文件";
            progress.sub_message = "准备读取 sidecar index".to_string();
            progress_reporter(progress.clone());
            let mut result = read_cached_result(
                path,
                &sidecar_dir,
                &manifest,
                progress,
                progress_reporter,
                cancel_flag,
            )?;
            result.cache_status = Some("sidecar index 命中".to_string());
            Ok(Some(result))
        })();

        match loaded {
            Ok(Some(result)) => Some(Ok(result)),
            Ok(None) => None,
            Err(HprofError::Canceled) => Some(Err(HprofError::Canceled)),
            Err(error) => {
                progress.sub_message = format!("sidecar index 无法读取，将重新解析：{error}");
                progress_reporter(progress.clone());
                None
            }
        }
    }

    /// 在大 dump 解析前确认 sidecar 目录可写。
    pub(super) fn ensure_sidecar_writable(
        path: &Path,
        source_len: u64,
    ) -> Result<bool, HprofError> {
        let sidecar_dir = sidecar_dir_for_path(path);
        match std::fs::create_dir_all(&sidecar_dir) {
            Ok(()) => Ok(true),
            Err(error) if source_len >= HPROF_SIDECAR_REQUIRED_BYTES => {
                Err(HprofError::Io(format!(
                    "无法创建过程索引文件目录 {}：{}",
                    sidecar_dir.display(),
                    error
                )))
            }
            Err(_) => Ok(false),
        }
    }

    /// 写入完成结果到 sidecar。
    pub(super) fn write_hprof_sidecar_result<F>(
        result: &HprofDominatorResult,
        progress: &mut HprofProgress,
        progress_reporter: &mut F,
        cancel_flag: &AtomicBool,
    ) -> Result<(), HprofError>
    where
        F: FnMut(HprofProgress),
    {
        let sidecar_dir = sidecar_dir_for_path(&result.file_path);
        std::fs::create_dir_all(&sidecar_dir).map_err(|error| {
            HprofError::Io(format!(
                "无法创建过程索引文件目录 {}：{}",
                sidecar_dir.display(),
                error
            ))
        })?;
        let tmp_dir =
            sidecar_dir.join(format!(".building-{}-{}", std::process::id(), now_millis()));
        std::fs::create_dir_all(&tmp_dir).map_err(|error| {
            HprofError::Io(format!(
                "无法创建过程索引临时目录 {}：{}",
                tmp_dir.display(),
                error
            ))
        })?;

        let write_result = (|| {
            let identity = source_identity(&result.file_path)?;
            let manifest = HprofSidecarManifest {
                schema_version: HPROF_SIDECAR_SCHEMA_VERSION,
                mat_semantics_version: HPROF_MAT_SEMANTICS_VERSION,
                logclinic_version: env!("CARGO_PKG_VERSION").to_string(),
                state: "complete".to_string(),
                source_file_name: identity.file_name,
                source_len: identity.len,
                source_modified_millis: identity.modified_millis,
                head_crc32: identity.head_crc32,
                tail_crc32: identity.tail_crc32,
                header: result.header.clone(),
                size_model: result.size_model.clone(),
                created_millis: now_millis(),
            };
            progress.stage = HprofAnalysisStage::WritingIndex;
            progress.message = "正在写入 HPROF sidecar index".to_string();

            report_cache_write(
                progress,
                progress_reporter,
                0,
                1,
                "文件",
                "准备写入对象摘要",
            )?;
            write_objects_file(
                &tmp_dir.join(OBJECTS_FILE),
                result,
                progress,
                progress_reporter,
                cancel_flag,
            )?;
            check_cancel(cancel_flag)?;
            report_cache_write(
                progress,
                progress_reporter,
                0,
                1,
                "文件",
                "准备写入 dominator 索引",
            )?;
            write_dominator_file(
                &tmp_dir.join(DOMINATOR_FILE),
                result,
                progress,
                progress_reporter,
                cancel_flag,
            )?;
            check_cancel(cancel_flag)?;
            report_cache_write(
                progress,
                progress_reporter,
                0,
                1,
                "文件",
                "准备写入类名索引",
            )?;
            write_classes_file(
                &tmp_dir.join(CLASSES_FILE),
                result,
                progress,
                progress_reporter,
                cancel_flag,
            )?;
            check_cancel(cancel_flag)?;
            report_cache_write(
                progress,
                progress_reporter,
                0,
                1,
                "文件",
                "准备写入线程详情索引",
            )?;
            write_threads_file(
                &tmp_dir.join(THREADS_FILE),
                result,
                progress,
                progress_reporter,
                cancel_flag,
            )?;
            check_cancel(cancel_flag)?;
            report_cache_write(
                progress,
                progress_reporter,
                0,
                4,
                "文件",
                "写入占位过程文件",
            )?;
            write_empty_file(&tmp_dir.join(STRINGS_FILE))?;
            report_cache_write(
                progress,
                progress_reporter,
                1,
                4,
                "文件",
                "写入 strings.bin",
            )?;
            write_empty_file(&tmp_dir.join(EDGES_OFFSETS_FILE))?;
            report_cache_write(
                progress,
                progress_reporter,
                2,
                4,
                "文件",
                "写入 edges_offsets.bin",
            )?;
            write_empty_file(&tmp_dir.join(EDGES_TARGETS_FILE))?;
            report_cache_write(
                progress,
                progress_reporter,
                3,
                4,
                "文件",
                "写入 edges_targets.bin",
            )?;
            write_empty_file(&tmp_dir.join(ROOT_INDICES_FILE))?;
            check_cancel(cancel_flag)?;
            report_cache_write(
                progress,
                progress_reporter,
                4,
                4,
                "文件",
                "写入 root_indices.bin",
            )?;
            report_cache_write(progress, progress_reporter, 0, 1, "文件", "写入完成标记")?;
            write_manifest(&tmp_dir.join(MANIFEST_FILE), &manifest)?;
            publish_tmp_dir(&sidecar_dir, &tmp_dir)
        })();

        write_result?;
        Ok(())
    }

    fn report_cache_write<F>(
        progress: &mut HprofProgress,
        progress_reporter: &mut F,
        done: u64,
        total: u64,
        unit: &'static str,
        sub_message: &str,
    ) -> Result<(), HprofError>
    where
        F: FnMut(HprofProgress),
    {
        progress.phase_done = done;
        progress.phase_total = total;
        progress.phase_unit = unit;
        progress.sub_message = sub_message.to_string();
        progress_reporter(progress.clone());
        Ok(())
    }

    fn sidecar_dir_for_path(path: &Path) -> PathBuf {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.to_string())
            .unwrap_or_else(|| "dump".to_string());
        let sidecar_name = format!("{file_name}.logclinic-hprof");
        path.parent()
            .map(|parent| parent.join(&sidecar_name))
            .unwrap_or_else(|| PathBuf::from(sidecar_name))
    }

    fn read_manifest(path: &Path) -> Result<HprofSidecarManifest, HprofError> {
        let bytes = std::fs::read(path).map_err(|error| {
            HprofError::Io(format!(
                "读取 sidecar manifest {} 失败：{}",
                path.display(),
                error
            ))
        })?;
        serde_json::from_slice(&bytes).map_err(|error| {
            HprofError::InvalidFormat(format!(
                "sidecar manifest {} 格式损坏：{}",
                path.display(),
                error
            ))
        })
    }

    fn write_manifest(path: &Path, manifest: &HprofSidecarManifest) -> Result<(), HprofError> {
        let bytes = serde_json::to_vec_pretty(manifest).map_err(|error| {
            HprofError::InvalidFormat(format!("生成 sidecar manifest 失败：{error}"))
        })?;
        std::fs::write(path, bytes).map_err(|error| {
            HprofError::Io(format!(
                "写入 sidecar manifest {} 失败：{}",
                path.display(),
                error
            ))
        })
    }

    fn validate_manifest(
        path: &Path,
        manifest: &HprofSidecarManifest,
        identity: &SourceIdentity,
    ) -> Option<String> {
        if manifest.state != "complete" {
            return Some("索引未完成".to_string());
        }
        if manifest.schema_version != HPROF_SIDECAR_SCHEMA_VERSION {
            return Some("schema version 变化".to_string());
        }
        if manifest.mat_semantics_version != HPROF_MAT_SEMANTICS_VERSION {
            return Some("MAT 兼容语义版本变化".to_string());
        }
        if manifest.source_file_name
            != path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
        {
            return Some("源文件名变化".to_string());
        }
        if manifest.source_len != identity.len {
            return Some("源文件大小变化".to_string());
        }
        if manifest.source_modified_millis != identity.modified_millis {
            return Some("源文件修改时间变化".to_string());
        }
        if manifest.head_crc32 != identity.head_crc32 || manifest.tail_crc32 != identity.tail_crc32
        {
            return Some("源文件首尾校验不一致".to_string());
        }
        None
    }

    fn ensure_required_files(sidecar_dir: &Path) -> Result<(), HprofError> {
        // `objects.bin` 可能是最大的 sidecar 文件，缓存命中时改为可见行懒读取，不计入启动阶段总字节数。
        for file_name in [
            DOMINATOR_FILE,
            CLASSES_FILE,
            THREADS_FILE,
            STRINGS_FILE,
            EDGES_OFFSETS_FILE,
            EDGES_TARGETS_FILE,
            ROOT_INDICES_FILE,
        ] {
            let path = sidecar_dir.join(file_name);
            if !path.is_file() {
                return Err(HprofError::InvalidFormat(format!(
                    "sidecar index 缺少文件：{}",
                    path.display()
                )));
            }
        }
        Ok(())
    }

    /// 统计本次缓存命中需要读取的 sidecar 主体字节数。
    ///
    /// 业务意图：
    /// - 缓存加载阶段不再读取源 dump；进度条应反映 sidecar index 的读取进度，否则用户会看到 0 / dump 大小长时间不变。
    fn sidecar_total_bytes(sidecar_dir: &Path) -> Result<u64, HprofError> {
        let mut total = 0u64;
        for file_name in [
            OBJECTS_FILE,
            DOMINATOR_FILE,
            CLASSES_FILE,
            THREADS_FILE,
            STRINGS_FILE,
            EDGES_OFFSETS_FILE,
            EDGES_TARGETS_FILE,
            ROOT_INDICES_FILE,
        ] {
            let path = sidecar_dir.join(file_name);
            let len = std::fs::metadata(&path)
                .map_err(|error| {
                    HprofError::Io(format!(
                        "读取 sidecar 文件大小 {} 失败：{}",
                        path.display(),
                        error
                    ))
                })?
                .len();
            total = total.saturating_add(len);
        }
        Ok(total.max(1))
    }

    fn publish_tmp_dir(sidecar_dir: &Path, tmp_dir: &Path) -> Result<(), HprofError> {
        for file_name in [
            MANIFEST_FILE,
            OBJECTS_FILE,
            DOMINATOR_FILE,
            CLASSES_FILE,
            THREADS_FILE,
            STRINGS_FILE,
            EDGES_OFFSETS_FILE,
            EDGES_TARGETS_FILE,
            ROOT_INDICES_FILE,
        ] {
            let final_path = sidecar_dir.join(file_name);
            if final_path.exists() {
                std::fs::remove_file(&final_path).map_err(|error| {
                    HprofError::Io(format!(
                        "清理旧 sidecar 文件 {} 失败：{}",
                        final_path.display(),
                        error
                    ))
                })?;
            }
        }
        for file_name in [
            OBJECTS_FILE,
            DOMINATOR_FILE,
            CLASSES_FILE,
            THREADS_FILE,
            STRINGS_FILE,
            EDGES_OFFSETS_FILE,
            EDGES_TARGETS_FILE,
            ROOT_INDICES_FILE,
            MANIFEST_FILE,
        ] {
            std::fs::rename(tmp_dir.join(file_name), sidecar_dir.join(file_name)).map_err(
                |error| HprofError::Io(format!("发布 sidecar 文件 {} 失败：{}", file_name, error)),
            )?;
        }
        std::fs::remove_dir(tmp_dir).map_err(|error| {
            HprofError::Io(format!(
                "清理 sidecar 临时目录 {} 失败：{}",
                tmp_dir.display(),
                error
            ))
        })
    }

    fn source_identity(path: &Path) -> Result<SourceIdentity, HprofError> {
        let metadata = std::fs::metadata(path).map_err(|error| {
            HprofError::Io(format!(
                "读取源 dump 元数据 {} 失败：{}",
                path.display(),
                error
            ))
        })?;
        let modified_millis = metadata
            .modified()
            .ok()
            .and_then(system_time_millis)
            .unwrap_or(0);
        let (head_crc32, tail_crc32) = file_edge_crc32(path, metadata.len())?;
        Ok(SourceIdentity {
            file_name: path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.to_string())
                .unwrap_or_default(),
            len: metadata.len(),
            modified_millis,
            head_crc32,
            tail_crc32,
        })
    }

    fn file_edge_crc32(path: &Path, len: u64) -> Result<(u32, u32), HprofError> {
        let mut file = File::open(path).map_err(|error| {
            HprofError::Io(format!("打开源 dump {} 失败：{}", path.display(), error))
        })?;
        let head_len = len.min(HPROF_SIDECAR_CHECKSUM_BYTES);
        let head_crc32 = crc32_for_reader_slice(&mut file, head_len as usize)?;
        let tail_len = len.min(HPROF_SIDECAR_CHECKSUM_BYTES);
        file.seek(SeekFrom::Start(len.saturating_sub(tail_len)))?;
        let tail_crc32 = crc32_for_reader_slice(&mut file, tail_len as usize)?;
        Ok((head_crc32, tail_crc32))
    }

    fn crc32_for_reader_slice(file: &mut File, len: usize) -> Result<u32, HprofError> {
        let mut hasher = Crc32Hasher::new();
        let mut remaining = len;
        let mut buffer = vec![0u8; 64 * 1024];
        while remaining > 0 {
            let read_len = remaining.min(buffer.len());
            let n = file.read(&mut buffer[..read_len])?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
            remaining -= n;
        }
        Ok(hasher.finalize())
    }

    /// 创建带大缓冲的 sidecar 文件写入器。
    ///
    /// 业务意图：
    /// - sidecar 主体文件属于顺序写入场景，使用 32MiB 缓冲可减少大 dump 末尾写索引阶段的小系统调用。
    fn create_sidecar_writer(path: &Path) -> Result<BufWriter<File>, HprofError> {
        let file = File::create(path).map_err(|error| {
            HprofError::Io(format!(
                "创建 sidecar 文件 {} 失败：{}",
                path.display(),
                error
            ))
        })?;
        Ok(BufWriter::with_capacity(
            HPROF_SIDECAR_WRITER_BUFFER_BYTES,
            file,
        ))
    }

    /// 创建带大缓冲的 sidecar 文件读取器。
    ///
    /// 业务意图：
    /// - 缓存命中路径会按字段顺序读取数百万条对象摘要；如果直接对 `File` 做小块 `read_exact`，第二次打开仍会很慢。
    ///   使用大缓冲读取可以把大量小读合并成顺序读，明显降低系统调用开销。
    fn create_sidecar_reader(path: &Path) -> Result<(BufReader<File>, u64), HprofError> {
        let file = File::open(path).map_err(|error| {
            HprofError::Io(format!(
                "打开 sidecar 文件 {} 失败：{}",
                path.display(),
                error
            ))
        })?;
        let file_len = file.metadata().map_err(|error| {
            HprofError::Io(format!(
                "读取 sidecar 文件元数据 {} 失败：{}",
                path.display(),
                error
            ))
        })?;
        Ok((
            BufReader::with_capacity(HPROF_SIDECAR_WRITER_BUFFER_BYTES, file),
            file_len.len(),
        ))
    }

    /// 刷新 sidecar 文件写入器。
    fn finish_sidecar_writer(mut writer: BufWriter<File>) -> Result<(), HprofError> {
        writer.flush()?;
        Ok(())
    }

    /// 写入已编码的 sidecar chunk。
    ///
    /// 边界条件：
    /// - chunk 复用同一个 `Vec<u8>`，写完后清空但保留容量，避免数百万对象时反复分配。
    fn flush_sidecar_chunk<W: Write>(
        writer: &mut W,
        chunk: &mut Vec<u8>,
    ) -> Result<(), HprofError> {
        if !chunk.is_empty() {
            writer.write_all(chunk)?;
            chunk.clear();
        }
        Ok(())
    }

    /// 报告 sidecar 文件内 chunk 处理进度。
    ///
    /// 业务意图：
    /// - sidecar 首次生成和二次打开缓存命中都会按 chunk 处理同一批文件；进度文案必须区分“写入”和“读取”，
    ///   否则用户在加载缓存时会看到“写入 dominator”这种误导性状态。
    fn report_sidecar_chunk<F>(
        progress: &mut HprofProgress,
        progress_reporter: &mut F,
        action: &str,
        file_name: &str,
        done: usize,
        total: usize,
        unit: &'static str,
    ) -> Result<(), HprofError>
    where
        F: FnMut(HprofProgress),
    {
        progress.phase_done = done as u64;
        progress.phase_total = total as u64;
        progress.phase_unit = unit;
        progress.sub_message = format!("{action} {file_name} {done} / {total}");
        progress_reporter(progress.clone());
        Ok(())
    }

    /// 报告 sidecar 文件内 chunk 写入进度。
    fn report_cache_write_chunk<F>(
        progress: &mut HprofProgress,
        progress_reporter: &mut F,
        file_name: &str,
        done: usize,
        total: usize,
        unit: &'static str,
    ) -> Result<(), HprofError>
    where
        F: FnMut(HprofProgress),
    {
        report_sidecar_chunk(
            progress,
            progress_reporter,
            "写入",
            file_name,
            done,
            total,
            unit,
        )
    }

    /// 报告 sidecar 文件内 chunk 读取进度。
    fn report_cache_read_chunk<F>(
        progress: &mut HprofProgress,
        progress_reporter: &mut F,
        file_name: &str,
        done: usize,
        total: usize,
        unit: &'static str,
    ) -> Result<(), HprofError>
    where
        F: FnMut(HprofProgress),
    {
        report_sidecar_chunk(
            progress,
            progress_reporter,
            "读取",
            file_name,
            done,
            total,
            unit,
        )
    }

    /// 校验 sidecar 里的长度字段不会触发异常大分配。
    ///
    /// 边界条件：
    /// - sidecar 是过程文件，可能因为崩溃、手工修改或旧版本残留而损坏；任何来自磁盘的 count
    ///   都必须先和 manifest / 文件大小上限比对，再用于 `Vec::with_capacity`。
    fn validate_sidecar_count(
        label: &str,
        count: usize,
        max_count: usize,
    ) -> Result<(), HprofError> {
        if count > max_count {
            return Err(HprofError::InvalidFormat(format!(
                "sidecar {label} 数量异常：{count}，上限 {max_count}"
            )));
        }
        Ok(())
    }

    /// 校验 sidecar 里的长度字段必须等于预期值。
    fn validate_sidecar_count_eq(
        label: &str,
        count: usize,
        expected: usize,
    ) -> Result<(), HprofError> {
        if count != expected {
            return Err(HprofError::InvalidFormat(format!(
                "sidecar {label} 数量不匹配：{count}，预期 {expected}"
            )));
        }
        Ok(())
    }

    /// 按文件大小估算最多能容纳多少条 8 字节长度记录。
    fn max_u64_records_for_file(file_len: u64) -> usize {
        usize::try_from(file_len / 8).unwrap_or(usize::MAX)
    }

    /// 把“读取完一个 sidecar 文件”计入字节进度。
    fn finish_cache_file_progress<F>(
        progress: &mut HprofProgress,
        progress_reporter: &mut F,
        file_name: &str,
        file_len: u64,
    ) -> Result<(), HprofError>
    where
        F: FnMut(HprofProgress),
    {
        progress.bytes_read = progress.bytes_read.saturating_add(file_len);
        progress.phase_done = progress.phase_total;
        progress.sub_message = format!("读取 {file_name} 完成");
        progress_reporter(progress.clone());
        Ok(())
    }

    fn write_objects_file<F>(
        path: &Path,
        result: &HprofDominatorResult,
        progress: &mut HprofProgress,
        progress_reporter: &mut F,
        cancel_flag: &AtomicBool,
    ) -> Result<(), HprofError>
    where
        F: FnMut(HprofProgress),
    {
        let summary_count = result.summary_count();
        let mut writer = create_sidecar_writer(path)?;
        write_magic(&mut writer, MAGIC_OBJECTS)?;
        write_len(&mut writer, summary_count)?;
        report_cache_write_chunk(
            progress,
            progress_reporter,
            OBJECTS_FILE,
            0,
            summary_count,
            "对象",
        )?;
        let mut chunk = Vec::with_capacity(HPROF_SIDECAR_RECORD_CHUNK * 48);
        for chunk_start in (0..summary_count).step_by(HPROF_SIDECAR_RECORD_CHUNK) {
            check_cancel(cancel_flag)?;
            let chunk_end = (chunk_start + HPROF_SIDECAR_RECORD_CHUNK).min(summary_count);
            for summary_index in chunk_start..chunk_end {
                let Some(summary) = result.summary_at_index(summary_index) else {
                    return Err(HprofError::InvalidFormat(format!(
                        "写入 sidecar 时缺少对象摘要：{summary_index}"
                    )));
                };
                write_u64(&mut chunk, summary.object_id)?;
                write_u64(&mut chunk, summary.class_id)?;
                write_kind(&mut chunk, &summary.kind)?;
                write_u64(&mut chunk, summary.shallow_size)?;
                write_u64(&mut chunk, summary.retained_size)?;
                write_u32(&mut chunk, summary.retained_percent.to_bits())?;
                write_len(&mut chunk, summary.direct_child_count)?;
            }
            flush_sidecar_chunk(&mut writer, &mut chunk)?;
            report_cache_write_chunk(
                progress,
                progress_reporter,
                OBJECTS_FILE,
                chunk_end,
                summary_count,
                "对象",
            )?;
        }
        finish_sidecar_writer(writer)
    }

    /// 打开对象摘要懒加载存储。
    fn open_object_summary_storage<F>(
        path: &Path,
        expected_count: usize,
        progress: &mut HprofProgress,
        progress_reporter: &mut F,
    ) -> Result<HprofSummaryStorage, HprofError>
    where
        F: FnMut(HprofProgress),
    {
        let (mut file, file_len) = create_sidecar_reader(path)?;
        read_magic(&mut file, MAGIC_OBJECTS)?;
        let count = read_len(&mut file)?;
        validate_sidecar_count_eq("对象摘要", count, expected_count)?;
        validate_object_summary_file_len(count, file_len)?;
        progress.phase_done = 1;
        progress.phase_total = 1;
        progress.phase_unit = "文件";
        progress.sub_message = "建立对象摘要懒加载读取器".to_string();
        progress_reporter(progress.clone());
        Ok(HprofSummaryStorage::Sidecar {
            count,
            reader: std::cell::RefCell::new(file),
            cache: std::cell::RefCell::new(FxHashMap::default()),
            #[cfg(test)]
            object_index_cache: std::cell::RefCell::new(FxHashMap::default()),
        })
    }

    /// 从已打开的 `objects.bin` 随机读取单条对象摘要。
    ///
    /// 业务意图：
    /// - 缓存命中时 dominator tree 首屏只需要少量 top row；按固定记录长度 seek 可以避免全量读取数百万对象摘要。
    /// - 调用方复用同一个 `BufReader<File>`，避免 UI 重绘时为每个可见行重复打开 sidecar 文件。
    pub(super) fn read_object_summary_from_reader<R: Read + Seek>(
        reader: &mut R,
        summary_index: usize,
        expected_count: usize,
    ) -> Result<HprofDominatorObjectSummary, HprofError> {
        if summary_index >= expected_count {
            return Err(HprofError::InvalidFormat(format!(
                "sidecar 对象摘要下标越界：{summary_index} / {expected_count}"
            )));
        }
        let header_len = object_summary_data_offset();
        let offset = header_len
            .checked_add((summary_index as u64).saturating_mul(OBJECT_SUMMARY_RECORD_BYTES))
            .ok_or_else(|| HprofError::InvalidFormat("sidecar 对象摘要偏移溢出".to_string()))?;
        reader.seek(SeekFrom::Start(offset))?;
        read_object_summary_record(reader)
    }

    /// 校验 `objects.bin` 是否足以容纳固定长度摘要记录。
    fn validate_object_summary_file_len(count: usize, file_len: u64) -> Result<(), HprofError> {
        let expected_len = object_summary_data_offset()
            .checked_add((count as u64).saturating_mul(OBJECT_SUMMARY_RECORD_BYTES))
            .ok_or_else(|| HprofError::InvalidFormat("sidecar 对象摘要文件长度溢出".to_string()))?;
        if file_len < expected_len {
            return Err(HprofError::InvalidFormat(format!(
                "sidecar 对象摘要文件截断：{} < {}",
                file_len, expected_len
            )));
        }
        Ok(())
    }

    /// 返回 `objects.bin` 固定摘要记录起始偏移。
    fn object_summary_data_offset() -> u64 {
        8 + MAGIC_OBJECTS.len() as u64 + 8
    }

    /// 读取一条固定长度对象摘要记录。
    fn read_object_summary_record<R: Read>(
        reader: &mut R,
    ) -> Result<HprofDominatorObjectSummary, HprofError> {
        Ok(HprofDominatorObjectSummary {
            object_id: read_u64(reader)?,
            class_id: read_u64(reader)?,
            kind: read_kind(reader)?,
            shallow_size: read_u64(reader)?,
            retained_size: read_u64(reader)?,
            retained_percent: f32::from_bits(read_u32(reader)?),
            direct_child_count: read_len(reader)?,
        })
    }

    fn write_dominator_file<F>(
        path: &Path,
        result: &HprofDominatorResult,
        progress: &mut HprofProgress,
        progress_reporter: &mut F,
        cancel_flag: &AtomicBool,
    ) -> Result<(), HprofError>
    where
        F: FnMut(HprofProgress),
    {
        let mut writer = create_sidecar_writer(path)?;
        write_magic(&mut writer, MAGIC_DOMINATOR)?;
        write_len(&mut writer, result.total_objects)?;
        write_len(&mut writer, result.total_classes)?;
        write_len(&mut writer, result.gc_root_count)?;
        write_len(&mut writer, result.edge_count)?;
        write_len(&mut writer, result.raw_edge_count)?;
        write_len(&mut writer, result.reference_edge_stats.weak_like)?;
        write_len(&mut writer, result.reference_edge_stats.soft_like)?;
        write_len(&mut writer, result.reference_edge_stats.phantom_like)?;
        write_len(&mut writer, result.reference_edge_stats.finalizer_like)?;
        write_len(&mut writer, result.synthetic_class_loader_edge_count)?;
        write_len(&mut writer, result.synthetic_bootstrap_class_root_count)?;
        write_u64(&mut writer, result.total_shallow_size)?;
        write_u64(&mut writer, result.reachable_shallow_size)?;
        write_len(&mut writer, result.reachable_object_count)?;
        write_len(&mut writer, result.unreachable_object_count)?;
        write_u64(&mut writer, result.unreachable_shallow_size)?;
        write_len(&mut writer, result.top_object_ids.len())?;

        let total_records = 1usize
            .saturating_add(result.top_object_ids.len())
            .saturating_add(result.top_summary_indices.len())
            .saturating_add(result.child_offsets.len())
            .saturating_add(result.child_summary_indices.len());
        let mut done = 1usize;
        report_cache_write_chunk(
            progress,
            progress_reporter,
            DOMINATOR_FILE,
            done,
            total_records,
            "条",
        )?;

        let mut chunk = Vec::with_capacity(HPROF_SIDECAR_RECORD_CHUNK * 8);
        for object_ids in result.top_object_ids.chunks(HPROF_SIDECAR_RECORD_CHUNK) {
            check_cancel(cancel_flag)?;
            for object_id in object_ids {
                write_u64(&mut chunk, *object_id)?;
            }
            flush_sidecar_chunk(&mut writer, &mut chunk)?;
            done = done.saturating_add(object_ids.len());
            report_cache_write_chunk(
                progress,
                progress_reporter,
                DOMINATOR_FILE,
                done,
                total_records,
                "条",
            )?;
        }

        write_len(&mut writer, result.top_summary_indices.len())?;
        for indices in result
            .top_summary_indices
            .chunks(HPROF_SIDECAR_RECORD_CHUNK)
        {
            check_cancel(cancel_flag)?;
            for index in indices {
                write_len(&mut chunk, *index)?;
            }
            flush_sidecar_chunk(&mut writer, &mut chunk)?;
            done = done.saturating_add(indices.len());
            report_cache_write_chunk(
                progress,
                progress_reporter,
                DOMINATOR_FILE,
                done,
                total_records,
                "条",
            )?;
        }

        write_len(&mut writer, result.child_offsets.len())?;
        for offsets in result.child_offsets.chunks(HPROF_SIDECAR_RECORD_CHUNK) {
            check_cancel(cancel_flag)?;
            for offset in offsets {
                write_len(&mut chunk, *offset)?;
            }
            flush_sidecar_chunk(&mut writer, &mut chunk)?;
            done = done.saturating_add(offsets.len());
            report_cache_write_chunk(
                progress,
                progress_reporter,
                DOMINATOR_FILE,
                done,
                total_records,
                "条",
            )?;
        }

        write_len(&mut writer, result.child_summary_indices.len())?;
        for indices in result
            .child_summary_indices
            .chunks(HPROF_SIDECAR_RECORD_CHUNK)
        {
            check_cancel(cancel_flag)?;
            for index in indices {
                write_len(&mut chunk, *index)?;
            }
            flush_sidecar_chunk(&mut writer, &mut chunk)?;
            done = done.saturating_add(indices.len());
            report_cache_write_chunk(
                progress,
                progress_reporter,
                DOMINATOR_FILE,
                done,
                total_records,
                "条",
            )?;
        }
        finish_sidecar_writer(writer)
    }

    fn read_dominator_file<F>(
        path: &Path,
        progress: &mut HprofProgress,
        progress_reporter: &mut F,
        cancel_flag: &AtomicBool,
    ) -> Result<CachedDominatorData, HprofError>
    where
        F: FnMut(HprofProgress),
    {
        let (mut file, file_len) = create_sidecar_reader(path)?;
        progress.phase_done = 0;
        progress.phase_total = 1;
        progress.phase_unit = "文件";
        progress.sub_message = "读取 dominator 元数据".to_string();
        progress_reporter(progress.clone());
        read_magic(&mut file, MAGIC_DOMINATOR)?;
        let total_objects = read_len(&mut file)?;
        let total_classes = read_len(&mut file)?;
        let gc_root_count = read_len(&mut file)?;
        let edge_count = read_len(&mut file)?;
        let raw_edge_count = read_len(&mut file)?;
        let reference_edge_stats = HprofReferenceEdgeStats {
            weak_like: read_len(&mut file)?,
            soft_like: read_len(&mut file)?,
            phantom_like: read_len(&mut file)?,
            finalizer_like: read_len(&mut file)?,
        };
        let synthetic_class_loader_edge_count = read_len(&mut file)?;
        let synthetic_bootstrap_class_root_count = read_len(&mut file)?;
        let total_shallow_size = read_u64(&mut file)?;
        let reachable_shallow_size = read_u64(&mut file)?;
        let reachable_object_count = read_len(&mut file)?;
        let unreachable_object_count = read_len(&mut file)?;
        let unreachable_shallow_size = read_u64(&mut file)?;
        // 缓存命中时不会重新扫描源 HPROF，因此摘要计数必须尽早从 dominator 文件恢复。
        // 这样用户在大 sidecar 读取期间也能看到对象、类、Root、边数，而不是长时间保持 0。
        progress.object_count = total_objects;
        progress.class_count = total_classes;
        progress.gc_root_count = gc_root_count;
        progress.edge_count = edge_count;
        progress_reporter(progress.clone());
        validate_sidecar_count("可达对象", reachable_object_count, total_objects)?;
        validate_sidecar_count(
            "不可达对象",
            unreachable_object_count,
            total_objects.saturating_sub(reachable_object_count),
        )?;
        let max_records_by_file = max_u64_records_for_file(file_len);
        let top_count = read_len(&mut file)?;
        validate_sidecar_count(
            "Top retained",
            top_count,
            HPROF_TOP_DOMINATOR_LIMIT.min(max_records_by_file),
        )?;
        let mut top_object_ids = Vec::with_capacity(top_count);
        for index in 0..top_count {
            check_cancel(cancel_flag)?;
            top_object_ids.push(read_u64(&mut file)?);
            let done = index + 1;
            if should_report_work(done, top_count) {
                report_cache_read_chunk(
                    progress,
                    progress_reporter,
                    DOMINATOR_FILE,
                    done,
                    top_count,
                    "Top",
                )?;
            }
        }
        let top_summary_count = read_len(&mut file)?;
        validate_sidecar_count_eq("Top summary indices", top_summary_count, top_count)?;
        validate_sidecar_count(
            "Top summary indices",
            top_summary_count,
            reachable_object_count,
        )?;
        let mut top_summary_indices = Vec::with_capacity(top_summary_count);
        for index in 0..top_summary_count {
            check_cancel(cancel_flag)?;
            let summary_index = read_len(&mut file)?;
            validate_sidecar_count(
                "Top summary index",
                summary_index,
                reachable_object_count.saturating_sub(1),
            )?;
            top_summary_indices.push(summary_index);
            let done = index + 1;
            if should_report_work(done, top_summary_count) {
                report_cache_read_chunk(
                    progress,
                    progress_reporter,
                    DOMINATOR_FILE,
                    done,
                    top_summary_count,
                    "Top",
                )?;
            }
        }
        let offset_count = read_len(&mut file)?;
        let expected_offsets = reachable_object_count.checked_add(1).ok_or_else(|| {
            HprofError::InvalidFormat("sidecar child offsets 数量溢出".to_string())
        })?;
        validate_sidecar_count_eq("child offsets", offset_count, expected_offsets)?;
        validate_sidecar_count("child offsets", offset_count, max_records_by_file)?;
        let mut child_offsets = Vec::with_capacity(offset_count);
        for index in 0..offset_count {
            check_cancel(cancel_flag)?;
            child_offsets.push(read_len(&mut file)?);
            let done = index + 1;
            if should_report_work(done, offset_count) {
                report_cache_read_chunk(
                    progress,
                    progress_reporter,
                    DOMINATOR_FILE,
                    done,
                    offset_count,
                    "offset",
                )?;
            }
        }
        let child_count = read_len(&mut file)?;
        validate_sidecar_count("child summary indices", child_count, reachable_object_count)?;
        validate_sidecar_count("child summary indices", child_count, max_records_by_file)?;
        let mut previous_offset = 0usize;
        for offset in &child_offsets {
            if *offset < previous_offset || *offset > child_count {
                return Err(HprofError::InvalidFormat(format!(
                    "sidecar child offsets 内容损坏：offset={offset}, child_count={child_count}"
                )));
            }
            previous_offset = *offset;
        }
        let mut child_summary_indices = Vec::with_capacity(child_count);
        for index in 0..child_count {
            check_cancel(cancel_flag)?;
            let child_summary_index = read_len(&mut file)?;
            validate_sidecar_count(
                "child summary index",
                child_summary_index,
                reachable_object_count.saturating_sub(1),
            )?;
            child_summary_indices.push(child_summary_index);
            let done = index + 1;
            if should_report_work(done, child_count) {
                report_cache_read_chunk(
                    progress,
                    progress_reporter,
                    DOMINATOR_FILE,
                    done,
                    child_count,
                    "child",
                )?;
            }
        }
        finish_cache_file_progress(progress, progress_reporter, DOMINATOR_FILE, file_len)?;
        Ok(CachedDominatorData {
            total_objects,
            total_classes,
            gc_root_count,
            edge_count,
            raw_edge_count,
            reference_edge_stats,
            synthetic_class_loader_edge_count,
            synthetic_bootstrap_class_root_count,
            total_shallow_size,
            reachable_shallow_size,
            reachable_object_count,
            unreachable_object_count,
            unreachable_shallow_size,
            top_object_ids,
            top_summary_indices,
            child_offsets,
            child_summary_indices,
        })
    }

    struct CachedDominatorData {
        total_objects: usize,
        total_classes: usize,
        gc_root_count: usize,
        edge_count: usize,
        raw_edge_count: usize,
        reference_edge_stats: HprofReferenceEdgeStats,
        synthetic_class_loader_edge_count: usize,
        synthetic_bootstrap_class_root_count: usize,
        total_shallow_size: u64,
        reachable_shallow_size: u64,
        reachable_object_count: usize,
        unreachable_object_count: usize,
        unreachable_shallow_size: u64,
        top_object_ids: Vec<HprofObjectId>,
        top_summary_indices: Vec<usize>,
        child_offsets: Vec<usize>,
        child_summary_indices: Vec<usize>,
    }

    fn write_classes_file<F>(
        path: &Path,
        result: &HprofDominatorResult,
        progress: &mut HprofProgress,
        progress_reporter: &mut F,
        cancel_flag: &AtomicBool,
    ) -> Result<(), HprofError>
    where
        F: FnMut(HprofProgress),
    {
        let mut writer = create_sidecar_writer(path)?;
        write_magic(&mut writer, MAGIC_CLASSES)?;
        write_len(&mut writer, result.class_names.len())?;
        report_cache_write_chunk(
            progress,
            progress_reporter,
            CLASSES_FILE,
            0,
            result.class_names.len(),
            "类",
        )?;

        let mut chunk = Vec::with_capacity(HPROF_SIDECAR_RECORD_CHUNK * 64);
        let mut done = 0usize;
        for (class_id, name) in &result.class_names {
            write_u64(&mut chunk, *class_id)?;
            write_string(&mut chunk, name)?;
            done = done.saturating_add(1);
            if done.is_multiple_of(HPROF_SIDECAR_RECORD_CHUNK) {
                check_cancel(cancel_flag)?;
                flush_sidecar_chunk(&mut writer, &mut chunk)?;
                report_cache_write_chunk(
                    progress,
                    progress_reporter,
                    CLASSES_FILE,
                    done,
                    result.class_names.len(),
                    "类",
                )?;
            }
        }
        flush_sidecar_chunk(&mut writer, &mut chunk)?;
        report_cache_write_chunk(
            progress,
            progress_reporter,
            CLASSES_FILE,
            done,
            result.class_names.len(),
            "类",
        )?;
        finish_sidecar_writer(writer)
    }

    fn read_classes_file<F>(
        path: &Path,
        max_class_count: usize,
        progress: &mut HprofProgress,
        progress_reporter: &mut F,
        cancel_flag: &AtomicBool,
    ) -> Result<FxHashMap<HprofObjectId, String>, HprofError>
    where
        F: FnMut(HprofProgress),
    {
        let (mut file, file_len) = create_sidecar_reader(path)?;
        read_magic(&mut file, MAGIC_CLASSES)?;
        let count = read_len(&mut file)?;
        validate_sidecar_count("类名", count, max_class_count)?;
        validate_sidecar_count(
            "类名",
            count,
            usize::try_from(file_len / 16).unwrap_or(usize::MAX),
        )?;
        progress.phase_done = 0;
        progress.phase_total = count as u64;
        progress.phase_unit = "类";
        progress.sub_message = "读取类名索引".to_string();
        progress_reporter(progress.clone());
        let mut class_names = FxHashMap::default();
        class_names.reserve(count);
        for index in 0..count {
            check_cancel(cancel_flag)?;
            class_names.insert(read_u64(&mut file)?, read_string(&mut file)?);
            let done = index + 1;
            if should_report_work(done, count) {
                report_cache_read_chunk(
                    progress,
                    progress_reporter,
                    CLASSES_FILE,
                    done,
                    count,
                    "类",
                )?;
            }
        }
        finish_cache_file_progress(progress, progress_reporter, CLASSES_FILE, file_len)?;
        Ok(class_names)
    }

    fn write_threads_file<F>(
        path: &Path,
        result: &HprofDominatorResult,
        progress: &mut HprofProgress,
        progress_reporter: &mut F,
        cancel_flag: &AtomicBool,
    ) -> Result<(), HprofError>
    where
        F: FnMut(HprofProgress),
    {
        let mut writer = create_sidecar_writer(path)?;
        write_magic(&mut writer, MAGIC_THREADS)?;
        write_len(&mut writer, result.thread_details.len())?;
        report_cache_write_chunk(
            progress,
            progress_reporter,
            THREADS_FILE,
            0,
            result.thread_details.len(),
            "线程",
        )?;

        let mut chunk = Vec::with_capacity(HPROF_SIDECAR_RECORD_CHUNK * 256);
        let mut done = 0usize;
        for (object_id, details) in &result.thread_details {
            write_u64(&mut chunk, *object_id)?;
            write_u64(&mut chunk, details.object_id)?;
            write_string(&mut chunk, &details.class_name)?;
            write_string(&mut chunk, &details.thread_name)?;
            write_len(&mut chunk, details.properties.len())?;
            for property in &details.properties {
                write_string(&mut chunk, &property.name)?;
                write_string(&mut chunk, &property.value)?;
            }
            write_len(&mut chunk, details.stack_frames.len())?;
            for frame in &details.stack_frames {
                write_string(&mut chunk, &frame.class_name)?;
                write_string(&mut chunk, &frame.method_name)?;
                write_string(&mut chunk, &frame.method_signature)?;
                write_string(&mut chunk, &frame.source)?;
                write_i32(&mut chunk, frame.line_number)?;
                write_string(&mut chunk, &frame.display)?;
            }
            write_option_string(&mut chunk, details.stack_message.as_deref())?;
            done = done.saturating_add(1);
            if done.is_multiple_of(HPROF_SIDECAR_RECORD_CHUNK)
                || chunk.len() >= HPROF_SIDECAR_WRITER_BUFFER_BYTES
            {
                check_cancel(cancel_flag)?;
                flush_sidecar_chunk(&mut writer, &mut chunk)?;
                report_cache_write_chunk(
                    progress,
                    progress_reporter,
                    THREADS_FILE,
                    done,
                    result.thread_details.len(),
                    "线程",
                )?;
            }
        }
        flush_sidecar_chunk(&mut writer, &mut chunk)?;
        report_cache_write_chunk(
            progress,
            progress_reporter,
            THREADS_FILE,
            done,
            result.thread_details.len(),
            "线程",
        )?;
        finish_sidecar_writer(writer)
    }

    fn read_threads_file<F>(
        path: &Path,
        max_thread_count: usize,
        progress: &mut HprofProgress,
        progress_reporter: &mut F,
        cancel_flag: &AtomicBool,
    ) -> Result<FxHashMap<HprofObjectId, HprofThreadDetails>, HprofError>
    where
        F: FnMut(HprofProgress),
    {
        let (mut file, file_len) = create_sidecar_reader(path)?;
        read_magic(&mut file, MAGIC_THREADS)?;
        let count = read_len(&mut file)?;
        validate_sidecar_count("线程详情", count, max_thread_count)?;
        validate_sidecar_count(
            "线程详情",
            count,
            usize::try_from(file_len / 32).unwrap_or(usize::MAX),
        )?;
        progress.phase_done = 0;
        progress.phase_total = count as u64;
        progress.phase_unit = "线程";
        progress.sub_message = "读取线程详情".to_string();
        progress_reporter(progress.clone());
        let mut thread_details = FxHashMap::default();
        thread_details.reserve(count);
        let max_records_by_file = max_u64_records_for_file(file_len);
        for index in 0..count {
            check_cancel(cancel_flag)?;
            let key = read_u64(&mut file)?;
            let object_id = read_u64(&mut file)?;
            let class_name = read_string(&mut file)?;
            let thread_name = read_string(&mut file)?;
            let property_count = read_len(&mut file)?;
            validate_sidecar_count("线程属性", property_count, 128)?;
            let mut properties = Vec::with_capacity(property_count);
            for _ in 0..property_count {
                properties.push(HprofThreadProperty {
                    name: read_string(&mut file)?,
                    value: read_string(&mut file)?,
                });
            }
            let frame_count = read_len(&mut file)?;
            validate_sidecar_count("线程栈帧", frame_count, max_records_by_file)?;
            let mut stack_frames = Vec::with_capacity(frame_count);
            for _ in 0..frame_count {
                stack_frames.push(HprofThreadStackFrame {
                    class_name: read_string(&mut file)?,
                    method_name: read_string(&mut file)?,
                    method_signature: read_string(&mut file)?,
                    source: read_string(&mut file)?,
                    line_number: read_i32(&mut file)?,
                    display: read_string(&mut file)?,
                });
            }
            let stack_message = read_option_string(&mut file)?;
            thread_details.insert(
                key,
                HprofThreadDetails {
                    object_id,
                    class_name,
                    thread_name,
                    properties,
                    stack_frames,
                    stack_message,
                },
            );
            let done = index + 1;
            if should_report_work(done, count) {
                report_cache_read_chunk(
                    progress,
                    progress_reporter,
                    THREADS_FILE,
                    done,
                    count,
                    "线程",
                )?;
            }
        }
        finish_cache_file_progress(progress, progress_reporter, THREADS_FILE, file_len)?;
        Ok(thread_details)
    }

    fn read_cached_result<F>(
        path: &Path,
        sidecar_dir: &Path,
        manifest: &HprofSidecarManifest,
        progress: &mut HprofProgress,
        progress_reporter: &mut F,
        cancel_flag: &AtomicBool,
    ) -> Result<HprofDominatorResult, HprofError>
    where
        F: FnMut(HprofProgress),
    {
        let dominator_path = sidecar_dir.join(DOMINATOR_FILE);
        let dominator =
            read_dominator_file(&dominator_path, progress, progress_reporter, cancel_flag)?;
        let objects_path = sidecar_dir.join(OBJECTS_FILE);
        let summary_storage = open_object_summary_storage(
            &objects_path,
            dominator.reachable_object_count,
            progress,
            progress_reporter,
        )?;
        check_cancel(cancel_flag)?;
        let classes_path = sidecar_dir.join(CLASSES_FILE);
        let class_names = read_classes_file(
            &classes_path,
            dominator.total_classes,
            progress,
            progress_reporter,
            cancel_flag,
        )?;
        let threads_path = sidecar_dir.join(THREADS_FILE);
        let thread_details = read_threads_file(
            &threads_path,
            dominator.reachable_object_count,
            progress,
            progress_reporter,
            cancel_flag,
        )?;
        Ok(HprofDominatorResult {
            file_path: path.to_path_buf(),
            header: manifest.header.clone(),
            total_objects: dominator.total_objects,
            total_classes: dominator.total_classes,
            gc_root_count: dominator.gc_root_count,
            edge_count: dominator.edge_count,
            raw_edge_count: dominator.raw_edge_count,
            reference_edge_stats: dominator.reference_edge_stats,
            synthetic_class_loader_edge_count: dominator.synthetic_class_loader_edge_count,
            synthetic_bootstrap_class_root_count: dominator.synthetic_bootstrap_class_root_count,
            total_shallow_size: dominator.total_shallow_size,
            reachable_shallow_size: dominator.reachable_shallow_size,
            reachable_object_count: dominator.reachable_object_count,
            unreachable_object_count: dominator.unreachable_object_count,
            unreachable_shallow_size: dominator.unreachable_shallow_size,
            size_model: manifest.size_model.clone(),
            cache_status: None,
            top_object_ids: dominator.top_object_ids,
            top_summary_indices: dominator.top_summary_indices,
            summary_storage,
            class_names,
            thread_details,
            child_offsets: dominator.child_offsets,
            child_summary_indices: dominator.child_summary_indices,
        })
    }

    fn write_empty_file(path: &Path) -> Result<(), HprofError> {
        let mut writer = create_sidecar_writer(path)?;
        write_magic(&mut writer, MAGIC_EMPTY)?;
        write_u64(&mut writer, 0)?;
        finish_sidecar_writer(writer)
    }

    fn write_magic<W: Write>(writer: &mut W, magic: &[u8]) -> Result<(), HprofError> {
        write_len(writer, magic.len())?;
        writer.write_all(magic)?;
        Ok(())
    }

    fn read_magic<R: Read>(reader: &mut R, expected: &[u8]) -> Result<(), HprofError> {
        let len = read_len(reader)?;
        validate_sidecar_count("magic", len, MAX_MAGIC_BYTES)?;
        let mut magic = vec![0u8; len];
        reader.read_exact(&mut magic)?;
        if magic == expected {
            Ok(())
        } else {
            Err(HprofError::InvalidFormat(
                "sidecar 二进制文件 magic 不匹配".to_string(),
            ))
        }
    }

    fn write_kind<W: Write>(writer: &mut W, kind: &HprofObjectKind) -> Result<(), HprofError> {
        match kind {
            HprofObjectKind::Class => {
                write_u8(writer, 0)?;
                write_u32(writer, 0)?;
                write_u8(writer, 0)
            }
            HprofObjectKind::Instance => {
                write_u8(writer, 1)?;
                write_u32(writer, 0)?;
                write_u8(writer, 0)
            }
            HprofObjectKind::ObjectArray { length } => {
                write_u8(writer, 2)?;
                write_u32(writer, *length)?;
                write_u8(writer, 0)
            }
            HprofObjectKind::PrimitiveArray {
                element_type,
                length,
            } => {
                write_u8(writer, 3)?;
                write_u32(writer, *length)?;
                write_u8(writer, *element_type)
            }
        }
    }

    fn read_kind<R: Read>(reader: &mut R) -> Result<HprofObjectKind, HprofError> {
        let tag = read_u8(reader)?;
        let length = read_u32(reader)?;
        let element_type = read_u8(reader)?;
        match tag {
            0 => Ok(HprofObjectKind::Class),
            1 => Ok(HprofObjectKind::Instance),
            2 => Ok(HprofObjectKind::ObjectArray { length }),
            3 => Ok(HprofObjectKind::PrimitiveArray {
                element_type,
                length,
            }),
            tag => Err(HprofError::InvalidFormat(format!(
                "未知 sidecar 对象类型：{tag}"
            ))),
        }
    }

    fn write_string<W: Write>(writer: &mut W, value: &str) -> Result<(), HprofError> {
        write_len(writer, value.len())?;
        writer.write_all(value.as_bytes())?;
        Ok(())
    }

    fn read_string<R: Read>(reader: &mut R) -> Result<String, HprofError> {
        let len = read_len(reader)?;
        validate_sidecar_count("字符串字节", len, MAX_STRING_BYTES)?;
        let mut bytes = vec![0u8; len];
        reader.read_exact(&mut bytes)?;
        String::from_utf8(bytes)
            .map_err(|_| HprofError::InvalidFormat("sidecar 字符串不是有效 UTF-8".to_string()))
    }

    fn write_option_string<W: Write>(
        writer: &mut W,
        value: Option<&str>,
    ) -> Result<(), HprofError> {
        match value {
            Some(value) => {
                write_u8(writer, 1)?;
                write_string(writer, value)
            }
            None => write_u8(writer, 0),
        }
    }

    fn read_option_string<R: Read>(reader: &mut R) -> Result<Option<String>, HprofError> {
        match read_u8(reader)? {
            0 => Ok(None),
            1 => Ok(Some(read_string(reader)?)),
            tag => Err(HprofError::InvalidFormat(format!(
                "未知 sidecar Option<String> 标记：{tag}"
            ))),
        }
    }

    fn write_len<W: Write>(writer: &mut W, value: usize) -> Result<(), HprofError> {
        write_u64(writer, value as u64)
    }

    fn read_len<R: Read>(reader: &mut R) -> Result<usize, HprofError> {
        usize::try_from(read_u64(reader)?)
            .map_err(|_| HprofError::InvalidFormat("sidecar 长度超过当前平台限制".to_string()))
    }

    fn write_u8<W: Write>(writer: &mut W, value: u8) -> Result<(), HprofError> {
        writer.write_all(&[value])?;
        Ok(())
    }

    fn read_u8<R: Read>(reader: &mut R) -> Result<u8, HprofError> {
        let mut bytes = [0u8; 1];
        reader.read_exact(&mut bytes)?;
        Ok(bytes[0])
    }

    fn write_u32<W: Write>(writer: &mut W, value: u32) -> Result<(), HprofError> {
        writer.write_all(&value.to_le_bytes())?;
        Ok(())
    }

    fn read_u32<R: Read>(reader: &mut R) -> Result<u32, HprofError> {
        let mut bytes = [0u8; 4];
        reader.read_exact(&mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn write_i32<W: Write>(writer: &mut W, value: i32) -> Result<(), HprofError> {
        writer.write_all(&value.to_le_bytes())?;
        Ok(())
    }

    fn read_i32<R: Read>(reader: &mut R) -> Result<i32, HprofError> {
        let mut bytes = [0u8; 4];
        reader.read_exact(&mut bytes)?;
        Ok(i32::from_le_bytes(bytes))
    }

    fn write_u64<W: Write>(writer: &mut W, value: u64) -> Result<(), HprofError> {
        writer.write_all(&value.to_le_bytes())?;
        Ok(())
    }

    fn read_u64<R: Read>(reader: &mut R) -> Result<u64, HprofError> {
        let mut bytes = [0u8; 8];
        reader.read_exact(&mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn now_millis() -> u128 {
        system_time_millis(SystemTime::now()).unwrap_or(0)
    }

    fn system_time_millis(time: SystemTime) -> Option<u128> {
        time.duration_since(UNIX_EPOCH).ok().map(|duration| {
            u128::from(duration.as_secs()) * 1000 + u128::from(duration.subsec_millis())
        })
    }
}

/// 紧凑引用图，给 LT 算法使用。
///
/// 业务意图：
/// - dominator 计算只需要连续对象下标和出边目标；移除 petgraph 实体图可以避免为同一批节点和边额外复制一份内存。
struct HprofCompactAdjacency {
    /// 虚拟 Root 指向的 GC Root 对象下标。
    root_indices: Vec<HprofNodeId>,
    /// 每个对象出边在 `targets` 中的起始偏移。
    offsets: Vec<HprofNodeId>,
    /// 所有对象出边目标对象下标。
    targets: Vec<HprofNodeId>,
}

/// 连续对象下标上的紧凑 dominator 子节点表。
struct HprofCompactChildren {
    /// 每个对象直接支配子节点在 `indices` 中的起始偏移。
    offsets: Vec<usize>,
    /// 所有直接支配子节点对象下标。
    indices: Vec<usize>,
    /// 由虚拟 Root 直接支配的对象下标。
    root_children: Vec<usize>,
}

impl HprofCompactChildren {
    /// 返回指定对象下标的直接支配子节点切片。
    fn children(&self, object_index: usize) -> &[usize] {
        let start = self.offsets.get(object_index).copied().unwrap_or(0);
        let end = self.offsets.get(object_index + 1).copied().unwrap_or(start);
        &self.indices[start..end]
    }

    /// 返回指定对象下标的直接支配子节点数量。
    fn child_count(&self, object_index: usize) -> usize {
        self.children(object_index).len()
    }
}

/// LT 算法输出的对象下标级 dominator 结果。
struct HprofDominatorIndexResult {
    /// 对象是否可从虚拟 Root 到达。
    reachable: Vec<bool>,
    /// 对象的 immediate dominator；`None` 表示由虚拟 Root 直接支配或不可达。
    parent_by_index: Vec<Option<usize>>,
}

/// HPROF 解析错误。
///
/// 业务意图：
/// - UI 需要给用户中文可理解错误，不应直接暴露底层 `io::ErrorKind` 或二进制偏移的英文 panic。
#[derive(Debug)]
pub(crate) enum HprofError {
    /// 文件系统或读取错误。
    Io(String),
    /// 用户选择或输入不符合要求。
    InvalidInput(String),
    /// 文件内容不是合法 HPROF。
    InvalidFormat(String),
    /// 第一版尚未支持的 HPROF 记录。
    Unsupported(String),
    /// 用户主动取消。
    Canceled,
}

impl Display for HprofError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message)
            | Self::InvalidInput(message)
            | Self::InvalidFormat(message)
            | Self::Unsupported(message) => formatter.write_str(message),
            Self::Canceled => formatter.write_str("HPROF 解析已取消"),
        }
    }
}

impl Error for HprofError {}

impl From<io::Error> for HprofError {
    fn from(error: io::Error) -> Self {
        Self::Io(format!("读取 HPROF 文件失败：{error}"))
    }
}

/// 分析指定 HPROF 文件并返回 dominator tree。
///
/// 业务意图：
/// - 这是 UI 调用的唯一入口；路径校验、二进制解析、对象图构建和 dominator 计算都集中在后台任务中执行。
///
/// 边界条件：
/// - `progress_reporter` 可能在后台线程高频调用，调用方应只做轻量状态覆盖，不应直接触发重型 UI 操作。
/// - `cancel_flag` 在解析循环和自研 Lengauer-Tarjan 批量边界检查，保证大 dump 计算阶段也能尽快响应取消。
pub(crate) fn analyze_hprof_dominator_tree<F>(
    path: PathBuf,
    mut progress_reporter: F,
    cancel_flag: Arc<AtomicBool>,
) -> Result<HprofDominatorResult, HprofError>
where
    F: FnMut(HprofProgress),
{
    let mut progress = HprofProgress::new(file_size_for_progress(&path));
    progress_reporter(progress.clone());
    validate_hprof_file_selection(&path)?;
    check_cancel(&cancel_flag)?;

    if let Some(cached_result) = hprof_cache::try_load_cached_hprof_result(
        &path,
        &mut progress,
        &mut progress_reporter,
        &cancel_flag,
    ) {
        return cached_result;
    }

    let sidecar_writable = hprof_cache::ensure_sidecar_writable(&path, progress.total_bytes)?;

    progress.stage = HprofAnalysisStage::ReadingHeader;
    progress.message = "正在读取 HPROF 文件头".to_string();
    progress.sub_message.clear();
    progress_reporter(progress.clone());

    let graph = parse_hprof_object_graph(&path, progress, &mut progress_reporter, &cancel_flag)?;
    check_cancel(&cancel_flag)?;

    let mut progress = HprofProgress {
        stage: HprofAnalysisStage::BuildingDominatorGraph,
        message: "正在构建对象引用图".to_string(),
        total_bytes: file_size_for_progress(&path),
        bytes_read: file_size_for_progress(&path),
        record_count: 0,
        object_count: graph.object_count(),
        class_count: graph.class_count(),
        gc_root_count: graph.gc_roots.len(),
        edge_count: graph.edge_count(),
        phase_done: 0,
        phase_total: graph.object_count() as u64,
        phase_unit: "对象",
        sub_message: "准备构建紧凑图节点".to_string(),
    };
    progress_reporter(progress.clone());

    let mut result = build_hprof_dominator_result(
        &path,
        graph,
        &cancel_flag,
        |stage, message, sub_message, phase_done, phase_total, phase_unit| {
            progress.stage = stage;
            progress.message = message.to_string();
            progress.sub_message = sub_message.to_string();
            progress.phase_done = phase_done;
            progress.phase_total = phase_total;
            progress.phase_unit = phase_unit;
            progress_reporter(progress.clone());
        },
    )?;

    if sidecar_writable {
        hprof_cache::write_hprof_sidecar_result(
            &result,
            &mut progress,
            &mut progress_reporter,
            &cancel_flag,
        )?;
        result.cache_status = Some("sidecar index 已生成".to_string());
        // 写入 sidecar 后立即按缓存路径重新打开一次结果，释放首次解析阶段构造的全量对象摘要和对象 ID 查询结构。
        //
        // 业务意图：
        // - 首次解析大 dump 仍需要在 LT/result 阶段短暂构造 Owned 结果，但 UI 最终持有的结果应与第二次打开一致，
        //   只保留 dominator/top/children 等轻量索引，对象摘要按可见行从 sidecar 懒加载。
        // - 如果刚写出的 sidecar 因外部文件系统问题无法读回，仍返回内存结果，不影响本次分析正确性。
        if let Some(cached_result) = hprof_cache::try_load_cached_hprof_result(
            &path,
            &mut progress,
            &mut progress_reporter,
            &cancel_flag,
        ) {
            match cached_result {
                Ok(mut cached_result) => {
                    cached_result.cache_status =
                        Some("sidecar index 已生成并切换为懒加载".to_string());
                    result = cached_result;
                }
                Err(HprofError::Canceled) => return Err(HprofError::Canceled),
                Err(_) => {}
            }
        }
    }

    progress.stage = HprofAnalysisStage::Completed;
    progress.message = "HPROF dominator tree 计算完成".to_string();
    progress.sub_message.clear();
    progress.phase_done = 1;
    progress.phase_total = 1;
    progress.phase_unit = "阶段";
    progress.object_count = result.total_objects;
    progress.class_count = result.total_classes;
    progress.gc_root_count = result.gc_root_count;
    progress.edge_count = result.edge_count;
    progress_reporter(progress);
    Ok(result)
}

/// 校验用户选择的 HPROF 文件。
///
/// 业务意图：
/// - GPUI 0.2.2 的文件选择器不能过滤扩展名，因此选择后必须在业务层检查 `.hprof` 和 `.bin`。
/// - `.bin` 只作为改名的 HPROF 处理，仍需要 header 通过校验。
pub(crate) fn validate_hprof_file_selection(path: &Path) -> Result<(), HprofError> {
    if !hprof_path_has_supported_extension(path) {
        return Err(HprofError::InvalidInput(format!(
            "请选择 .hprof 或 .bin 文件：{}",
            path.display()
        )));
    }
    let mut file = File::open(path).map_err(|error| {
        HprofError::Io(format!("无法打开 HPROF 文件 {}：{}", path.display(), error))
    })?;
    let label = read_hprof_header_label(&mut file)?;
    validate_hprof_label(&label)
}

/// 判断路径后缀是否属于当前允许的 HPROF 输入。
pub(crate) fn hprof_path_has_supported_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            let extension = extension.to_ascii_lowercase();
            extension == "hprof" || extension == "bin"
        })
        .unwrap_or(false)
}

/// 返回用于进度展示的文件大小。
fn file_size_for_progress(path: &Path) -> u64 {
    fs_metadata_len(path).unwrap_or(0)
}

/// 读取文件大小。
///
/// 实现原因：
/// - 拆成独立函数便于在 `file_size_for_progress` 中吞掉错误，同时其它路径仍可以保留明确错误。
fn fs_metadata_len(path: &Path) -> io::Result<u64> {
    Ok(std::fs::metadata(path)?.len())
}

/// 读取并校验 HPROF header 标签。
fn read_hprof_header_label(reader: &mut File) -> Result<String, HprofError> {
    let mut label_bytes = Vec::new();
    for _ in 0..HPROF_HEADER_MAX_BYTES {
        let mut byte = [0u8; 1];
        if reader.read(&mut byte)? == 0 {
            return Err(HprofError::InvalidFormat(
                "HPROF 文件头不完整，未找到格式字符串结束符".to_string(),
            ));
        }
        if byte[0] == 0 {
            return String::from_utf8(label_bytes)
                .map_err(|_| HprofError::InvalidFormat("HPROF 文件头不是有效 UTF-8".to_string()));
        }
        label_bytes.push(byte[0]);
    }
    Err(HprofError::InvalidFormat(
        "HPROF 文件头过长，可能不是 JVM HPROF 文件".to_string(),
    ))
}

/// 校验 HPROF header 标签。
fn validate_hprof_label(label: &str) -> Result<(), HprofError> {
    if label.starts_with("JAVA PROFILE ") {
        Ok(())
    } else {
        Err(HprofError::InvalidFormat(format!(
            "文件头不是 JVM HPROF：{label}"
        )))
    }
}

/// 检查用户是否取消。
fn check_cancel(cancel_flag: &AtomicBool) -> Result<(), HprofError> {
    if cancel_flag.load(Ordering::Relaxed) {
        Err(HprofError::Canceled)
    } else {
        Ok(())
    }
}

/// 解析 HPROF 对象图。
fn parse_hprof_object_graph<F>(
    path: &Path,
    progress: HprofProgress,
    progress_reporter: &mut F,
    cancel_flag: &AtomicBool,
) -> Result<HprofObjectGraph, HprofError>
where
    F: FnMut(HprofProgress),
{
    let bytes = HprofInputBytes::open(path)?;
    let mut parser = HprofParser {
        bytes: bytes.as_slice(),
        position: 0,
        identifier_size: 0,
        progress,
        progress_reporter,
        cancel_flag,
        state: HprofParserState::default(),
    };
    parser.parse()
}

/// HPROF 输入字节来源。
///
/// 业务意图：
/// - 大 dump 优先使用 mmap 避免把文件内容复制到用户态缓冲区；mmap 不可用时回退 `Vec<u8>`，保证跨平台可恢复。
enum HprofInputBytes {
    /// 只读内存映射。
    Mapped(Mmap),
    /// 回退路径下的整文件字节。
    Owned(Vec<u8>),
}

impl HprofInputBytes {
    /// 打开 HPROF 文件并准备可随机访问字节切片。
    fn open(path: &Path) -> Result<Self, HprofError> {
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
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Mapped(mapped) => mapped,
            Self::Owned(bytes) => bytes,
        }
    }
}

/// 原始类信息。
///
/// 业务意图：
/// - HPROF 中实例字段会继承父类字段；解析 instance dump 前需要保留 class dump 的原始字段布局。
#[derive(Clone, Debug)]
struct RawClassInfo {
    /// 父类 ID。
    super_class_id: HprofObjectId,
    /// 类加载器对象 ID。
    ///
    /// 业务意图：
    /// - MAT 兼容强引用图仍需要把 class object 到 class loader 的关系建成强边，避免静态字段和类加载器 retained 结构断开。
    class_loader_id: HprofObjectId,
    /// 当前类声明的静态字段。
    static_fields: Vec<RawStaticFieldDescriptor>,
    /// 当前类声明的实例字段。
    instance_fields: Vec<RawFieldDescriptor>,
}

/// 原始字段描述。
#[derive(Clone, Debug)]
struct RawFieldDescriptor {
    /// 字段名字符串 ID。
    ///
    /// 实现原因：
    /// - `java.lang.ref.Reference.referent` 必须根据字段名和声明类识别；解析阶段先保留 ID，待字符串表完整后再解析成名称。
    name_id: HprofObjectId,
    /// HPROF 字段类型编码。
    field_type: u8,
}

/// 原始静态字段描述。
#[derive(Clone, Debug)]
struct RawStaticFieldDescriptor {
    /// 字段名字符串 ID。
    _name_id: HprofObjectId,
    /// HPROF 字段类型编码。
    field_type: u8,
    /// 对象静态字段的值；非对象字段为 `None`。
    reference_id: Option<HprofObjectId>,
}

/// START_THREAD 顶层记录。
///
/// 业务意图：
/// - 线程对象、线程序号、栈轨迹和线程名在 HPROF 中被拆成多张表；保留原始 ID 便于完成阶段统一解析。
#[derive(Clone, Debug)]
struct HprofThreadStartRecord {
    /// 线程启动记录关联的 stack trace serial。
    stack_trace_serial: u32,
    /// 线程名字符串 ID。
    thread_name_id: HprofObjectId,
}

/// ROOT_THREAD_OBJECT 子记录。
///
/// 业务意图：
/// - 部分 dump 可能没有完整 START_THREAD 记录，Thread Object root 仍可提供线程对象与栈轨迹的关联。
#[derive(Clone, Debug)]
struct HprofThreadObjectRootRecord {
    /// Thread Object root 关联的 stack trace serial。
    stack_trace_serial: u32,
}

/// STACK_TRACE 顶层记录。
#[derive(Clone, Debug)]
struct HprofStackTraceRecord {
    /// 关联的 thread serial。
    thread_serial: u32,
    /// 按调用顺序保存的 frame ID。
    frame_ids: Vec<HprofObjectId>,
}

/// STACK_FRAME 顶层记录。
#[derive(Clone, Debug)]
struct HprofStackFrameRecord {
    /// 方法名字符串 ID。
    method_name_id: HprofObjectId,
    /// JVM 方法签名字符串 ID。
    method_signature_id: HprofObjectId,
    /// 源文件名字符串 ID。
    source_file_id: HprofObjectId,
    /// LOAD_CLASS serial。
    class_serial: u32,
    /// HPROF 原始行号；负数有特殊含义。
    line_number: i32,
}

/// 延迟解析出的 `java.lang.Thread` 实例字段。
///
/// 业务意图：
/// - Retained heap 要等 dominator 完成才能得到，但 name、daemon、priority、threadStatus 等来自实例字段，
///   因此在解析阶段先保存结构化原始值，完成阶段再拼成最终属性表。
#[derive(Clone, Debug, Default)]
struct HprofThreadInstanceInfo {
    /// 线程对象 ID。
    object_id: HprofObjectId,
    /// 线程对象类 ID。
    class_id: HprofObjectId,
    /// `Thread.name` 字段引用的 `java.lang.String` 对象 ID。
    name_object_id: Option<HprofObjectId>,
    /// `Thread.contextClassLoader` 字段引用的对象 ID。
    context_class_loader_id: Option<HprofObjectId>,
    /// `Thread.daemon` 字段值。
    daemon: Option<bool>,
    /// `Thread.priority` 字段值。
    priority: Option<i32>,
    /// `Thread.threadStatus` 字段值。
    thread_status: Option<u32>,
}

/// 可用于轻量字符串 fallback 的 primitive array 数据位置。
///
/// 边界条件：
/// - 只保存较短的 `byte[]` / `char[]` 偏移，避免为了少数线程名把大型业务 byte array 全部复制到内存。
#[derive(Clone, Copy, Debug)]
struct PendingPrimitiveArrayDump {
    /// HPROF 基础类型编码。
    element_type: u8,
    /// 数组元素数量。
    length: u32,
    /// 数组数据在输入切片中的起始偏移。
    data_start: usize,
    /// 数组数据字节数。
    data_len: usize,
}

/// 延迟物化的实例 dump。
///
/// 业务意图：
/// - 实例字段的 Reference 诊断分类依赖完整类名、字段名和继承链；解析二进制记录时只保存数据区偏移，避免提前丢失 referent 语义。
#[derive(Clone, Copy, Debug)]
struct PendingInstanceDump {
    /// 实例对象 ID。
    object_id: HprofObjectId,
    /// 实例所属类 ID。
    class_id: HprofObjectId,
    /// 实例字段数据在 mmap/输入切片中的起始偏移。
    data_start: usize,
    /// 实例字段数据长度。
    data_len: usize,
}

/// HPROF parser 的可变状态。
///
/// 业务意图：
/// - 解析过程需要同时累积对象图、类布局缓存和进度计数；集中在状态结构里可以让读取函数签名保持简单。
#[derive(Default)]
struct HprofParserState {
    /// 当前对象图，读取 header 后初始化。
    graph: Option<HprofObjectGraph>,
    /// 原始类布局。
    raw_classes: FxHashMap<HprofObjectId, RawClassInfo>,
    /// 需要在类名和字段名解析完成后再物化对象引用边的实例对象。
    pending_instances: Vec<PendingInstanceDump>,
    /// 可用于解析线程名 fallback 的短基础类型数组数据位置。
    pending_primitive_arrays: FxHashMap<HprofObjectId, PendingPrimitiveArrayDump>,
    /// 已展开继承后的引用字段偏移缓存。
    reference_layout_cache: FxHashMap<HprofObjectId, ResolvedReferenceLayout>,
}

/// 已解析的引用字段布局。
///
/// 业务意图：
/// - `INSTANCE_DUMP` 数量通常远大于类数量，缓存对象引用字段的字节偏移可以避免每个实例都遍历完整字段描述。
#[derive(Clone, Debug)]
struct ResolvedReferenceLayout {
    /// 对象引用字段在实例数据区中的字节偏移及 MAT 引用强度。
    reference_fields: Vec<ResolvedReferenceField>,
    /// 该类继承展开后的 HPROF 实例数据字节数，用于读取字段偏移。
    hprof_byte_size: usize,
    /// 该类按 MAT size model 估算的字段区域字节数，用于 shallow size。
    heap_field_byte_size: u64,
}

/// 已解析的对象引用字段。
#[derive(Clone, Debug)]
struct ResolvedReferenceField {
    /// 该对象 ID 在实例数据区中的字节偏移。
    offset: usize,
    /// 该引用在 MAT 兼容对象图中的强度。
    strength: HprofReferenceStrength,
}

/// HPROF 二进制 parser。
///
/// 边界条件：
/// - 所有读取方法都会维护 `position`，用于进度展示和 heap segment 边界校验。
struct HprofParser<'a, F>
where
    F: FnMut(HprofProgress),
{
    /// HPROF 文件完整字节。
    bytes: &'a [u8],
    /// 已读取字节偏移。
    position: usize,
    /// header 中声明的对象 ID 字节数。
    identifier_size: u8,
    /// 当前进度。
    progress: HprofProgress,
    /// 进度回调。
    progress_reporter: &'a mut F,
    /// 取消标记。
    cancel_flag: &'a AtomicBool,
    /// 累积解析状态。
    state: HprofParserState,
}

impl<'a, F> HprofParser<'a, F>
where
    F: FnMut(HprofProgress),
{
    /// 执行完整 HPROF 对象图解析。
    fn parse(&mut self) -> Result<HprofObjectGraph, HprofError> {
        self.report_stage(HprofAnalysisStage::ReadingHeader, "正在读取 HPROF header")?;
        let header = self.read_header()?;
        let size_model = HprofSizeModel::mat_compatible(&header, self.progress.total_bytes);
        self.state.graph = Some(HprofObjectGraph::new(header, size_model));
        self.report_stage(HprofAnalysisStage::ReadingRecords, "正在解析 HPROF 记录")?;

        loop {
            self.check_cancel()?;
            if self.is_eof() {
                break;
            }
            let tag = self.read_u8()?;
            let _time_delta = self.read_u32()?;
            let length = self.read_u32()?;
            self.progress.record_count += 1;

            match tag {
                TAG_STRING_IN_UTF8 => self.read_string_record(length)?,
                TAG_LOAD_CLASS => self.read_load_class_record(length)?,
                TAG_STACK_FRAME => self.read_stack_frame_record(length)?,
                TAG_STACK_TRACE => self.read_stack_trace_record(length)?,
                TAG_START_THREAD => self.read_start_thread_record(length)?,
                TAG_END_THREAD => self.skip_bytes(u64::from(length))?,
                TAG_HEAP_DUMP | TAG_HEAP_DUMP_SEGMENT => self.read_heap_dump_record(length)?,
                TAG_HEAP_DUMP_END => self.skip_bytes(u64::from(length))?,
                _ => self.skip_bytes(u64::from(length))?,
            }
            self.refresh_progress_counts();
            self.maybe_report_progress();
        }

        self.report_stage(HprofAnalysisStage::ResolvingClasses, "正在解析类名和字段名")?;
        self.resolve_class_and_field_names()?;
        self.report_stage(
            HprofAnalysisStage::MaterializingReferences,
            "正在构建 MAT 兼容引用边",
        )?;
        self.materialize_mat_compatible_graph()?;
        self.refresh_progress_counts();
        self.report_progress();
        self.state
            .graph
            .take()
            .ok_or_else(|| HprofError::InvalidFormat("内部错误：HPROF 对象图未初始化".to_string()))
    }

    /// 读取 HPROF header。
    fn read_header(&mut self) -> Result<HprofHeader, HprofError> {
        let mut label_bytes = Vec::new();
        for _ in 0..HPROF_HEADER_MAX_BYTES {
            let byte = self.read_u8()?;
            if byte == 0 {
                let label = String::from_utf8(label_bytes).map_err(|_| {
                    HprofError::InvalidFormat("HPROF 文件头不是有效 UTF-8".to_string())
                })?;
                validate_hprof_label(&label)?;
                let identifier_size = self.read_u32()?;
                if !matches!(identifier_size, 4 | 8) {
                    return Err(HprofError::InvalidFormat(format!(
                        "不支持的 HPROF 对象 ID 字节数：{identifier_size}"
                    )));
                }
                self.identifier_size = identifier_size as u8;
                let timestamp_millis = self.read_u64()?;
                return Ok(HprofHeader {
                    label,
                    identifier_size: identifier_size as u8,
                    timestamp_millis,
                });
            }
            label_bytes.push(byte);
        }
        Err(HprofError::InvalidFormat(
            "HPROF 文件头过长，可能不是 JVM HPROF 文件".to_string(),
        ))
    }

    /// 读取 UTF8 字符串记录。
    fn read_string_record(&mut self, length: u32) -> Result<(), HprofError> {
        let id_size = self.id_size()?;
        if length < u32::from(id_size) {
            return Err(HprofError::InvalidFormat(
                "STRING_IN_UTF8 记录长度小于对象 ID 长度".to_string(),
            ));
        }
        let id = self.read_id()?;
        let string_len = length as usize - id_size as usize;
        let bytes = self.read_slice(string_len)?;
        let value = String::from_utf8_lossy(bytes).into_owned();
        self.graph_mut()?.strings.insert(id, value);
        Ok(())
    }

    /// 读取 STACK_FRAME 记录。
    ///
    /// 业务意图：
    /// - MAT 线程详情中的每一行堆栈都来自该记录；这里先保留字符串 ID，等 STRING 和 LOAD_CLASS 全部解析后再生成展示文案。
    fn read_stack_frame_record(&mut self, length: u32) -> Result<(), HprofError> {
        let id_size = self.id_size()?;
        let expected = u32::from(id_size) * 4 + 8;
        if length < expected {
            return Err(HprofError::InvalidFormat(
                "STACK_FRAME 记录长度不完整".to_string(),
            ));
        }
        let frame_id = self.read_id()?;
        let method_name_id = self.read_id()?;
        let method_signature_id = self.read_id()?;
        let source_file_id = self.read_id()?;
        let class_serial = self.read_u32()?;
        let line_number = self.read_i32()?;
        self.graph_mut()?.stack_frames.insert(
            frame_id,
            HprofStackFrameRecord {
                method_name_id,
                method_signature_id,
                source_file_id,
                class_serial,
                line_number,
            },
        );
        let extra = u64::from(length - expected);
        if extra > 0 {
            self.skip_bytes(extra)?;
        }
        Ok(())
    }

    /// 读取 STACK_TRACE 记录。
    fn read_stack_trace_record(&mut self, length: u32) -> Result<(), HprofError> {
        let id_size = self.id_size()?;
        if length < 12 {
            return Err(HprofError::InvalidFormat(
                "STACK_TRACE 记录长度不完整".to_string(),
            ));
        }
        let stack_trace_serial = self.read_u32()?;
        let thread_serial = self.read_u32()?;
        let frame_count = self.read_u32()?;
        let expected = 12u32
            .checked_add(frame_count.saturating_mul(u32::from(id_size)))
            .ok_or_else(|| HprofError::InvalidFormat("STACK_TRACE 长度溢出".to_string()))?;
        if length < expected {
            return Err(HprofError::InvalidFormat(
                "STACK_TRACE frame 列表长度不完整".to_string(),
            ));
        }
        let mut frame_ids = Vec::with_capacity(frame_count as usize);
        for _ in 0..frame_count {
            frame_ids.push(self.read_id()?);
        }
        self.graph_mut()?.stack_traces.insert(
            stack_trace_serial,
            HprofStackTraceRecord {
                thread_serial,
                frame_ids,
            },
        );
        let extra = u64::from(length - expected);
        if extra > 0 {
            self.skip_bytes(extra)?;
        }
        Ok(())
    }

    /// 读取 START_THREAD 记录。
    fn read_start_thread_record(&mut self, length: u32) -> Result<(), HprofError> {
        let id_size = self.id_size()?;
        let expected = 8 + u32::from(id_size) * 4;
        if length < expected {
            return Err(HprofError::InvalidFormat(
                "START_THREAD 记录长度不完整".to_string(),
            ));
        }
        let thread_serial = self.read_u32()?;
        let object_id = self.read_id()?;
        let stack_trace_serial = self.read_u32()?;
        let thread_name_id = self.read_id()?;
        let _thread_group_name_id = self.read_id()?;
        let _thread_parent_group_name_id = self.read_id()?;
        let graph = self.graph_mut()?;
        graph
            .thread_serial_by_object_id
            .insert(object_id, thread_serial);
        graph.thread_starts.insert(
            thread_serial,
            HprofThreadStartRecord {
                stack_trace_serial,
                thread_name_id,
            },
        );
        let extra = u64::from(length - expected);
        if extra > 0 {
            self.skip_bytes(extra)?;
        }
        Ok(())
    }

    /// 读取 LOAD_CLASS 记录。
    fn read_load_class_record(&mut self, length: u32) -> Result<(), HprofError> {
        let id_size = self.id_size()?;
        let expected = 4 + u32::from(id_size) + 4 + u32::from(id_size);
        if length < expected {
            return Err(HprofError::InvalidFormat(
                "LOAD_CLASS 记录长度不完整".to_string(),
            ));
        }
        let serial = self.read_u32()?;
        let class_object_id = self.read_id()?;
        let _stack_serial = self.read_u32()?;
        let name_string_id = self.read_id()?;
        self.graph_mut()?
            .loaded_classes
            .insert(serial, (class_object_id, name_string_id));
        let extra = u64::from(length - expected);
        if extra > 0 {
            self.skip_bytes(extra)?;
        }
        Ok(())
    }

    /// 读取 HEAP_DUMP 或 HEAP_DUMP_SEGMENT 记录。
    fn read_heap_dump_record(&mut self, length: u32) -> Result<(), HprofError> {
        let segment_end = self
            .position
            .checked_add(length as usize)
            .ok_or_else(|| HprofError::InvalidFormat("HPROF segment 长度溢出".to_string()))?;
        while self.position < segment_end {
            self.check_cancel()?;
            let sub_tag = self.read_u8()?;
            self.progress.record_count = self.progress.record_count.saturating_add(1);
            self.read_heap_sub_record(sub_tag)?;
            if self.position > segment_end {
                return Err(HprofError::InvalidFormat(format!(
                    "HEAP_DUMP 子记录越过 segment 边界：0x{sub_tag:02X}"
                )));
            }
            if self
                .progress
                .record_count
                .is_multiple_of(HPROF_PROGRESS_RECORD_INTERVAL)
            {
                self.refresh_progress_counts();
                self.report_progress();
            }
        }
        Ok(())
    }

    /// 读取单个 heap dump 子记录。
    fn read_heap_sub_record(&mut self, sub_tag: u8) -> Result<(), HprofError> {
        match sub_tag {
            SUB_ROOT_UNKNOWN => self.read_root_simple(HprofGcRootKind::Unknown),
            SUB_ROOT_JNI_GLOBAL => {
                let object_id = self.read_id()?;
                let _jni_global_ref = self.read_id()?;
                self.push_gc_root(object_id, HprofGcRootKind::JniGlobal)
            }
            SUB_ROOT_JNI_LOCAL => self.read_root_with_thread_and_frame(HprofGcRootKind::JniLocal),
            SUB_ROOT_JAVA_FRAME => self.read_root_with_thread_and_frame(HprofGcRootKind::JavaFrame),
            SUB_ROOT_NATIVE_STACK => self.read_root_with_thread(HprofGcRootKind::NativeStack),
            SUB_ROOT_STICKY_CLASS => self.read_root_simple(HprofGcRootKind::StickyClass),
            SUB_ROOT_THREAD_BLOCK => self.read_root_with_thread(HprofGcRootKind::ThreadBlock),
            SUB_ROOT_MONITOR_USED => self.read_root_simple(HprofGcRootKind::MonitorUsed),
            SUB_ROOT_THREAD_OBJECT => {
                let object_id = self.read_id()?;
                let thread_serial = self.read_u32()?;
                let stack_trace_serial = self.read_u32()?;
                {
                    let graph = self.graph_mut()?;
                    graph
                        .thread_serial_by_object_id
                        .entry(object_id)
                        .or_insert(thread_serial);
                    graph.thread_object_roots.insert(
                        object_id,
                        HprofThreadObjectRootRecord { stack_trace_serial },
                    );
                }
                self.push_gc_root(object_id, HprofGcRootKind::ThreadObject)
            }
            SUB_HEAP_DUMP_INFO => {
                let _heap_type = self.read_u32()?;
                let _heap_name_id = self.read_id()?;
                Ok(())
            }
            SUB_PRIMITIVE_ARRAY_NODATA => {
                let _array_id = self.read_id()?;
                let _stack_serial = self.read_u32()?;
                let _elements = self.read_u32()?;
                let _element_type = self.read_u8()?;
                Ok(())
            }
            SUB_CLASS_DUMP => self.read_class_dump(),
            SUB_INSTANCE_DUMP => self.read_instance_dump(),
            SUB_OBJECT_ARRAY_DUMP => self.read_object_array_dump(),
            SUB_PRIMITIVE_ARRAY_DUMP => self.read_primitive_array_dump(),
            _ => Err(HprofError::Unsupported(format!(
                "暂不支持的 HEAP_DUMP 子记录：0x{sub_tag:02X}，偏移 {}",
                self.position.saturating_sub(1)
            ))),
        }
    }

    /// 读取只包含对象 ID 的 GC root。
    fn read_root_simple(&mut self, kind: HprofGcRootKind) -> Result<(), HprofError> {
        let object_id = self.read_id()?;
        self.push_gc_root(object_id, kind)
    }

    /// 读取包含线程序号的 GC root。
    fn read_root_with_thread(&mut self, kind: HprofGcRootKind) -> Result<(), HprofError> {
        let object_id = self.read_id()?;
        let _thread_serial = self.read_u32()?;
        self.push_gc_root(object_id, kind)
    }

    /// 读取包含线程序号和 frame 序号的 GC root。
    fn read_root_with_thread_and_frame(&mut self, kind: HprofGcRootKind) -> Result<(), HprofError> {
        let object_id = self.read_id()?;
        let _thread_serial = self.read_u32()?;
        let _frame = self.read_u32()?;
        self.push_gc_root(object_id, kind)
    }

    /// 写入 GC root。
    fn push_gc_root(
        &mut self,
        object_id: HprofObjectId,
        kind: HprofGcRootKind,
    ) -> Result<(), HprofError> {
        self.graph_mut()?
            .gc_roots
            .push(HprofGcRoot { object_id, kind });
        Ok(())
    }

    /// 读取 CLASS_DUMP。
    fn read_class_dump(&mut self) -> Result<(), HprofError> {
        let class_object_id = self.read_id()?;
        let _stack_serial = self.read_u32()?;
        let super_class_id = self.read_id()?;
        let class_loader_id = self.read_id()?;
        let _signers = self.read_id()?;
        let _protection_domain = self.read_id()?;
        let _reserved_1 = self.read_id()?;
        let _reserved_2 = self.read_id()?;
        let instance_size = u64::from(self.read_u32()?);

        let constant_pool_count = self.read_u16()?;
        for _ in 0..constant_pool_count {
            let _index = self.read_u16()?;
            let field_type = self.read_u8()?;
            self.skip_field_value(field_type)?;
        }

        let static_field_count = self.read_u16()?;
        let mut static_fields = Vec::with_capacity(static_field_count as usize);
        for _ in 0..static_field_count {
            let name_id = self.read_id()?;
            let field_type = self.read_u8()?;
            let reference_id = if field_type == FIELD_TYPE_OBJECT {
                Some(self.read_id()?)
            } else {
                self.skip_field_value(field_type)?;
                None
            };
            static_fields.push(RawStaticFieldDescriptor {
                _name_id: name_id,
                field_type,
                reference_id,
            });
        }

        let instance_field_count = self.read_u16()?;
        let mut instance_fields = Vec::with_capacity(instance_field_count as usize);
        for _ in 0..instance_field_count {
            let name_id = self.read_id()?;
            let field_type = self.read_u8()?;
            instance_fields.push(RawFieldDescriptor {
                name_id,
                field_type,
            });
        }

        self.state.raw_classes.insert(
            class_object_id,
            RawClassInfo {
                super_class_id,
                class_loader_id,
                static_fields: static_fields.clone(),
                instance_fields: instance_fields.clone(),
            },
        );
        let field_descriptors = instance_fields
            .iter()
            .map(|field| HprofFieldDescriptor {
                name: None,
                field_type: field.field_type,
            })
            .collect();
        let graph = self.graph_mut()?;
        graph.classes.insert(
            class_object_id,
            HprofClassInfo {
                instance_size,
                name: None,
                instance_fields: field_descriptors,
            },
        );
        graph.insert_object(
            HprofHeapObject {
                id: class_object_id,
                class_id: class_object_id,
                shallow_size: 0,
                kind: HprofObjectKind::Class,
            },
            Vec::new(),
        )?;
        Ok(())
    }

    /// 读取 INSTANCE_DUMP。
    fn read_instance_dump(&mut self) -> Result<(), HprofError> {
        let object_id = self.read_id()?;
        let _stack_serial = self.read_u32()?;
        let class_id = self.read_id()?;
        let data_len = self.read_u32()? as usize;
        let data_start = self.position;
        self.skip_bytes(data_len as u64)?;
        self.state.pending_instances.push(PendingInstanceDump {
            object_id,
            class_id,
            data_start,
            data_len,
        });

        self.graph_mut()?.insert_object(
            HprofHeapObject {
                id: object_id,
                class_id,
                shallow_size: 0,
                kind: HprofObjectKind::Instance,
            },
            Vec::new(),
        )?;
        Ok(())
    }

    /// 读取 OBJECT_ARRAY_DUMP。
    fn read_object_array_dump(&mut self) -> Result<(), HprofError> {
        let array_id = self.read_id()?;
        let _stack_serial = self.read_u32()?;
        let length = self.read_u32()?;
        let array_class_id = self.read_id()?;
        let mut references = Vec::with_capacity(length as usize);
        for _ in 0..length {
            let reference_id = self.read_id()?;
            push_non_zero_reference(&mut references, reference_id);
        }
        let shallow_size = object_array_shallow_size(&self.graph_ref()?.size_model, length);
        self.graph_mut()?.insert_object(
            HprofHeapObject {
                id: array_id,
                class_id: array_class_id,
                shallow_size,
                kind: HprofObjectKind::ObjectArray { length },
            },
            references,
        )?;
        Ok(())
    }

    /// 读取 PRIMITIVE_ARRAY_DUMP。
    fn read_primitive_array_dump(&mut self) -> Result<(), HprofError> {
        let array_id = self.read_id()?;
        let _stack_serial = self.read_u32()?;
        let length = self.read_u32()?;
        let element_type = self.read_u8()?;
        let width = field_value_size(element_type, self.id_size()?).ok_or_else(|| {
            HprofError::InvalidFormat(format!("未知 primitive array 元素类型：{element_type}"))
        })?;
        let data_len = u64::from(length) * u64::from(width);
        let data_start = self.position;
        self.skip_bytes(data_len)?;
        if matches!(element_type, FIELD_TYPE_BYTE | FIELD_TYPE_CHAR)
            && length <= HPROF_THREAD_STRING_FALLBACK_MAX_ELEMENTS
        {
            let data_len = usize::try_from(data_len).map_err(|_| {
                HprofError::InvalidFormat("primitive array 数据长度超过当前平台限制".to_string())
            })?;
            self.state.pending_primitive_arrays.insert(
                array_id,
                PendingPrimitiveArrayDump {
                    element_type,
                    length,
                    data_start,
                    data_len,
                },
            );
        }
        let shallow_size = primitive_array_shallow_size(
            &self.graph_ref()?.size_model,
            element_type,
            u64::from(length),
        )?;
        self.graph_mut()?.insert_object(
            HprofHeapObject {
                id: array_id,
                class_id: 0,
                shallow_size,
                kind: HprofObjectKind::PrimitiveArray {
                    element_type,
                    length,
                },
            },
            Vec::new(),
        )?;
        Ok(())
    }

    /// 解析 LOAD_CLASS、STRING_IN_UTF8 得到类名和字段名。
    ///
    /// 业务意图：
    /// - MAT 兼容引用诊断依赖 `java.lang.ref.Reference.referent` 的字段名和声明类；必须在物化对象引用边前完成解析。
    fn resolve_class_and_field_names(&mut self) -> Result<(), HprofError> {
        let class_names = {
            let graph = self.graph_ref()?;
            graph
                .loaded_classes
                .values()
                .filter_map(|(class_object_id, name_string_id)| {
                    graph
                        .strings
                        .get(name_string_id)
                        .map(|name| (*class_object_id, normalize_java_class_name(name)))
                })
                .collect::<FxHashMap<_, _>>()
        };
        let class_updates = self
            .state
            .raw_classes
            .iter()
            .map(|(class_id, raw_class)| {
                let instance_fields = raw_class
                    .instance_fields
                    .iter()
                    .map(|field| HprofFieldDescriptor {
                        name: self
                            .state
                            .graph
                            .as_ref()
                            .and_then(|graph| graph.strings.get(&field.name_id))
                            .cloned(),
                        field_type: field.field_type,
                    })
                    .collect::<Vec<_>>();
                (*class_id, instance_fields)
            })
            .collect::<Vec<_>>();
        for (class_id, instance_fields) in class_updates {
            if let Some(class_info) = self.graph_mut()?.classes.get_mut(&class_id) {
                class_info.name = class_names.get(&class_id).cloned();
                class_info.instance_fields = instance_fields;
            }
        }
        Ok(())
    }

    /// 基于完整类元数据构建 MAT 兼容对象引用边和 shallow size。
    ///
    /// 实现原因：
    /// - 二进制扫描阶段无法可靠判断 `Reference.referent` 是否弱/软/虚引用，因为类名、字段名和继承链可能在后续记录中才完整。
    /// - 这里统一扫描延迟保存的实例数据，把所有对象引用写入 MAT 对象图，并同步统计 Reference referent 分类和 shallow size。
    fn materialize_mat_compatible_graph(&mut self) -> Result<(), HprofError> {
        let class_ids = self.state.raw_classes.keys().copied().collect::<Vec<_>>();
        // pending instance 表在引用物化后不再使用，直接移动可避免大 dump 中额外复制数百万条偏移记录。
        let pending_instances = std::mem::take(&mut self.state.pending_instances);
        let total_work = class_ids.len().saturating_add(pending_instances.len());
        let mut reference_edge_stats = HprofReferenceEdgeStats::default();
        let mut done = 0usize;

        for class_id in class_ids {
            self.check_cancel()?;
            let (shallow_size, references) = self.class_object_payload(class_id)?;
            self.graph_mut()?
                .update_object_payload(class_id, shallow_size, references);
            done = done.saturating_add(1);
            self.report_materialize_progress(done, total_work, "构建 class/static 引用边")?;
        }

        let id_size = self.id_size()? as usize;
        let mut layout_cache = std::mem::take(&mut self.state.reference_layout_cache);
        let (java_string_class_ids, thread_class_ids) = {
            let graph = self.graph_ref()?;
            (
                collect_subclass_ids(&self.state.raw_classes, &graph.classes, "java.lang.String"),
                collect_subclass_ids(&self.state.raw_classes, &graph.classes, "java.lang.Thread"),
            )
        };
        for pending in pending_instances {
            self.check_cancel()?;
            let (shallow_size, references, edge_stats) =
                self.instance_payload(pending, id_size, &mut layout_cache)?;
            // 线程名 fallback 只需要 String/Thread 两类对象；预先按 class_id 筛选，避免普通业务对象反复走继承链。
            let java_string_value = if java_string_class_ids.contains(&pending.class_id) {
                self.decode_java_string_instance(pending, id_size)?
            } else {
                None
            };
            let thread_instance = if thread_class_ids.contains(&pending.class_id) {
                self.decode_thread_instance(pending, id_size)?
            } else {
                None
            };
            reference_edge_stats.merge(edge_stats);
            let graph = self.graph_mut()?;
            graph.update_object_payload(pending.object_id, shallow_size, references);
            if let Some(value) = java_string_value {
                graph.java_string_values.insert(pending.object_id, value);
            }
            if let Some(thread_instance) = thread_instance {
                graph
                    .thread_instances
                    .insert(thread_instance.object_id, thread_instance);
            }
            done = done.saturating_add(1);
            self.report_materialize_progress(
                done,
                total_work,
                "构建实例引用边 / 计算 shallow size",
            )?;
        }
        self.state.reference_layout_cache = layout_cache;
        self.graph_mut()?.reference_edge_stats = reference_edge_stats;
        self.report_materialize_progress(done, total_work, "补充 ClassLoader 合成可达边")?;
        self.materialize_class_loader_edges()?;
        self.refresh_progress_counts();
        Ok(())
    }

    /// 补充 MAT 兼容的 class loader 可达关系。
    ///
    /// 业务意图：
    /// - HPROF `CLASS_DUMP` 只记录 class object 的 `class_loader_id`，这不是普通 Java 字段；
    ///   如果只建 `class object -> class loader`，从已可达的 class loader 无法走到它加载的 class object，class static 字段也会断开。
    /// - MAT 会把“类加载器定义的类”作为可达关系处理，因此这里合成 `class loader -> class object` 强边。
    /// - `class_loader_id == 0` 的 bootstrap/system class 没有普通对象可挂载，直接加入虚拟 root 入口，保证系统类 static 字段可达。
    fn materialize_class_loader_edges(&mut self) -> Result<(), HprofError> {
        let mut loader_to_classes: FxHashMap<HprofObjectId, Vec<HprofObjectId>> =
            FxHashMap::default();
        let mut bootstrap_class_ids = Vec::new();
        for (class_id, raw_class) in &self.state.raw_classes {
            if raw_class.class_loader_id == 0 {
                bootstrap_class_ids.push(*class_id);
            } else {
                loader_to_classes
                    .entry(raw_class.class_loader_id)
                    .or_default()
                    .push(*class_id);
            }
        }

        let mut synthetic_class_loader_edge_count = 0usize;
        for (loader_id, mut class_ids) in loader_to_classes {
            self.check_cancel()?;
            normalize_references(&mut class_ids);
            let Some(loader_index) = self.graph_ref()?.object_index(loader_id) else {
                continue;
            };
            let loader_object = &self.graph_ref()?.objects[loader_index];
            let shallow_size = loader_object.shallow_size;
            let mut references = self
                .graph_ref()?
                .references_for_index(loader_index)
                .to_vec();
            let previous_len = references.len();
            references.extend(class_ids);
            normalize_references(&mut references);
            synthetic_class_loader_edge_count = synthetic_class_loader_edge_count
                .saturating_add(references.len().saturating_sub(previous_len));
            self.graph_mut()?
                .update_object_payload(loader_id, shallow_size, references);
        }

        let mut synthetic_bootstrap_class_root_count = 0usize;
        normalize_references(&mut bootstrap_class_ids);
        for class_id in bootstrap_class_ids {
            self.check_cancel()?;
            if self.graph_ref()?.contains_object_id(class_id) {
                self.graph_mut()?.gc_roots.push(HprofGcRoot {
                    object_id: class_id,
                    kind: HprofGcRootKind::SyntheticBootstrapClass,
                });
                synthetic_bootstrap_class_root_count =
                    synthetic_bootstrap_class_root_count.saturating_add(1);
            }
        }

        let graph = self.graph_mut()?;
        graph.synthetic_class_loader_edge_count = synthetic_class_loader_edge_count;
        graph.synthetic_bootstrap_class_root_count = synthetic_bootstrap_class_root_count;
        Ok(())
    }

    /// 构建 class object 的对象引用列表。
    fn class_object_payload(
        &self,
        class_id: HprofObjectId,
    ) -> Result<(u64, Vec<HprofObjectId>), HprofError> {
        let raw_class = self.state.raw_classes.get(&class_id).ok_or_else(|| {
            HprofError::InvalidFormat(format!("内部错误：缺少 class 0x{class_id:x} 的原始信息"))
        })?;
        let mut references = Vec::new();
        for field in &raw_class.static_fields {
            if field.field_type == FIELD_TYPE_OBJECT
                && let Some(reference_id) = field.reference_id
            {
                push_non_zero_reference(&mut references, reference_id);
            }
        }
        push_non_zero_reference(&mut references, raw_class.super_class_id);
        push_non_zero_reference(&mut references, raw_class.class_loader_id);
        Ok((0, references))
    }

    /// 构建单个实例对象的 MAT 兼容 shallow size、对象引用列表和 Reference referent 统计。
    fn instance_payload(
        &self,
        pending: PendingInstanceDump,
        id_size: usize,
        layout_cache: &mut FxHashMap<HprofObjectId, ResolvedReferenceLayout>,
    ) -> Result<(u64, Vec<HprofObjectId>, HprofReferenceEdgeStats), HprofError> {
        let data_end = pending
            .data_start
            .checked_add(pending.data_len)
            .ok_or_else(|| HprofError::InvalidFormat("实例字段数据偏移溢出".to_string()))?;
        if data_end > self.bytes.len() {
            return Err(HprofError::InvalidFormat(format!(
                "实例 0x{:x} 字段数据越过文件边界",
                pending.object_id
            )));
        }
        let data = &self.bytes[pending.data_start..data_end];
        let layout = {
            let graph = self.graph_ref()?;
            resolve_reference_layout(
                &self.state.raw_classes,
                &graph.classes,
                &graph.strings,
                layout_cache,
                pending.class_id,
                self.identifier_size,
                &graph.size_model,
            )
        };

        let mut references = Vec::new();
        let mut reference_edge_stats = HprofReferenceEdgeStats::default();
        let shallow_size = if let Some(layout) = layout {
            for field in &layout.reference_fields {
                if field.offset + id_size <= data.len() {
                    let reference_id =
                        read_id_from_slice(&data[field.offset..field.offset + id_size]);
                    if reference_id == 0 {
                        continue;
                    }
                    references.push(reference_id);
                    reference_edge_stats.add_strength(field.strength);
                }
            }
            instance_shallow_size(&self.graph_ref()?.size_model, layout.heap_field_byte_size)
        } else {
            let fallback_size = self
                .graph_ref()?
                .classes
                .get(&pending.class_id)
                .map(|class| class.instance_size)
                .unwrap_or(pending.data_len as u64);
            instance_shallow_size(&self.graph_ref()?.size_model, fallback_size)
        };
        Ok((shallow_size, references, reference_edge_stats))
    }

    /// 尝试从 `java.lang.String` 实例解析字符串值。
    ///
    /// 业务意图：
    /// - START_THREAD 是线程名的首选来源；当 dump 缺失 START_THREAD 时，仍可通过 `Thread.name` 指向的 String 实例恢复部分线程名。
    /// - 这里只支持常见 HotSpot `char[] value` 和 compact-string `byte[] value + coder`，解析失败返回 `None` 而不影响主分析。
    /// - 调用方已用 class_id 缓存确认对象属于 `java.lang.String`，这里不再重复走继承链。
    fn decode_java_string_instance(
        &self,
        pending: PendingInstanceDump,
        id_size: usize,
    ) -> Result<Option<String>, HprofError> {
        let data = self.pending_instance_data(pending)?;
        let mut value_array_id = None;
        let mut coder = None;
        let mut offset = None;
        let mut count = None;
        self.for_each_instance_field(
            pending.class_id,
            data,
            id_size,
            |name, field_type, bytes| match (name, field_type) {
                ("value", FIELD_TYPE_OBJECT) => value_array_id = Some(read_id_from_slice(bytes)),
                ("coder", FIELD_TYPE_BYTE) => coder = bytes.first().copied(),
                ("offset", FIELD_TYPE_INT) => {
                    offset = Some(read_i32_from_slice(bytes).max(0) as usize)
                }
                ("count", FIELD_TYPE_INT) => {
                    count = Some(read_i32_from_slice(bytes).max(0) as usize)
                }
                _ => {}
            },
        )?;

        let Some(value_array_id) = value_array_id.filter(|id| *id != 0) else {
            return Ok(None);
        };
        Ok(self.decode_java_string_array(value_array_id, coder, offset, count))
    }

    /// 尝试从 `java.lang.Thread` 或其子类实例解析线程属性原始值。
    ///
    /// 业务意图：
    /// - 调用方已用 class_id 缓存确认对象属于线程类，这里只负责读取字段值，避免对每个普通实例重复做继承链判断。
    fn decode_thread_instance(
        &self,
        pending: PendingInstanceDump,
        id_size: usize,
    ) -> Result<Option<HprofThreadInstanceInfo>, HprofError> {
        let data = self.pending_instance_data(pending)?;
        let mut info = HprofThreadInstanceInfo {
            object_id: pending.object_id,
            class_id: pending.class_id,
            ..HprofThreadInstanceInfo::default()
        };
        self.for_each_instance_field(
            pending.class_id,
            data,
            id_size,
            |name, field_type, bytes| match (name, field_type) {
                ("name", FIELD_TYPE_OBJECT) => {
                    let value = read_id_from_slice(bytes);
                    if value != 0 {
                        info.name_object_id = Some(value);
                    }
                }
                ("contextClassLoader", FIELD_TYPE_OBJECT) => {
                    let value = read_id_from_slice(bytes);
                    if value != 0 {
                        info.context_class_loader_id = Some(value);
                    }
                }
                ("daemon", FIELD_TYPE_BOOLEAN) => {
                    info.daemon = bytes.first().map(|value| *value != 0);
                }
                ("priority", FIELD_TYPE_INT) => {
                    info.priority = Some(read_i32_from_slice(bytes));
                }
                ("threadStatus", FIELD_TYPE_INT) => {
                    info.thread_status = Some(read_u32_from_slice(bytes));
                }
                _ => {}
            },
        )?;

        Ok(Some(info))
    }

    /// 返回延迟实例字段数据切片。
    fn pending_instance_data(&self, pending: PendingInstanceDump) -> Result<&[u8], HprofError> {
        let data_end = pending
            .data_start
            .checked_add(pending.data_len)
            .ok_or_else(|| HprofError::InvalidFormat("实例字段数据偏移溢出".to_string()))?;
        if data_end > self.bytes.len() {
            return Err(HprofError::InvalidFormat(format!(
                "实例 0x{:x} 字段数据越过文件边界",
                pending.object_id
            )));
        }
        Ok(&self.bytes[pending.data_start..data_end])
    }

    /// 以 HPROF INSTANCE_DUMP 字节顺序遍历实例字段。
    ///
    /// 边界条件：
    /// - 字段布局缺失或字段数据截断时返回格式错误；调用方可以把错误转成用户可见中文说明，而不是产生错位解析。
    fn for_each_instance_field(
        &self,
        class_id: HprofObjectId,
        data: &[u8],
        id_size: usize,
        mut visitor: impl FnMut(&str, u8, &[u8]),
    ) -> Result<(), HprofError> {
        let mut fields = Vec::new();
        collect_declared_fields(&self.state.raw_classes, class_id, &mut fields).ok_or_else(
            || HprofError::InvalidFormat(format!("缺少类 0x{class_id:x} 的实例字段布局")),
        )?;
        let mut offset = 0usize;
        for (_declaring_class_id, field) in fields {
            let width =
                field_value_size(field.field_type, self.identifier_size).ok_or_else(|| {
                    HprofError::InvalidFormat(format!("未知 HPROF 字段类型：{}", field.field_type))
                })? as usize;
            let end = offset.saturating_add(width);
            if end > data.len() {
                return Err(HprofError::InvalidFormat(format!(
                    "类 0x{class_id:x} 的实例字段数据截断"
                )));
            }
            if let Some(name) = self.graph_ref()?.strings.get(&field.name_id) {
                visitor(name, field.field_type, &data[offset..end]);
            }
            offset = end;
        }
        let _ = id_size;
        Ok(())
    }

    /// 从短 primitive array 中解码 Java String 的字符内容。
    fn decode_java_string_array(
        &self,
        array_id: HprofObjectId,
        coder: Option<u8>,
        offset: Option<usize>,
        count: Option<usize>,
    ) -> Option<String> {
        let array = self.state.pending_primitive_arrays.get(&array_id)?;
        let data_end = array.data_start.checked_add(array.data_len)?;
        let data = self.bytes.get(array.data_start..data_end)?;
        match array.element_type {
            FIELD_TYPE_CHAR => decode_char_array_string(data, array.length, offset, count),
            FIELD_TYPE_BYTE => decode_byte_array_string(data, array.length, coder, offset, count),
            _ => None,
        }
    }

    /// 报告 MAT 对象引用物化阶段进度。
    fn report_materialize_progress(
        &mut self,
        done: usize,
        total: usize,
        sub_message: &str,
    ) -> Result<(), HprofError> {
        if should_report_work(done, total) {
            self.refresh_progress_counts();
            self.progress.stage = HprofAnalysisStage::MaterializingReferences;
            self.progress.message = "正在构建 MAT 兼容引用边".to_string();
            self.progress.sub_message = sub_message.to_string();
            self.progress.phase_done = done as u64;
            self.progress.phase_total = total as u64;
            self.progress.phase_unit = "对象";
            self.report_progress();
        }
        Ok(())
    }

    /// 更新进度中的计数字段。
    fn refresh_progress_counts(&mut self) {
        if let Some(graph) = self.state.graph.as_ref() {
            self.progress.bytes_read = self.position as u64;
            self.progress.object_count = graph.object_count();
            self.progress.class_count = graph.class_count();
            self.progress.gc_root_count = graph.gc_roots.len();
            self.progress.edge_count = graph.edge_count();
        }
    }

    /// 按解析阶段报告进度。
    fn report_stage(&mut self, stage: HprofAnalysisStage, message: &str) -> Result<(), HprofError> {
        self.check_cancel()?;
        self.progress.stage = stage;
        self.progress.message = message.to_string();
        self.refresh_progress_counts();
        self.report_progress();
        Ok(())
    }

    /// 按节流规则报告进度。
    fn maybe_report_progress(&mut self) {
        if self
            .progress
            .record_count
            .is_multiple_of(HPROF_PROGRESS_RECORD_INTERVAL)
        {
            self.report_progress();
        }
    }

    /// 立即报告当前进度。
    fn report_progress(&mut self) {
        (self.progress_reporter)(self.progress.clone());
    }

    /// 检查取消状态。
    fn check_cancel(&self) -> Result<(), HprofError> {
        check_cancel(self.cancel_flag)
    }

    /// 判断是否已经读取到文件末尾。
    fn is_eof(&self) -> bool {
        self.position >= self.bytes.len()
    }

    /// 读取固定长度切片并推进偏移。
    fn read_slice(&mut self, len: usize) -> Result<&'a [u8], HprofError> {
        let end = self
            .position
            .checked_add(len)
            .ok_or_else(|| HprofError::InvalidFormat("HPROF 偏移计算溢出".to_string()))?;
        if end > self.bytes.len() {
            return Err(HprofError::InvalidFormat(format!(
                "HPROF 在偏移 {} 截断，需要读取 {} 字节",
                self.position, len
            )));
        }
        let slice = &self.bytes[self.position..end];
        self.position = end;
        Ok(slice)
    }

    /// 读取 `u8`。
    fn read_u8(&mut self) -> Result<u8, HprofError> {
        Ok(self.read_slice(1)?[0])
    }

    /// 读取大端 `u16`。
    fn read_u16(&mut self) -> Result<u16, HprofError> {
        let bytes = self.read_slice(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    /// 读取大端 `u32`。
    fn read_u32(&mut self) -> Result<u32, HprofError> {
        let bytes = self.read_slice(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// 读取大端 `i32`。
    fn read_i32(&mut self) -> Result<i32, HprofError> {
        let bytes = self.read_slice(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// 读取大端 `u64`。
    fn read_u64(&mut self) -> Result<u64, HprofError> {
        let bytes = self.read_slice(8)?;
        Ok(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    /// 按 header 中的 ID 字节数读取对象 ID。
    fn read_id(&mut self) -> Result<HprofObjectId, HprofError> {
        match self.identifier_size {
            4 => Ok(u64::from(self.read_u32()?)),
            8 => self.read_u64(),
            other => Err(HprofError::InvalidFormat(format!(
                "不支持的对象 ID 字节数：{other}"
            ))),
        }
    }

    /// 跳过指定字节数。
    fn skip_bytes(&mut self, len: u64) -> Result<(), HprofError> {
        self.check_cancel()?;
        let len = usize::try_from(len).map_err(|_| {
            HprofError::InvalidFormat("HPROF 跳过长度超过当前平台可寻址范围".to_string())
        })?;
        self.read_slice(len)?;
        Ok(())
    }

    /// 跳过指定字段类型对应的值。
    fn skip_field_value(&mut self, field_type: u8) -> Result<(), HprofError> {
        let width = field_value_size(field_type, self.id_size()?).ok_or_else(|| {
            HprofError::InvalidFormat(format!("未知 HPROF 字段类型：{field_type}"))
        })?;
        self.skip_bytes(u64::from(width))
    }

    /// 返回 HPROF 对象 ID 字节数。
    fn id_size(&self) -> Result<u8, HprofError> {
        if matches!(self.identifier_size, 4 | 8) {
            Ok(self.identifier_size)
        } else {
            Err(HprofError::InvalidFormat(
                "内部错误：HPROF header 尚未读取".to_string(),
            ))
        }
    }

    /// 返回对象图不可变引用。
    fn graph_ref(&self) -> Result<&HprofObjectGraph, HprofError> {
        self.state
            .graph
            .as_ref()
            .ok_or_else(|| HprofError::InvalidFormat("内部错误：HPROF header 尚未读取".to_string()))
    }

    /// 返回对象图可变引用。
    fn graph_mut(&mut self) -> Result<&mut HprofObjectGraph, HprofError> {
        self.state
            .graph
            .as_mut()
            .ok_or_else(|| HprofError::InvalidFormat("内部错误：HPROF header 尚未读取".to_string()))
    }
}

/// 向 UI 报告计算阶段进度。
fn report_work_progress<F>(
    reporter: &mut F,
    stage: HprofAnalysisStage,
    message: &str,
    sub_message: &str,
    phase_done: u64,
    phase_total: u64,
    phase_unit: &'static str,
) where
    F: FnMut(HprofAnalysisStage, &str, &str, u64, u64, &'static str),
{
    reporter(
        stage,
        message,
        sub_message,
        phase_done,
        phase_total,
        phase_unit,
    );
}

/// 判断是否到达需要报告进度的批量边界。
fn should_report_work(done: usize, total: usize) -> bool {
    done == total || done.is_multiple_of(HPROF_PROGRESS_WORK_INTERVAL)
}

/// 把 `usize` 工作下标压缩为 `u32`。
///
/// 边界条件：
/// - `u32::MAX` 保留给无效哨兵，因此对象数、边数和算法编号超过 `u32::MAX - 1` 时明确报错，
///   避免大 dump 上发生静默截断后生成错误 dominator tree。
fn hprof_node_id(value: usize, what: &str) -> Result<HprofNodeId, HprofError> {
    if value >= HPROF_INVALID_NODE as usize {
        return Err(HprofError::Unsupported(format!(
            "{what} 超过当前低内存索引上限，暂不支持超过 {} 的 HPROF dump",
            HPROF_INVALID_NODE - 1
        )));
    }
    Ok(value as HprofNodeId)
}

/// 将内部 `u32` 节点编号转换为数组下标。
fn hprof_node_index(value: HprofNodeId) -> usize {
    value as usize
}

/// 构建 LT 使用的紧凑邻接表。
fn build_compact_adjacency<F>(
    graph: &HprofObjectGraph,
    cancel_flag: &AtomicBool,
    reporter: &mut F,
) -> Result<HprofCompactAdjacency, HprofError>
where
    F: FnMut(HprofAnalysisStage, &str, &str, u64, u64, &'static str),
{
    let object_count = graph.object_count();
    let _ = hprof_node_id(object_count, "对象数量")?;
    let _ = hprof_node_id(graph.edge_count(), "引用边数量")?;

    for (object_index, object) in graph.objects.iter().enumerate() {
        check_cancel(cancel_flag)?;
        let _ = object;
        let done = object_index + 1;
        if should_report_work(done, object_count) {
            report_work_progress(
                reporter,
                HprofAnalysisStage::BuildingDominatorGraph,
                "正在构建紧凑引用图",
                "添加紧凑图节点",
                done as u64,
                object_count as u64,
                "对象",
            );
        }
    }

    let root_ids = graph
        .gc_roots
        .iter()
        .map(|root| root.object_id)
        .filter(|object_id| graph.contains_object_id(*object_id))
        .collect::<FxHashSet<_>>();
    let root_total = root_ids.len();
    let mut root_indices = Vec::with_capacity(root_total);
    for (root_done, root_id) in root_ids.iter().enumerate() {
        check_cancel(cancel_flag)?;
        if let Some(root_index) = graph.object_index(*root_id) {
            root_indices.push(hprof_node_id(root_index, "GC Root 对象下标")?);
        }
        let done = root_done + 1;
        if should_report_work(done, root_total) {
            report_work_progress(
                reporter,
                HprofAnalysisStage::BuildingDominatorGraph,
                "正在构建紧凑引用图",
                "添加 GC Root 边",
                done as u64,
                root_total as u64,
                "Root",
            );
        }
    }

    let mut offsets = Vec::with_capacity(object_count + 1);
    let mut targets = Vec::with_capacity(graph.edge_count());
    offsets.push(0);
    let mut processed_edges = 0usize;
    for object_index in 0..graph.objects.len() {
        check_cancel(cancel_flag)?;
        for reference_id in graph.references_for_index(object_index) {
            processed_edges = processed_edges.saturating_add(1);
            if let Some(to_index) = graph.object_index(*reference_id) {
                targets.push(hprof_node_id(to_index, "对象引用目标下标")?);
            }
            if should_report_work(processed_edges, graph.edge_count()) {
                report_work_progress(
                    reporter,
                    HprofAnalysisStage::BuildingDominatorGraph,
                    "正在构建紧凑引用图",
                    "添加对象引用边",
                    processed_edges as u64,
                    graph.edge_count() as u64,
                    "边",
                );
            }
        }
        offsets.push(hprof_node_id(targets.len(), "引用边偏移")?);
    }

    Ok(HprofCompactAdjacency {
        root_indices,
        offsets,
        targets,
    })
}

/// 根据对象图构建 dominator 结果。
fn build_hprof_dominator_result<F>(
    path: &Path,
    mut graph: HprofObjectGraph,
    cancel_flag: &AtomicBool,
    mut stage_reporter: F,
) -> Result<HprofDominatorResult, HprofError>
where
    F: FnMut(HprofAnalysisStage, &str, &str, u64, u64, &'static str),
{
    check_cancel(cancel_flag)?;
    report_work_progress(
        &mut stage_reporter,
        HprofAnalysisStage::BuildingDominatorGraph,
        "正在构建紧凑引用图",
        "添加紧凑图节点",
        0,
        graph.object_count() as u64,
        "对象",
    );
    let adjacency = build_compact_adjacency(&graph, cancel_flag, &mut stage_reporter)?;
    graph.clear_reference_pool();

    let dominator_indices = compute_lengauer_tarjan_dominators(
        adjacency,
        graph.object_count(),
        cancel_flag,
        &mut stage_reporter,
    )?;

    report_work_progress(
        &mut stage_reporter,
        HprofAnalysisStage::BuildingRows,
        "正在整理 dominator tree 结果",
        "构建紧凑子节点表",
        0,
        graph.object_count() as u64,
        "对象",
    );
    let mut compact_children = build_compact_children(
        &dominator_indices,
        graph.object_count(),
        cancel_flag,
        &mut stage_reporter,
    )?;
    report_work_progress(
        &mut stage_reporter,
        HprofAnalysisStage::BuildingRows,
        "正在计算 retained size",
        "后序聚合 retained size",
        0,
        dominator_indices
            .reachable
            .iter()
            .filter(|reachable| **reachable)
            .count() as u64,
        "对象",
    );
    let retained_sizes = compute_retained_sizes_by_index(
        &graph,
        &compact_children,
        cancel_flag,
        &mut stage_reporter,
    )?;
    let reachable_object_count = dominator_indices
        .reachable
        .iter()
        .filter(|is_reachable| **is_reachable)
        .count();
    let unreachable_shallow_size = graph
        .objects
        .iter()
        .enumerate()
        .filter(|(object_index, _)| !dominator_indices.reachable[*object_index])
        .map(|(_, object)| object.shallow_size)
        .sum::<u64>();
    let unreachable_object_count = graph.object_count().saturating_sub(reachable_object_count);
    let total_shallow_size = graph.total_shallow_size();
    let reachable_shallow_size = total_shallow_size.saturating_sub(unreachable_shallow_size);

    sort_compact_children(&graph, &retained_sizes, &mut compact_children);

    // MAT 的 dominator tree 顶层是虚拟 Root 的直接支配子节点，而不是全局 retained Top N。
    // 如果从所有可达对象里排序，会把父节点及其子节点同时放在顶层，例如 PooledBasedBackEnd、
    // AtomicReferenceArray 和 Object[] 会并列显示，破坏树结构语义。
    let mut top_indices = compact_children.root_children.clone();
    top_indices.sort_unstable_by(|left, right| {
        retained_sizes[*right]
            .cmp(&retained_sizes[*left])
            .then_with(|| graph.objects[*left].id.cmp(&graph.objects[*right].id))
    });
    top_indices.truncate(HPROF_TOP_DOMINATOR_LIMIT);
    let top_object_ids = top_indices
        .iter()
        .map(|object_index| graph.objects[*object_index].id)
        .collect::<Vec<_>>();

    let mut summaries = Vec::with_capacity(reachable_object_count);
    let mut object_to_summary_index = vec![None; graph.object_count()];
    let mut summary_object_indices = Vec::with_capacity(reachable_object_count);
    for (object_index, object) in graph.objects.iter().enumerate() {
        if !dominator_indices.reachable[object_index] {
            continue;
        }
        let retained_size = retained_sizes[object_index];
        let retained_percent = if reachable_shallow_size == 0 {
            0.0
        } else {
            retained_size as f32 * 100.0 / reachable_shallow_size as f32
        };
        let summary_index = summaries.len();
        object_to_summary_index[object_index] = Some(summary_index);
        summary_object_indices.push(object_index);
        summaries.push(HprofDominatorObjectSummary {
            object_id: object.id,
            class_id: object.class_id,
            kind: object.kind.clone(),
            shallow_size: object.shallow_size,
            retained_size,
            retained_percent,
            direct_child_count: compact_children.child_count(object_index),
        });
    }

    let mut child_offsets = Vec::with_capacity(summaries.len() + 1);
    let mut child_summary_indices = Vec::with_capacity(compact_children.indices.len());
    child_offsets.push(0);
    for object_index in summary_object_indices {
        for child_index in compact_children.children(object_index) {
            if let Some(child_summary_index) = object_to_summary_index[*child_index] {
                child_summary_indices.push(child_summary_index);
            }
        }
        child_offsets.push(child_summary_indices.len());
    }
    let top_summary_indices = top_indices
        .iter()
        .filter_map(|object_index| object_to_summary_index[*object_index])
        .collect::<Vec<_>>();
    let class_names = graph.class_names_for_result();
    let thread_details =
        build_thread_details(&graph, &retained_sizes, &dominator_indices.reachable);

    Ok(HprofDominatorResult {
        file_path: path.to_path_buf(),
        header: graph.header.clone(),
        total_objects: graph.object_count(),
        total_classes: graph.class_count(),
        gc_root_count: graph.gc_roots.len(),
        edge_count: graph.edge_count(),
        raw_edge_count: graph.raw_edge_count(),
        reference_edge_stats: graph.reference_edge_stats().clone(),
        synthetic_class_loader_edge_count: graph.synthetic_class_loader_edge_count(),
        synthetic_bootstrap_class_root_count: graph.synthetic_bootstrap_class_root_count(),
        total_shallow_size,
        reachable_shallow_size,
        reachable_object_count,
        unreachable_object_count,
        unreachable_shallow_size,
        size_model: graph.size_model.clone(),
        cache_status: None,
        top_object_ids,
        top_summary_indices,
        summary_storage: HprofSummaryStorage::Owned { summaries },
        class_names,
        thread_details,
        child_offsets,
        child_summary_indices,
    })
}

/// 根据解析阶段保留的线程记录构造线程详情索引。
///
/// 业务意图：
/// - 线程详情依赖对象字段、START_THREAD、ROOT_THREAD_OBJECT、STACK_TRACE、STACK_FRAME 和 dominator retained size；
///   在结果构造阶段统一拼装，可以避免 UI 层重复理解 HPROF 多张表之间的关联。
fn build_thread_details(
    graph: &HprofObjectGraph,
    retained_sizes: &[u64],
    reachable: &[bool],
) -> FxHashMap<HprofObjectId, HprofThreadDetails> {
    let mut details = FxHashMap::default();
    for (object_id, instance) in &graph.thread_instances {
        let Some(object_index) = graph.object_index(*object_id) else {
            continue;
        };
        if !reachable.get(object_index).copied().unwrap_or(false) {
            continue;
        }
        let object = &graph.objects[object_index];
        let class_name = graph
            .classes
            .get(&instance.class_id)
            .and_then(|class_info| class_info.name.clone())
            .unwrap_or_else(|| format!("<unknown class 0x{:x}>", instance.class_id));
        let (thread_serial, stack_trace_serial) =
            thread_serial_and_stack_for_object(graph, *object_id);
        let thread_name = thread_name_for_instance(graph, instance, thread_serial);
        let retained_size = retained_sizes.get(object_index).copied().unwrap_or(0);
        let stack_frames = stack_frames_for_thread(graph, thread_serial, stack_trace_serial);
        let stack_message = if stack_frames.is_empty() {
            Some("该 HPROF 未包含此线程的堆栈信息".to_string())
        } else {
            None
        };
        let properties = thread_properties_for_instance(
            graph,
            instance,
            &class_name,
            &thread_name,
            object.shallow_size,
            retained_size,
        );
        details.insert(
            *object_id,
            HprofThreadDetails {
                object_id: *object_id,
                class_name,
                thread_name,
                properties,
                stack_frames,
                stack_message,
            },
        );
    }
    details
}

/// 返回线程对象对应的 thread serial 和 stack trace serial。
fn thread_serial_and_stack_for_object(
    graph: &HprofObjectGraph,
    object_id: HprofObjectId,
) -> (Option<u32>, Option<u32>) {
    let serial = graph.thread_serial_by_object_id.get(&object_id).copied();
    let start_stack = serial
        .and_then(|serial| graph.thread_starts.get(&serial))
        .map(|record| record.stack_trace_serial)
        .filter(|serial| *serial != 0);
    let root_stack = graph
        .thread_object_roots
        .get(&object_id)
        .map(|record| record.stack_trace_serial)
        .filter(|serial| *serial != 0);
    (serial, start_stack.or(root_stack))
}

/// 返回线程名。
fn thread_name_for_instance(
    graph: &HprofObjectGraph,
    instance: &HprofThreadInstanceInfo,
    thread_serial: Option<u32>,
) -> String {
    if let Some(name) = thread_serial
        .and_then(|serial| graph.thread_starts.get(&serial))
        .and_then(|record| graph.strings.get(&record.thread_name_id))
    {
        return name.clone();
    }
    if let Some(name) = instance
        .name_object_id
        .and_then(|object_id| graph.java_string_values.get(&object_id))
    {
        return name.clone();
    }
    "<unknown>".to_string()
}

/// 构造线程属性表。
fn thread_properties_for_instance(
    graph: &HprofObjectGraph,
    instance: &HprofThreadInstanceInfo,
    class_name: &str,
    thread_name: &str,
    shallow_size: u64,
    retained_size: u64,
) -> Vec<HprofThreadProperty> {
    let state_value = instance
        .thread_status
        .map(|status| format!("0x{status:x}"))
        .unwrap_or_else(|| "<unknown>".to_string());
    vec![
        HprofThreadProperty {
            name: "Object / Stack Frame".to_string(),
            value: format!("{class_name} @ 0x{:x}", instance.object_id),
        },
        HprofThreadProperty {
            name: "Name".to_string(),
            value: thread_name.to_string(),
        },
        HprofThreadProperty {
            name: "Shallow Heap".to_string(),
            value: format_hprof_bytes(shallow_size),
        },
        HprofThreadProperty {
            name: "Retained Heap".to_string(),
            value: format_hprof_bytes(retained_size),
        },
        HprofThreadProperty {
            name: "Max. Locals' Retained Heap".to_string(),
            value: "暂未计算".to_string(),
        },
        HprofThreadProperty {
            name: "Context Class Loader".to_string(),
            value: instance
                .context_class_loader_id
                .map(|object_id| object_label(graph, object_id))
                .unwrap_or_else(|| "<unknown>".to_string()),
        },
        HprofThreadProperty {
            name: "Is Daemon".to_string(),
            value: instance
                .daemon
                .map(|daemon| daemon.to_string())
                .unwrap_or_else(|| "<unknown>".to_string()),
        },
        HprofThreadProperty {
            name: "Priority".to_string(),
            value: instance
                .priority
                .map(|priority| priority.to_string())
                .unwrap_or_else(|| "<unknown>".to_string()),
        },
        HprofThreadProperty {
            name: "State".to_string(),
            value: instance
                .thread_status
                .map(describe_thread_status)
                .unwrap_or_else(|| "<unknown>".to_string()),
        },
        HprofThreadProperty {
            name: "State value".to_string(),
            value: state_value,
        },
    ]
}

/// 返回对象的 MAT 风格简短展示名。
fn object_label(graph: &HprofObjectGraph, object_id: HprofObjectId) -> String {
    let Some(object_index) = graph.object_index(object_id) else {
        return format!("0x{object_id:x}");
    };
    let object = &graph.objects[object_index];
    let class_name = match &object.kind {
        HprofObjectKind::Class => graph
            .classes
            .get(&object.id)
            .and_then(|class_info| class_info.name.as_deref())
            .map(|name| format!("Class {name}"))
            .unwrap_or_else(|| "Class".to_string()),
        HprofObjectKind::PrimitiveArray { element_type, .. } => {
            format!("{}[]", primitive_type_label(*element_type))
        }
        HprofObjectKind::ObjectArray { .. } | HprofObjectKind::Instance => graph
            .classes
            .get(&object.class_id)
            .and_then(|class_info| class_info.name.clone())
            .unwrap_or_else(|| format!("<unknown class 0x{:x}>", object.class_id)),
    };
    format!("{class_name} @ 0x{object_id:x}")
}

/// 根据 HotSpot/MAT 常见位标记解码线程状态。
///
/// 边界条件：
/// - 不同 JVM 版本可能保留额外状态位；未知位会保留十六进制，避免误导用户。
fn describe_thread_status(status: u32) -> String {
    let mut labels = Vec::new();
    let known_flags = [
        (0x0001, "alive"),
        (0x0002, "terminated"),
        (0x0004, "runnable"),
        (0x0010, "waiting indefinitely"),
        (0x0020, "waiting with timeout"),
        (0x0040, "sleeping"),
        (0x0080, "waiting"),
        (0x0100, "in object wait"),
        (0x0200, "parked"),
        (0x0400, "blocked on monitor enter"),
        (0x100000, "suspended"),
        (0x200000, "interrupted"),
        (0x400000, "in native"),
    ];
    let mut known_mask = 0u32;
    for (flag, label) in known_flags {
        known_mask |= flag;
        if status & flag != 0 {
            labels.push(label.to_string());
        }
    }
    let unknown = status & !known_mask;
    if unknown != 0 {
        labels.push(format!("unknown bits 0x{unknown:x}"));
    }
    if labels.is_empty() {
        "unknown".to_string()
    } else {
        labels.join(", ")
    }
}

/// 返回线程栈帧列表。
fn stack_frames_for_thread(
    graph: &HprofObjectGraph,
    thread_serial: Option<u32>,
    stack_trace_serial: Option<u32>,
) -> Vec<HprofThreadStackFrame> {
    let stack_trace = stack_trace_serial
        .and_then(|serial| graph.stack_traces.get(&serial))
        .or_else(|| {
            thread_serial.and_then(|thread_serial| {
                graph
                    .stack_traces
                    .values()
                    .find(|trace| trace.thread_serial == thread_serial)
            })
        });
    let Some(stack_trace) = stack_trace else {
        return Vec::new();
    };
    stack_trace
        .frame_ids
        .iter()
        .filter_map(|frame_id| graph.stack_frames.get(frame_id))
        .map(|frame| stack_frame_for_record(graph, frame))
        .collect()
}

/// 把原始 STACK_FRAME 记录转换成 UI 栈帧。
fn stack_frame_for_record(
    graph: &HprofObjectGraph,
    frame: &HprofStackFrameRecord,
) -> HprofThreadStackFrame {
    let class_name = graph
        .loaded_classes
        .get(&frame.class_serial)
        .and_then(|(class_id, _)| graph.classes.get(class_id))
        .and_then(|class_info| class_info.name.clone())
        .or_else(|| {
            graph
                .loaded_classes
                .get(&frame.class_serial)
                .and_then(|(_, name_id)| graph.strings.get(name_id))
                .map(|name| normalize_java_class_name(name))
        })
        .unwrap_or_else(|| format!("<unknown class serial {}>", frame.class_serial));
    let method_name = graph
        .strings
        .get(&frame.method_name_id)
        .cloned()
        .unwrap_or_else(|| "<unknown>".to_string());
    let method_signature = graph
        .strings
        .get(&frame.method_signature_id)
        .cloned()
        .unwrap_or_default();
    let source_file = graph
        .strings
        .get(&frame.source_file_id)
        .cloned()
        .unwrap_or_else(|| "Unknown Source".to_string());
    let source = stack_frame_source_label(&source_file, frame.line_number);
    let display = format!("at {class_name}.{method_name}{method_signature} ({source})");
    HprofThreadStackFrame {
        class_name,
        method_name,
        method_signature,
        source,
        line_number: frame.line_number,
        display,
    }
}

/// 返回栈帧源文件和行号展示文案。
fn stack_frame_source_label(source_file: &str, line_number: i32) -> String {
    match line_number {
        -3 => "Native Method".to_string(),
        -2 => "Compiled Method".to_string(),
        line if line > 0 => format!("{source_file}:{line}"),
        _ => "Unknown Source".to_string(),
    }
}

/// 使用 Lengauer-Tarjan 算法计算对象下标级 immediate dominator。
fn compute_lengauer_tarjan_dominators<F>(
    adjacency: HprofCompactAdjacency,
    object_count: usize,
    cancel_flag: &AtomicBool,
    reporter: &mut F,
) -> Result<HprofDominatorIndexResult, HprofError>
where
    F: FnMut(HprofAnalysisStage, &str, &str, u64, u64, &'static str),
{
    let node_count = object_count.saturating_add(1);
    let _ = hprof_node_id(object_count, "LT 节点数量")?;
    report_work_progress(
        reporter,
        HprofAnalysisStage::ComputingDominatorTree,
        "正在运行 Lengauer-Tarjan dominator 算法",
        "DFS 编号",
        0,
        node_count as u64,
        "节点",
    );

    let mut dfn = vec![0 as HprofNodeId; node_count];
    let mut vertex = vec![HPROF_INVALID_NODE];
    let mut parent = vec![HPROF_INVALID_NODE; node_count];
    let mut semi = vec![0 as HprofNodeId; node_count];
    let mut stack = vec![(0 as HprofNodeId, 0usize)];
    dfn[0] = 1;
    semi[0] = 1;
    vertex.push(0);

    while let Some((node, next_child)) = stack.last_mut() {
        let child_count = lt_neighbor_count(&adjacency, *node);
        if *next_child < child_count {
            let child = lt_neighbor_at(&adjacency, *node, *next_child);
            let child_index = hprof_node_index(child);
            *next_child += 1;
            if dfn[child_index] == 0 {
                parent[child_index] = *node;
                let number = hprof_node_id(vertex.len(), "DFS 编号")?;
                dfn[child_index] = number;
                semi[child_index] = number;
                vertex.push(child);
                if should_report_work(hprof_node_index(number), node_count) {
                    report_work_progress(
                        reporter,
                        HprofAnalysisStage::ComputingDominatorTree,
                        "正在运行 Lengauer-Tarjan dominator 算法",
                        "DFS 编号",
                        u64::from(number),
                        node_count as u64,
                        "节点",
                    );
                    check_cancel(cancel_flag)?;
                }
                stack.push((child, 0));
            }
        } else {
            stack.pop();
        }
    }

    let reachable_node_count = vertex.len().saturating_sub(1);
    let reachable_non_root_count = reachable_node_count.saturating_sub(1);
    report_work_progress(
        reporter,
        HprofAnalysisStage::ComputingDominatorTree,
        "正在运行 Lengauer-Tarjan dominator 算法",
        "构建前驱索引",
        0,
        adjacency
            .targets
            .len()
            .saturating_add(adjacency.root_indices.len()) as u64,
        "边",
    );
    let (predecessor_offsets, predecessors) =
        build_lengauer_tarjan_predecessors(&adjacency, &dfn, cancel_flag, reporter)?;
    // adjacency 在前驱表构建后不再使用，显式释放 CSR 目标数组，降低 LT 主循环峰值内存。
    drop(adjacency);

    report_work_progress(
        reporter,
        HprofAnalysisStage::ComputingDominatorTree,
        "正在运行 Lengauer-Tarjan dominator 算法",
        "LT 主循环",
        0,
        reachable_non_root_count as u64,
        "节点",
    );

    let mut ancestor = vec![HPROF_INVALID_NODE; node_count];
    let mut label = (0..node_count)
        .map(|index| hprof_node_id(index, "LT label 初始化"))
        .collect::<Result<Vec<_>, _>>()?;
    let mut bucket_head = vec![HPROF_INVALID_NODE; node_count];
    let mut bucket_next = vec![HPROF_INVALID_NODE; node_count];
    let mut idom = vec![HPROF_INVALID_NODE; node_count];
    let mut eval_path = Vec::new();

    for reverse_index in (2..=reachable_node_count).rev() {
        check_cancel(cancel_flag)?;
        let w = vertex[reverse_index];
        let w_index = hprof_node_index(w);
        let predecessor_start = hprof_node_index(predecessor_offsets[w_index]);
        let predecessor_end = hprof_node_index(predecessor_offsets[w_index + 1]);
        for v in &predecessors[predecessor_start..predecessor_end] {
            let u = lengauer_tarjan_eval(*v, &mut ancestor, &mut label, &semi, &mut eval_path);
            let u_index = hprof_node_index(u);
            if semi[u_index] < semi[w_index] {
                semi[w_index] = semi[u_index];
            }
        }

        let semi_vertex = vertex[hprof_node_index(semi[w_index])];
        bucket_next[w_index] = bucket_head[hprof_node_index(semi_vertex)];
        bucket_head[hprof_node_index(semi_vertex)] = w;
        ancestor[w_index] = parent[w_index];

        let parent_w = parent[w_index];
        let parent_w_index = hprof_node_index(parent_w);
        let mut bucket_node = bucket_head[parent_w_index];
        bucket_head[parent_w_index] = HPROF_INVALID_NODE;
        while bucket_node != HPROF_INVALID_NODE {
            let v = bucket_node;
            let v_index = hprof_node_index(v);
            bucket_node = bucket_next[v_index];
            bucket_next[v_index] = HPROF_INVALID_NODE;
            let u = lengauer_tarjan_eval(v, &mut ancestor, &mut label, &semi, &mut eval_path);
            idom[v_index] = if semi[hprof_node_index(u)] < semi[v_index] {
                u
            } else {
                parent_w
            };
        }

        let done = reachable_node_count - reverse_index + 1;
        if should_report_work(done, reachable_non_root_count) {
            report_work_progress(
                reporter,
                HprofAnalysisStage::ComputingDominatorTree,
                "正在运行 Lengauer-Tarjan dominator 算法",
                "LT 主循环",
                done as u64,
                reachable_non_root_count as u64,
                "节点",
            );
        }
    }

    report_work_progress(
        reporter,
        HprofAnalysisStage::ComputingDominatorTree,
        "正在运行 Lengauer-Tarjan dominator 算法",
        "修正 immediate dominator",
        0,
        reachable_non_root_count as u64,
        "节点",
    );
    for index in 2..=reachable_node_count {
        let w = vertex[index];
        let w_index = hprof_node_index(w);
        if idom[w_index] != vertex[hprof_node_index(semi[w_index])] {
            idom[w_index] = idom[hprof_node_index(idom[w_index])];
        }
        if should_report_work(index - 1, reachable_non_root_count) {
            report_work_progress(
                reporter,
                HprofAnalysisStage::ComputingDominatorTree,
                "正在运行 Lengauer-Tarjan dominator 算法",
                "修正 immediate dominator",
                (index - 1) as u64,
                reachable_non_root_count as u64,
                "节点",
            );
            check_cancel(cancel_flag)?;
        }
    }
    idom[0] = 0;
    drop(semi);
    drop(ancestor);
    drop(label);
    drop(predecessor_offsets);
    drop(predecessors);

    let mut reachable = vec![false; object_count];
    let mut parent_by_index = vec![None; object_count];
    for object_index in 0..object_count {
        let node = object_index + 1;
        if dfn[node] == 0 {
            continue;
        }
        reachable[object_index] = true;
        if idom[node] != 0 && idom[node] != HPROF_INVALID_NODE {
            parent_by_index[object_index] = Some(hprof_node_index(idom[node] - 1));
        }
    }

    Ok(HprofDominatorIndexResult {
        reachable,
        parent_by_index,
    })
}

/// 返回 LT 节点的出边数量。
fn lt_neighbor_count(adjacency: &HprofCompactAdjacency, node: HprofNodeId) -> usize {
    if node == 0 {
        adjacency.root_indices.len()
    } else {
        let object_index = hprof_node_index(node - 1);
        hprof_node_index(adjacency.offsets[object_index + 1] - adjacency.offsets[object_index])
    }
}

/// 返回 LT 节点的第 `neighbor_index` 个出边目标节点。
fn lt_neighbor_at(
    adjacency: &HprofCompactAdjacency,
    node: HprofNodeId,
    neighbor_index: usize,
) -> HprofNodeId {
    if node == 0 {
        adjacency.root_indices[neighbor_index] + 1
    } else {
        let object_index = hprof_node_index(node - 1);
        adjacency.targets[hprof_node_index(adjacency.offsets[object_index]) + neighbor_index] + 1
    }
}

/// 构建 LT 算法需要的紧凑前驱表。
fn build_lengauer_tarjan_predecessors<F>(
    adjacency: &HprofCompactAdjacency,
    dfn: &[HprofNodeId],
    cancel_flag: &AtomicBool,
    reporter: &mut F,
) -> Result<(Vec<HprofNodeId>, Vec<HprofNodeId>), HprofError>
where
    F: FnMut(HprofAnalysisStage, &str, &str, u64, u64, &'static str),
{
    let node_count = dfn.len();
    let total_edges = adjacency
        .root_indices
        .len()
        .saturating_add(adjacency.targets.len());
    let mut predecessor_counts = vec![0 as HprofNodeId; node_count];
    let mut processed_edges = 0usize;

    for root_index in &adjacency.root_indices {
        processed_edges = processed_edges.saturating_add(1);
        let target = hprof_node_index(root_index + 1);
        if dfn[target] != 0 {
            predecessor_counts[target] =
                predecessor_counts[target].checked_add(1).ok_or_else(|| {
                    HprofError::Unsupported("单节点前驱数量超过 u32 上限".to_string())
                })?;
        }
        if should_report_work(processed_edges, total_edges) {
            report_work_progress(
                reporter,
                HprofAnalysisStage::ComputingDominatorTree,
                "正在运行 Lengauer-Tarjan dominator 算法",
                "构建前驱索引",
                processed_edges as u64,
                total_edges as u64,
                "边",
            );
            check_cancel(cancel_flag)?;
        }
    }

    for object_index in 0..adjacency.offsets.len().saturating_sub(1) {
        let from = object_index + 1;
        let start = hprof_node_index(adjacency.offsets[object_index]);
        let end = hprof_node_index(adjacency.offsets[object_index + 1]);
        for target_index in &adjacency.targets[start..end] {
            processed_edges = processed_edges.saturating_add(1);
            let target = hprof_node_index(*target_index + 1);
            if dfn[from] != 0 && dfn[target] != 0 {
                predecessor_counts[target] =
                    predecessor_counts[target].checked_add(1).ok_or_else(|| {
                        HprofError::Unsupported("单节点前驱数量超过 u32 上限".to_string())
                    })?;
            }
            if should_report_work(processed_edges, total_edges) {
                report_work_progress(
                    reporter,
                    HprofAnalysisStage::ComputingDominatorTree,
                    "正在运行 Lengauer-Tarjan dominator 算法",
                    "构建前驱索引",
                    processed_edges as u64,
                    total_edges as u64,
                    "边",
                );
                check_cancel(cancel_flag)?;
            }
        }
    }

    let mut offsets = Vec::with_capacity(node_count + 1);
    offsets.push(0 as HprofNodeId);
    for count in &predecessor_counts {
        let next = offsets
            .last()
            .copied()
            .unwrap_or(0)
            .checked_add(*count)
            .ok_or_else(|| HprofError::Unsupported("前驱表大小超过 u32 上限".to_string()))?;
        offsets.push(next);
    }
    let mut predecessors =
        vec![0 as HprofNodeId; hprof_node_index(offsets.last().copied().unwrap_or(0))];
    let mut write_offsets = offsets.clone();

    for root_index in &adjacency.root_indices {
        let target = hprof_node_index(root_index + 1);
        if dfn[target] != 0 {
            let position = hprof_node_index(write_offsets[target]);
            predecessors[position] = 0;
            write_offsets[target] += 1;
        }
    }
    for object_index in 0..adjacency.offsets.len().saturating_sub(1) {
        let from = object_index + 1;
        if dfn[from] == 0 {
            continue;
        }
        let start = hprof_node_index(adjacency.offsets[object_index]);
        let end = hprof_node_index(adjacency.offsets[object_index + 1]);
        for target_index in &adjacency.targets[start..end] {
            let target = hprof_node_index(*target_index + 1);
            if dfn[target] != 0 {
                let position = hprof_node_index(write_offsets[target]);
                predecessors[position] = hprof_node_id(from, "前驱节点下标")?;
                write_offsets[target] += 1;
            }
        }
    }

    Ok((offsets, predecessors))
}

/// LT eval 操作，使用迭代压缩避免大图递归栈溢出。
fn lengauer_tarjan_eval(
    node: HprofNodeId,
    ancestor: &mut [HprofNodeId],
    label: &mut [HprofNodeId],
    semi: &[HprofNodeId],
    scratch_path: &mut Vec<HprofNodeId>,
) -> HprofNodeId {
    let node_index = hprof_node_index(node);
    if ancestor[node_index] == HPROF_INVALID_NODE {
        return label[node_index];
    }
    lengauer_tarjan_compress(node, ancestor, label, semi, scratch_path);
    label[node_index]
}

/// LT 路径压缩。
///
/// 性能约束：
/// - 该函数位于 LT 主循环最热路径，会按前驱边调用；scratch 由外层复用，避免每次压缩都分配新的 `Vec`。
fn lengauer_tarjan_compress(
    node: HprofNodeId,
    ancestor: &mut [HprofNodeId],
    label: &mut [HprofNodeId],
    semi: &[HprofNodeId],
    scratch_path: &mut Vec<HprofNodeId>,
) {
    scratch_path.clear();
    let mut current = node;
    while ancestor[hprof_node_index(current)] != HPROF_INVALID_NODE
        && ancestor[hprof_node_index(ancestor[hprof_node_index(current)])] != HPROF_INVALID_NODE
    {
        scratch_path.push(current);
        current = ancestor[hprof_node_index(current)];
    }
    for item in scratch_path.iter().rev().copied() {
        let item_index = hprof_node_index(item);
        let ancestor_item = ancestor[item_index];
        let ancestor_index = hprof_node_index(ancestor_item);
        if semi[hprof_node_index(label[ancestor_index])] < semi[hprof_node_index(label[item_index])]
        {
            label[item_index] = label[ancestor_index];
        }
        ancestor[item_index] = ancestor[ancestor_index];
    }
}

/// 收集所有继承自指定目标类名的 class object ID。
///
/// 业务意图：
/// - HPROF 物化阶段会扫描每个实例；把 String/Thread 的继承判断提前到类粒度，可以把热路径从“每对象走继承链”降到“每类走继承链”。
/// - 类数量通常远小于对象数量，且 class metadata 已经解析完毕，因此这里用一次性集合换取大 dump 中稳定的解析速度。
fn collect_subclass_ids(
    raw_classes: &FxHashMap<HprofObjectId, RawClassInfo>,
    classes: &FxHashMap<HprofObjectId, HprofClassInfo>,
    target_class_name: &str,
) -> FxHashSet<HprofObjectId> {
    raw_classes
        .keys()
        .copied()
        .filter(|class_id| is_subclass_of(raw_classes, classes, *class_id, target_class_name))
        .collect()
}

/// 根据 idom 结果构建紧凑子节点表。
fn build_compact_children<F>(
    dominators: &HprofDominatorIndexResult,
    object_count: usize,
    cancel_flag: &AtomicBool,
    reporter: &mut F,
) -> Result<HprofCompactChildren, HprofError>
where
    F: FnMut(HprofAnalysisStage, &str, &str, u64, u64, &'static str),
{
    let mut child_counts = vec![0usize; object_count];
    let mut root_children = Vec::new();
    for (object_index, reachable) in dominators.reachable.iter().enumerate() {
        if !*reachable {
            continue;
        }
        if let Some(parent_index) = dominators.parent_by_index[object_index] {
            child_counts[parent_index] = child_counts[parent_index].saturating_add(1);
        } else {
            root_children.push(object_index);
        }
        if should_report_work(object_index + 1, object_count) {
            report_work_progress(
                reporter,
                HprofAnalysisStage::BuildingRows,
                "正在整理 dominator tree 结果",
                "统计直接支配子节点",
                (object_index + 1) as u64,
                object_count as u64,
                "对象",
            );
            check_cancel(cancel_flag)?;
        }
    }

    let mut offsets = Vec::with_capacity(object_count + 1);
    offsets.push(0);
    for count in &child_counts {
        offsets.push(offsets.last().copied().unwrap_or(0) + *count);
    }
    let mut indices = vec![0usize; offsets.last().copied().unwrap_or(0)];
    let mut write_offsets = offsets.clone();
    for (object_index, parent_index) in dominators.parent_by_index.iter().enumerate() {
        if !dominators
            .reachable
            .get(object_index)
            .copied()
            .unwrap_or(false)
        {
            continue;
        }
        if let Some(parent_index) = *parent_index {
            let position = write_offsets[parent_index];
            indices[position] = object_index;
            write_offsets[parent_index] += 1;
        }
    }

    Ok(HprofCompactChildren {
        offsets,
        indices,
        root_children,
    })
}

/// 按 retained size 排序紧凑子节点表。
fn sort_compact_children(
    graph: &HprofObjectGraph,
    retained_sizes: &[u64],
    compact_children: &mut HprofCompactChildren,
) {
    for object_index in 0..graph.object_count() {
        let start = compact_children.offsets[object_index];
        let end = compact_children.offsets[object_index + 1];
        compact_children.indices[start..end].sort_by(|left, right| {
            retained_sizes[*right]
                .cmp(&retained_sizes[*left])
                .then_with(|| graph.objects[*left].id.cmp(&graph.objects[*right].id))
        });
    }
}

/// 按连续对象下标后序遍历 dominator tree 计算 retained size。
fn compute_retained_sizes_by_index<F>(
    graph: &HprofObjectGraph,
    compact_children: &HprofCompactChildren,
    cancel_flag: &AtomicBool,
    reporter: &mut F,
) -> Result<Vec<u64>, HprofError>
where
    F: FnMut(HprofAnalysisStage, &str, &str, u64, u64, &'static str),
{
    let mut retained_sizes = vec![0; graph.object_count()];
    let mut processed = 0usize;
    let total = compact_children
        .root_children
        .iter()
        .map(|root| retained_subtree_size(*root, compact_children))
        .sum::<usize>();
    let mut stack = compact_children
        .root_children
        .iter()
        .rev()
        .map(|object_index| (*object_index, false))
        .collect::<Vec<_>>();
    while let Some((object_id, visited)) = stack.pop() {
        if visited {
            let child_sum = compact_children
                .children(object_id)
                .iter()
                .map(|child_id| retained_sizes[*child_id])
                .sum::<u64>();
            retained_sizes[object_id] = graph.objects[object_id].shallow_size + child_sum;
            processed = processed.saturating_add(1);
            if should_report_work(processed, total) {
                report_work_progress(
                    reporter,
                    HprofAnalysisStage::BuildingRows,
                    "正在计算 retained size",
                    "后序聚合 retained size",
                    processed as u64,
                    total as u64,
                    "对象",
                );
                check_cancel(cancel_flag)?;
            }
        } else {
            stack.push((object_id, true));
            for child_id in compact_children.children(object_id) {
                stack.push((*child_id, false));
            }
        }
    }
    Ok(retained_sizes)
}

/// 统计 retained 聚合需要访问的子树对象数量。
fn retained_subtree_size(root: usize, compact_children: &HprofCompactChildren) -> usize {
    let mut count = 0usize;
    let mut stack = vec![root];
    while let Some(object_index) = stack.pop() {
        count = count.saturating_add(1);
        for child in compact_children.children(object_index) {
            stack.push(*child);
        }
    }
    count
}

/// 解析继承后的字段布局。
#[cfg(test)]
fn resolve_layout(
    raw_classes: &FxHashMap<HprofObjectId, RawClassInfo>,
    layout_cache: &mut FxHashMap<HprofObjectId, Vec<RawFieldDescriptor>>,
    class_id: HprofObjectId,
) -> Option<Vec<RawFieldDescriptor>> {
    if let Some(layout) = layout_cache.get(&class_id) {
        return Some(layout.clone());
    }
    let raw_class = raw_classes.get(&class_id)?;
    let mut fields = Vec::new();
    fields.extend(raw_class.instance_fields.clone());
    if raw_class.super_class_id != 0
        && let Some(super_fields) =
            resolve_layout(raw_classes, layout_cache, raw_class.super_class_id)
    {
        fields.extend(super_fields);
    }
    layout_cache.insert(class_id, fields.clone());
    Some(fields)
}

/// 解析继承后的对象引用字段偏移和 MAT 引用强度。
fn resolve_reference_layout(
    raw_classes: &FxHashMap<HprofObjectId, RawClassInfo>,
    classes: &FxHashMap<HprofObjectId, HprofClassInfo>,
    strings: &FxHashMap<HprofObjectId, String>,
    layout_cache: &mut FxHashMap<HprofObjectId, ResolvedReferenceLayout>,
    class_id: HprofObjectId,
    identifier_size: u8,
    size_model: &HprofSizeModel,
) -> Option<ResolvedReferenceLayout> {
    if let Some(layout) = layout_cache.get(&class_id) {
        return Some(layout.clone());
    }

    let mut fields = Vec::new();
    collect_declared_fields(raw_classes, class_id, &mut fields)?;
    let mut layout = ResolvedReferenceLayout {
        reference_fields: Vec::new(),
        hprof_byte_size: 0,
        heap_field_byte_size: 0,
    };

    for (declaring_class_id, field) in fields {
        let width = field_value_size(field.field_type, identifier_size)? as usize;
        let heap_width = u64::from(field_value_size(
            field.field_type,
            size_model.reference_size,
        )?);
        if field.field_type == FIELD_TYPE_OBJECT {
            layout.reference_fields.push(ResolvedReferenceField {
                offset: layout.hprof_byte_size,
                strength: reference_strength_for_field(
                    raw_classes,
                    classes,
                    strings,
                    class_id,
                    declaring_class_id,
                    field.name_id,
                ),
            });
        }
        layout.hprof_byte_size = layout.hprof_byte_size.saturating_add(width);
        layout.heap_field_byte_size = layout.heap_field_byte_size.saturating_add(heap_width);
    }

    layout_cache.insert(class_id, layout.clone());
    Some(layout)
}

/// 按继承顺序收集字段及其声明类。
fn collect_declared_fields(
    raw_classes: &FxHashMap<HprofObjectId, RawClassInfo>,
    class_id: HprofObjectId,
    fields: &mut Vec<(HprofObjectId, RawFieldDescriptor)>,
) -> Option<()> {
    let raw_class = raw_classes.get(&class_id)?;
    // HPROF INSTANCE_DUMP 的字段值顺序是“当前类字段，然后父类字段，再父类的父类字段”。
    // 这里必须和文件中的字节顺序一致，否则子类 primitive 字段会被当成父类 object 字段读取，
    // 导致大量真实引用边丢失，进而把对象误判为不可达。
    for field in &raw_class.instance_fields {
        fields.push((class_id, field.clone()));
    }
    if raw_class.super_class_id != 0 {
        let _ = collect_declared_fields(raw_classes, raw_class.super_class_id, fields);
    }
    Some(())
}

/// 返回字段在 MAT 兼容对象图中的引用强度。
fn reference_strength_for_field(
    raw_classes: &FxHashMap<HprofObjectId, RawClassInfo>,
    classes: &FxHashMap<HprofObjectId, HprofClassInfo>,
    strings: &FxHashMap<HprofObjectId, String>,
    receiver_class_id: HprofObjectId,
    declaring_class_id: HprofObjectId,
    field_name_id: HprofObjectId,
) -> HprofReferenceStrength {
    let Some(field_name) = strings.get(&field_name_id) else {
        return HprofReferenceStrength::Strong;
    };
    if field_name != "referent" {
        return HprofReferenceStrength::Strong;
    }
    let declaring_class_name = classes
        .get(&declaring_class_id)
        .and_then(|class_info| class_info.name.as_deref());
    if declaring_class_name != Some("java.lang.ref.Reference") {
        return HprofReferenceStrength::Strong;
    }

    for (class_name, strength) in [
        (
            "java.lang.ref.WeakReference",
            HprofReferenceStrength::WeakLike,
        ),
        (
            "java.lang.ref.SoftReference",
            HprofReferenceStrength::SoftLike,
        ),
        (
            "java.lang.ref.PhantomReference",
            HprofReferenceStrength::PhantomLike,
        ),
        (
            "java.lang.ref.FinalReference",
            HprofReferenceStrength::FinalizerLike,
        ),
    ] {
        if is_subclass_of(raw_classes, classes, receiver_class_id, class_name) {
            return strength;
        }
    }
    HprofReferenceStrength::Strong
}

/// 判断指定类是否继承自目标类名。
fn is_subclass_of(
    raw_classes: &FxHashMap<HprofObjectId, RawClassInfo>,
    classes: &FxHashMap<HprofObjectId, HprofClassInfo>,
    mut class_id: HprofObjectId,
    target_class_name: &str,
) -> bool {
    let mut visited = FxHashSet::default();
    while class_id != 0 && visited.insert(class_id) {
        if classes
            .get(&class_id)
            .and_then(|class_info| class_info.name.as_deref())
            == Some(target_class_name)
        {
            return true;
        }
        class_id = raw_classes
            .get(&class_id)
            .map(|raw_class| raw_class.super_class_id)
            .unwrap_or(0);
    }
    false
}

/// 计算普通实例对象的 MAT 兼容 shallow size。
fn instance_shallow_size(size_model: &HprofSizeModel, field_bytes: u64) -> u64 {
    size_model.align_size(size_model.object_header_size.saturating_add(field_bytes))
}

/// 计算对象数组的 MAT 兼容 shallow size。
fn object_array_shallow_size(size_model: &HprofSizeModel, length: u32) -> u64 {
    size_model.align_size(
        size_model
            .array_header_size
            .saturating_add(u64::from(length) * u64::from(size_model.reference_size)),
    )
}

/// 计算基础类型数组的 MAT 兼容 shallow size。
fn primitive_array_shallow_size(
    size_model: &HprofSizeModel,
    element_type: u8,
    length: u64,
) -> Result<u64, HprofError> {
    let width = field_value_size(element_type, size_model.reference_size).ok_or_else(|| {
        HprofError::InvalidFormat(format!("未知 primitive array 元素类型：{element_type}"))
    })?;
    Ok(size_model.align_size(
        size_model
            .array_header_size
            .saturating_add(length.saturating_mul(u64::from(width))),
    ))
}

/// 按指定对齐粒度向上取整。
fn align_size(size: u64, alignment: u64) -> u64 {
    if alignment == 0 {
        return size;
    }
    let remainder = size % alignment;
    if remainder == 0 {
        size
    } else {
        size.saturating_add(alignment - remainder)
    }
}

/// 返回字段类型对应字节宽度。
fn field_value_size(field_type: u8, identifier_size: u8) -> Option<u8> {
    match field_type {
        FIELD_TYPE_OBJECT => Some(identifier_size),
        FIELD_TYPE_BOOLEAN | FIELD_TYPE_BYTE => Some(1),
        FIELD_TYPE_CHAR | FIELD_TYPE_SHORT => Some(2),
        FIELD_TYPE_FLOAT | FIELD_TYPE_INT => Some(4),
        FIELD_TYPE_DOUBLE | FIELD_TYPE_LONG => Some(8),
        _ => None,
    }
}

/// 从内存切片读取对象 ID。
fn read_id_from_slice(bytes: &[u8]) -> HprofObjectId {
    bytes
        .iter()
        .fold(0u64, |value, byte| (value << 8) | u64::from(*byte))
}

/// 从 4 字节切片读取大端 `u32`。
fn read_u32_from_slice(bytes: &[u8]) -> u32 {
    if bytes.len() < 4 {
        return 0;
    }
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// 从 4 字节切片读取大端 `i32`。
fn read_i32_from_slice(bytes: &[u8]) -> i32 {
    if bytes.len() < 4 {
        return 0;
    }
    i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// 解码 HPROF `char[]` 中保存的 Java String。
fn decode_char_array_string(
    data: &[u8],
    length: u32,
    offset: Option<usize>,
    count: Option<usize>,
) -> Option<String> {
    let total_chars = length as usize;
    let start = offset.unwrap_or(0).min(total_chars);
    let char_count = count.unwrap_or(total_chars.saturating_sub(start));
    let end = start.saturating_add(char_count).min(total_chars);
    let mut utf16 = Vec::with_capacity(end.saturating_sub(start));
    for index in start..end {
        let byte_offset = index.checked_mul(2)?;
        let bytes = data.get(byte_offset..byte_offset + 2)?;
        utf16.push(u16::from_be_bytes([bytes[0], bytes[1]]));
    }
    String::from_utf16(&utf16).ok()
}

/// 解码 HPROF `byte[]` 中保存的 Java compact string。
fn decode_byte_array_string(
    data: &[u8],
    length: u32,
    coder: Option<u8>,
    offset: Option<usize>,
    count: Option<usize>,
) -> Option<String> {
    let total_bytes = length as usize;
    let start = offset.unwrap_or(0).min(total_bytes);
    let byte_count = count.unwrap_or(total_bytes.saturating_sub(start));
    let end = start.saturating_add(byte_count).min(total_bytes);
    let bytes = data.get(start..end)?;
    match coder.unwrap_or(0) {
        0 => Some(bytes.iter().map(|byte| char::from(*byte)).collect()),
        1 => {
            let mut utf16 = Vec::with_capacity(bytes.len() / 2);
            for pair in bytes.chunks_exact(2) {
                utf16.push(u16::from_be_bytes([pair[0], pair[1]]));
            }
            String::from_utf16(&utf16).ok()
        }
        _ => None,
    }
}

/// 向引用列表追加非零对象 ID。
fn push_non_zero_reference(references: &mut Vec<HprofObjectId>, reference_id: HprofObjectId) {
    if reference_id != 0 {
        references.push(reference_id);
    }
}

/// 对单个对象的引用列表做原地去重。
fn normalize_references(references: &mut Vec<HprofObjectId>) {
    if references.len() <= 1 {
        return;
    }
    references.sort_unstable();
    references.dedup();
}

/// 标准化 Java 类名。
fn normalize_java_class_name(name: &str) -> String {
    name.replace('/', ".")
}

/// 返回基础类型展示名。
fn primitive_type_label(element_type: u8) -> &'static str {
    match element_type {
        FIELD_TYPE_BOOLEAN => "boolean",
        FIELD_TYPE_CHAR => "char",
        FIELD_TYPE_FLOAT => "float",
        FIELD_TYPE_DOUBLE => "double",
        FIELD_TYPE_BYTE => "byte",
        FIELD_TYPE_SHORT => "short",
        FIELD_TYPE_INT => "int",
        FIELD_TYPE_LONG => "long",
        _ => "unknown",
    }
}

/// 格式化字节数。
///
/// 业务意图：
/// - UI 和测试都需要稳定的人类可读大小，集中实现避免每个窗口重复定义单位换算。
pub(crate) fn format_hprof_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bytes_f64 = bytes as f64;
    if bytes_f64 >= GIB {
        format!("{:.2} GiB", bytes_f64 / GIB)
    } else if bytes_f64 >= MIB {
        format!("{:.2} MiB", bytes_f64 / MIB)
    } else if bytes_f64 >= KIB {
        format!("{:.2} KiB", bytes_f64 / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    //! HPROF 解析和 dominator tree 纯逻辑测试。
    //!
    //! 业务意图：
    //! - 这些测试不依赖 GPUI 窗口系统，直接锁定文件校验、对象引用解析和 retained size 规则。
    //! - 真实生产 dump 通常很大，单元测试使用最小合法 HPROF fixture 覆盖边界行为。

    use super::*;
    use std::{env, fs};

    /// 构造唯一临时文件路径。
    fn test_file_path(name: &str, extension: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "logclinic3-hprof-test-{}-{}.{}",
            std::process::id(),
            name,
            extension
        ))
    }

    /// 写入测试文件并返回路径。
    fn write_test_file(name: &str, extension: &str, bytes: &[u8]) -> PathBuf {
        let path = test_file_path(name, extension);
        fs::write(&path, bytes).expect("测试 HPROF 文件应能写入临时目录");
        path
    }

    /// 返回测试文件对应的 sidecar 目录。
    fn test_sidecar_dir(path: &Path) -> PathBuf {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("dump");
        path.parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{file_name}.logclinic-hprof"))
    }

    /// 清理测试文件及其 sidecar 目录。
    fn cleanup_test_file(path: &Path) {
        let _ = fs::remove_file(path);
        let _ = fs::remove_dir_all(test_sidecar_dir(path));
    }

    /// 写入 4 字节 ID 的 HPROF header。
    fn push_header(bytes: &mut Vec<u8>) {
        push_header_with_id_size(bytes, 4);
    }

    /// 写入指定 ID 宽度的 HPROF header。
    fn push_header_with_id_size(bytes: &mut Vec<u8>, identifier_size: u32) {
        bytes.extend_from_slice(b"JAVA PROFILE 1.0.2\0");
        bytes.extend_from_slice(&identifier_size.to_be_bytes());
        bytes.extend_from_slice(&0u64.to_be_bytes());
    }

    /// 写入 HPROF 顶层记录。
    fn push_record(bytes: &mut Vec<u8>, tag: u8, body: Vec<u8>) {
        bytes.push(tag);
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&body);
    }

    /// 写入 4 字节对象 ID。
    fn push_id(bytes: &mut Vec<u8>, id: u32) {
        bytes.extend_from_slice(&id.to_be_bytes());
    }

    /// 构造 STRING_IN_UTF8 记录体。
    fn string_body(id: u32, value: &str) -> Vec<u8> {
        let mut body = Vec::new();
        push_id(&mut body, id);
        body.extend_from_slice(value.as_bytes());
        body
    }

    /// 构造 LOAD_CLASS 记录体。
    fn load_class_body(serial: u32, class_id: u32, name_id: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&serial.to_be_bytes());
        push_id(&mut body, class_id);
        body.extend_from_slice(&0u32.to_be_bytes());
        push_id(&mut body, name_id);
        body
    }

    /// 构造 START_THREAD 记录体。
    fn start_thread_body(
        thread_serial: u32,
        object_id: u32,
        stack_trace_serial: u32,
        thread_name_id: u32,
    ) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&thread_serial.to_be_bytes());
        push_id(&mut body, object_id);
        body.extend_from_slice(&stack_trace_serial.to_be_bytes());
        push_id(&mut body, thread_name_id);
        push_id(&mut body, 0);
        push_id(&mut body, 0);
        body
    }

    /// 构造 STACK_FRAME 记录体。
    fn stack_frame_body(
        frame_id: u32,
        method_name_id: u32,
        signature_id: u32,
        source_id: u32,
        class_serial: u32,
        line_number: i32,
    ) -> Vec<u8> {
        let mut body = Vec::new();
        push_id(&mut body, frame_id);
        push_id(&mut body, method_name_id);
        push_id(&mut body, signature_id);
        push_id(&mut body, source_id);
        body.extend_from_slice(&class_serial.to_be_bytes());
        body.extend_from_slice(&line_number.to_be_bytes());
        body
    }

    /// 构造 STACK_TRACE 记录体。
    fn stack_trace_body(stack_trace_serial: u32, thread_serial: u32, frame_ids: &[u32]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&stack_trace_serial.to_be_bytes());
        body.extend_from_slice(&thread_serial.to_be_bytes());
        body.extend_from_slice(&(frame_ids.len() as u32).to_be_bytes());
        for frame_id in frame_ids {
            push_id(&mut body, *frame_id);
        }
        body
    }

    /// 构造单字段 CLASS_DUMP 子记录。
    fn class_dump_subrecord(
        class_id: u32,
        super_id: u32,
        instance_size: u32,
        field_name_id: u32,
        field_type: u8,
    ) -> Vec<u8> {
        class_dump_subrecord_fields(
            class_id,
            super_id,
            instance_size,
            &[],
            &[(field_name_id, field_type)],
        )
    }

    /// 构造可指定静态字段和实例字段的 CLASS_DUMP 子记录。
    fn class_dump_subrecord_fields(
        class_id: u32,
        super_id: u32,
        instance_size: u32,
        static_object_fields: &[(u32, u32)],
        instance_fields: &[(u32, u8)],
    ) -> Vec<u8> {
        class_dump_subrecord_fields_with_loader(
            class_id,
            super_id,
            0,
            instance_size,
            static_object_fields,
            instance_fields,
        )
    }

    /// 构造可指定类加载器、静态字段和实例字段的 CLASS_DUMP 子记录。
    fn class_dump_subrecord_fields_with_loader(
        class_id: u32,
        super_id: u32,
        class_loader_id: u32,
        instance_size: u32,
        static_object_fields: &[(u32, u32)],
        instance_fields: &[(u32, u8)],
    ) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(SUB_CLASS_DUMP);
        push_id(&mut body, class_id);
        body.extend_from_slice(&0u32.to_be_bytes());
        push_id(&mut body, super_id);
        push_id(&mut body, class_loader_id);
        push_id(&mut body, 0);
        push_id(&mut body, 0);
        push_id(&mut body, 0);
        push_id(&mut body, 0);
        body.extend_from_slice(&instance_size.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&(static_object_fields.len() as u16).to_be_bytes());
        for (field_name_id, reference_id) in static_object_fields {
            push_id(&mut body, *field_name_id);
            body.push(FIELD_TYPE_OBJECT);
            push_id(&mut body, *reference_id);
        }
        body.extend_from_slice(&(instance_fields.len() as u16).to_be_bytes());
        for (field_name_id, field_type) in instance_fields {
            push_id(&mut body, *field_name_id);
            body.push(*field_type);
        }
        body
    }

    /// 构造单对象字段 INSTANCE_DUMP 子记录。
    fn instance_dump_subrecord(object_id: u32, class_id: u32, field_reference: u32) -> Vec<u8> {
        instance_dump_subrecord_data(object_id, class_id, &field_reference.to_be_bytes())
    }

    /// 构造指定字段数据的 INSTANCE_DUMP 子记录。
    fn instance_dump_subrecord_data(object_id: u32, class_id: u32, data: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(SUB_INSTANCE_DUMP);
        push_id(&mut body, object_id);
        body.extend_from_slice(&0u32.to_be_bytes());
        push_id(&mut body, class_id);
        body.extend_from_slice(&(data.len() as u32).to_be_bytes());
        body.extend_from_slice(data);
        body
    }

    /// 构造 OBJECT_ARRAY_DUMP 子记录。
    fn object_array_subrecord(array_id: u32, class_id: u32, references: &[u32]) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(SUB_OBJECT_ARRAY_DUMP);
        push_id(&mut body, array_id);
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&(references.len() as u32).to_be_bytes());
        push_id(&mut body, class_id);
        for reference in references {
            push_id(&mut body, *reference);
        }
        body
    }

    /// 构造 PRIMITIVE_ARRAY_DUMP 子记录。
    fn primitive_array_subrecord(array_id: u32, element_type: u8, data: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(SUB_PRIMITIVE_ARRAY_DUMP);
        push_id(&mut body, array_id);
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&(data.len() as u32).to_be_bytes());
        body.push(element_type);
        body.extend_from_slice(data);
        body
    }

    /// 构造 ROOT_UNKNOWN 子记录。
    fn root_unknown_subrecord(object_id: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(SUB_ROOT_UNKNOWN);
        push_id(&mut body, object_id);
        body
    }

    /// 构造 ROOT_THREAD_OBJECT 子记录。
    fn root_thread_object_subrecord(
        object_id: u32,
        thread_serial: u32,
        stack_trace_serial: u32,
    ) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(SUB_ROOT_THREAD_OBJECT);
        push_id(&mut body, object_id);
        body.extend_from_slice(&thread_serial.to_be_bytes());
        body.extend_from_slice(&stack_trace_serial.to_be_bytes());
        body
    }

    /// 构造最小 HPROF fixture。
    fn minimal_hprof_fixture() -> Vec<u8> {
        let mut bytes = Vec::new();
        push_header(&mut bytes);
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(1, "com/example/Node"),
        );
        push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "next"));
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));

        let mut heap = Vec::new();
        heap.extend(class_dump_subrecord(100, 0, 4, 2, FIELD_TYPE_OBJECT));
        heap.extend(root_unknown_subrecord(1));
        heap.extend(instance_dump_subrecord(1, 100, 2));
        heap.extend(instance_dump_subrecord(2, 100, 0));
        heap.extend(object_array_subrecord(3, 100, &[1, 2]));
        heap.extend(primitive_array_subrecord(4, FIELD_TYPE_BYTE, &[1, 2, 3]));
        push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
        bytes
    }

    /// 构造大量实例组成的链式 HPROF fixture。
    fn synthetic_chain_hprof_fixture(instance_count: u32) -> Vec<u8> {
        let class_id = 0x7fff_0000;
        let mut bytes = Vec::new();
        push_header(&mut bytes);
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(1, "com/example/Node"),
        );
        push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "next"));
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, class_id, 1));

        let mut heap = Vec::new();
        heap.extend(class_dump_subrecord(class_id, 0, 4, 2, FIELD_TYPE_OBJECT));
        heap.extend(root_unknown_subrecord(1));
        for object_id in 1..=instance_count {
            let next = if object_id == instance_count {
                0
            } else {
                object_id + 1
            };
            heap.extend(instance_dump_subrecord(object_id, class_id, next));
        }
        push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
        bytes
    }

    /// 构造包含 WeakReference.referent 的 HPROF fixture。
    fn weak_reference_hprof_fixture() -> Vec<u8> {
        let mut bytes = Vec::new();
        push_header(&mut bytes);
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(1, "java/lang/ref/Reference"),
        );
        push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "referent"));
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(3, "java/lang/ref/WeakReference"),
        );
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(4, "com/example/Target"),
        );
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(2, 200, 3));
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(3, 300, 4));

        let mut heap = Vec::new();
        heap.extend(class_dump_subrecord(100, 0, 4, 2, FIELD_TYPE_OBJECT));
        heap.extend(class_dump_subrecord_fields(200, 100, 4, &[], &[]));
        heap.extend(class_dump_subrecord_fields(300, 0, 0, &[], &[]));
        heap.extend(root_unknown_subrecord(10));
        heap.extend(instance_dump_subrecord(10, 200, 20));
        heap.extend(instance_dump_subrecord_data(20, 300, &[]));
        push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
        bytes
    }

    /// 构造 ThreadLocalMap.Entry 继承 WeakReference 的 HPROF fixture。
    fn thread_local_entry_hprof_fixture() -> Vec<u8> {
        let mut bytes = Vec::new();
        push_header(&mut bytes);
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(1, "java/lang/ref/Reference"),
        );
        push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "referent"));
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(3, "java/lang/ref/WeakReference"),
        );
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(4, "java/lang/ThreadLocal$ThreadLocalMap$Entry"),
        );
        push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(5, "value"));
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(6, "com/example/Target"),
        );
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(2, 200, 3));
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(3, 300, 4));
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(4, 400, 6));

        let mut heap = Vec::new();
        heap.extend(class_dump_subrecord(100, 0, 4, 2, FIELD_TYPE_OBJECT));
        heap.extend(class_dump_subrecord_fields(200, 100, 4, &[], &[]));
        heap.extend(class_dump_subrecord(300, 200, 8, 5, FIELD_TYPE_OBJECT));
        heap.extend(class_dump_subrecord_fields(400, 0, 0, &[], &[]));
        heap.extend(root_unknown_subrecord(10));
        let mut entry_data = Vec::new();
        push_id(&mut entry_data, 20);
        push_id(&mut entry_data, 30);
        heap.extend(instance_dump_subrecord_data(10, 300, &entry_data));
        heap.extend(instance_dump_subrecord_data(20, 400, &[]));
        heap.extend(instance_dump_subrecord_data(30, 400, &[]));
        push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
        bytes
    }

    /// 构造静态字段强引用 HPROF fixture。
    fn static_field_hprof_fixture() -> Vec<u8> {
        let mut bytes = Vec::new();
        push_header(&mut bytes);
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(1, "com/example/Statics"),
        );
        push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "cached"));
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(3, "com/example/Target"),
        );
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(2, 200, 3));

        let mut heap = Vec::new();
        heap.extend(class_dump_subrecord_fields(100, 0, 0, &[(2, 20)], &[]));
        heap.extend(class_dump_subrecord_fields(200, 0, 0, &[], &[]));
        heap.extend(root_unknown_subrecord(100));
        heap.extend(instance_dump_subrecord_data(20, 200, &[]));
        push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
        bytes
    }

    /// 构造对象数组强引用 HPROF fixture。
    fn object_array_root_hprof_fixture() -> Vec<u8> {
        let mut bytes = Vec::new();
        push_header(&mut bytes);
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(1, "com/example/Target"),
        );
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));

        let mut heap = Vec::new();
        heap.extend(class_dump_subrecord_fields(100, 0, 0, &[], &[]));
        heap.extend(root_unknown_subrecord(50));
        heap.extend(object_array_subrecord(50, 100, &[20]));
        heap.extend(instance_dump_subrecord_data(20, 100, &[]));
        push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
        bytes
    }

    /// 构造 class loader 通过 loaded class 和 static 字段保活对象的 HPROF fixture。
    fn class_loader_static_hprof_fixture() -> Vec<u8> {
        let mut bytes = Vec::new();
        push_header(&mut bytes);
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(1, "com/example/CacheStatics"),
        );
        push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "cache"));
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(3, "com/example/Target"),
        );
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(4, "com/example/Loader"),
        );
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(2, 200, 3));
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(3, 300, 4));

        let mut heap = Vec::new();
        heap.extend(class_dump_subrecord_fields_with_loader(
            100,
            0,
            10,
            0,
            &[(2, 20)],
            &[],
        ));
        heap.extend(class_dump_subrecord_fields(200, 0, 0, &[], &[]));
        heap.extend(class_dump_subrecord_fields(300, 0, 0, &[], &[]));
        heap.extend(root_unknown_subrecord(10));
        heap.extend(instance_dump_subrecord_data(10, 300, &[]));
        heap.extend(instance_dump_subrecord_data(20, 200, &[]));
        push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
        bytes
    }

    /// 构造 bootstrap/system class 通过 static 字段保活对象的 HPROF fixture。
    fn bootstrap_static_hprof_fixture() -> Vec<u8> {
        let mut bytes = Vec::new();
        push_header(&mut bytes);
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(1, "com/example/BootstrapStatics"),
        );
        push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "cache"));
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(3, "com/example/Target"),
        );
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(2, 200, 3));

        let mut heap = Vec::new();
        heap.extend(class_dump_subrecord_fields(100, 0, 0, &[(2, 20)], &[]));
        heap.extend(class_dump_subrecord_fields(200, 0, 0, &[], &[]));
        heap.extend(instance_dump_subrecord_data(20, 200, &[]));
        push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
        bytes
    }

    /// 构造子类 primitive 字段位于父类 object 字段之前的继承 fixture。
    fn inherited_field_order_hprof_fixture() -> Vec<u8> {
        let mut bytes = Vec::new();
        push_header(&mut bytes);
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(1, "com/example/Parent"),
        );
        push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "parentRef"));
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(3, "com/example/Child"),
        );
        push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(4, "childInt"));
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(5, "com/example/Target"),
        );
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(2, 200, 3));
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(3, 300, 5));

        let mut heap = Vec::new();
        heap.extend(class_dump_subrecord(100, 0, 4, 2, FIELD_TYPE_OBJECT));
        heap.extend(class_dump_subrecord(200, 100, 8, 4, FIELD_TYPE_INT));
        heap.extend(class_dump_subrecord_fields(300, 0, 0, &[], &[]));
        heap.extend(root_unknown_subrecord(10));
        let mut child_data = Vec::new();
        child_data.extend_from_slice(&1234u32.to_be_bytes());
        push_id(&mut child_data, 20);
        heap.extend(instance_dump_subrecord_data(10, 200, &child_data));
        heap.extend(instance_dump_subrecord_data(20, 300, &[]));
        push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
        bytes
    }

    /// 构造包含线程属性和堆栈的 HPROF fixture。
    fn thread_details_hprof_fixture(include_start_thread: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        push_header(&mut bytes);
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(1, "java/lang/Thread"),
        );
        push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "name"));
        push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(3, "priority"));
        push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(4, "daemon"));
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(5, "threadStatus"),
        );
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(6, "contextClassLoader"),
        );
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(7, "com/example/Loader"),
        );
        push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(8, "worker-1"));
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(9, "java/util/concurrent/locks/LockSupport"),
        );
        push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(10, "park"));
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(11, "(Ljava/lang/Object;)V"),
        );
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(12, "LockSupport.java"),
        );
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(2, 200, 7));
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(3, 300, 9));
        if include_start_thread {
            push_record(
                &mut bytes,
                TAG_START_THREAD,
                start_thread_body(7, 10, 70, 8),
            );
        }
        push_record(
            &mut bytes,
            TAG_STACK_FRAME,
            stack_frame_body(400, 10, 11, 12, 3, 175),
        );
        push_record(&mut bytes, TAG_STACK_TRACE, stack_trace_body(70, 7, &[400]));

        let mut heap = Vec::new();
        heap.extend(class_dump_subrecord_fields(
            100,
            0,
            17,
            &[],
            &[
                (2, FIELD_TYPE_OBJECT),
                (3, FIELD_TYPE_INT),
                (4, FIELD_TYPE_BOOLEAN),
                (5, FIELD_TYPE_INT),
                (6, FIELD_TYPE_OBJECT),
            ],
        ));
        heap.extend(class_dump_subrecord_fields(200, 0, 0, &[], &[]));
        heap.extend(root_thread_object_subrecord(10, 7, 70));
        let mut thread_data = Vec::new();
        push_id(&mut thread_data, 0);
        thread_data.extend_from_slice(&5u32.to_be_bytes());
        thread_data.push(1);
        thread_data.extend_from_slice(&0x291u32.to_be_bytes());
        push_id(&mut thread_data, 20);
        heap.extend(instance_dump_subrecord_data(10, 100, &thread_data));
        heap.extend(instance_dump_subrecord_data(20, 200, &[]));
        push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
        bytes
    }

    /// 构造 `java.lang.Thread` 子类的 HPROF fixture。
    fn thread_subclass_hprof_fixture() -> Vec<u8> {
        let mut bytes = Vec::new();
        push_header(&mut bytes);
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(1, "java/lang/Thread"),
        );
        push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "priority"));
        push_record(
            &mut bytes,
            TAG_STRING_IN_UTF8,
            string_body(3, "com/example/WorkerThread"),
        );
        push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(4, "worker-sub"));
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));
        push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(2, 110, 3));
        push_record(&mut bytes, TAG_START_THREAD, start_thread_body(8, 11, 0, 4));

        let mut heap = Vec::new();
        heap.extend(class_dump_subrecord(100, 0, 4, 2, FIELD_TYPE_INT));
        heap.extend(class_dump_subrecord_fields(110, 100, 4, &[], &[]));
        heap.extend(root_thread_object_subrecord(11, 8, 0));
        heap.extend(instance_dump_subrecord_data(11, 110, &5u32.to_be_bytes()));
        push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
        bytes
    }

    /// 验证 `.hprof` 和 `.bin` 后缀都允许，其他后缀拒绝。
    #[test]
    fn 文件选择只接受_hprof_和_bin后缀() {
        assert!(hprof_path_has_supported_extension(Path::new("heap.hprof")));
        assert!(hprof_path_has_supported_extension(Path::new("heap.bin")));
        assert!(!hprof_path_has_supported_extension(Path::new("heap.txt")));
    }

    /// 验证 `.hprof` 文件同样需要通过真实 header 校验。
    #[test]
    fn hprof文件必须是合法_header() {
        let valid_path = write_test_file("valid-hprof", "hprof", &minimal_hprof_fixture());
        assert!(validate_hprof_file_selection(&valid_path).is_ok());

        let invalid_path = write_test_file("invalid-hprof", "hprof", b"not hprof");
        assert!(validate_hprof_file_selection(&invalid_path).is_err());
        let _ = fs::remove_file(valid_path);
        let _ = fs::remove_file(invalid_path);
    }

    /// 验证 `.bin` 改名文件仍必须通过 HPROF header 校验。
    #[test]
    fn bin文件必须是合法_hprof_header() {
        let valid_path = write_test_file("valid-bin", "bin", &minimal_hprof_fixture());
        assert!(validate_hprof_file_selection(&valid_path).is_ok());

        let invalid_path = write_test_file("invalid-bin", "bin", b"not hprof");
        assert!(validate_hprof_file_selection(&invalid_path).is_err());
        let _ = fs::remove_file(valid_path);
        let _ = fs::remove_file(invalid_path);
    }

    /// 验证非法后缀即使内容像 HPROF 也会在选择校验阶段被拒绝。
    #[test]
    fn 非法后缀会被拒绝() {
        let path = write_test_file("invalid-extension", "txt", &minimal_hprof_fixture());
        assert!(validate_hprof_file_selection(&path).is_err());
        let _ = fs::remove_file(path);
    }

    /// 验证截断 header 会返回格式错误。
    #[test]
    fn 截断header会被拒绝() {
        let path = write_test_file("truncated", "hprof", b"JAVA PROFILE 1.0.2");
        assert!(validate_hprof_file_selection(&path).is_err());
        let _ = fs::remove_file(path);
    }

    /// 验证最小 fixture 可以解析类名、实例引用、对象数组和基础数组。
    #[test]
    fn 最小fixture可以解析对象图() {
        let path = write_test_file("minimal", "hprof", &minimal_hprof_fixture());
        let mut progresses = Vec::new();
        let result = analyze_hprof_dominator_tree(
            path.clone(),
            |progress| progresses.push(progress),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("最小 HPROF fixture 应解析成功");

        assert_eq!(result.header.label, "JAVA PROFILE 1.0.2");
        assert_eq!(result.total_classes, 1);
        assert_eq!(result.total_objects, 5);
        assert_eq!(result.gc_root_count, 2);
        assert_eq!(result.synthetic_bootstrap_class_root_count, 1);
        assert_eq!(
            result.node_for_object_id(1).unwrap().name,
            "com.example.Node"
        );
        assert!(
            progresses
                .iter()
                .any(|progress| progress.stage == HprofAnalysisStage::ReadingRecords)
        );
        let _ = fs::remove_file(path);
    }

    /// 验证优化后的紧凑结果仍保留第一版 fixture 的核心语义。
    #[test]
    fn 最小fixture紧凑结果保持等价() {
        let path = write_test_file("equivalence", "hprof", &minimal_hprof_fixture());
        let result =
            analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
                .expect("最小 HPROF fixture 应解析成功");

        assert_eq!(result.edge_count, 3);
        assert_eq!(result.raw_edge_count, 3);
        assert_eq!(result.reference_edge_stats.total(), 0);
        assert_eq!(result.node_for_object_id(1).unwrap().retained_size, 32);
        assert_eq!(result.node_for_object_id(2).unwrap().retained_size, 16);
        assert_eq!(result.reachable_shallow_size, 32);
        assert_eq!(result.top_object_ids.first().copied(), Some(1));
        let _ = fs::remove_file(path);
    }

    /// 验证 WeakReference.referent 会参与 MAT dominator 对象图，同时单独统计为 weak-like referent。
    #[test]
    fn weak_reference_referent会参与mat对象图() {
        let path = write_test_file("weak-reference", "hprof", &weak_reference_hprof_fixture());
        let result =
            analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
                .expect("WeakReference fixture 应解析成功");

        assert_eq!(result.reference_edge_stats.weak_like, 1);
        assert_eq!(result.reference_edge_stats.total(), 1);
        assert_eq!(result.raw_edge_count, result.edge_count);
        assert!(result.node_for_object_id(20).is_some());
        assert_eq!(result.node_for_object_id(10).unwrap().retained_size, 32);
        let _ = fs::remove_file(path);
    }

    /// 验证 ThreadLocalMap.Entry 继承 WeakReference 时 referent 和 value 都参与 MAT 对象图，referent 额外计入 weak-like 统计。
    #[test]
    fn threadlocal_entry会保留referent并统计弱引用边() {
        let path = write_test_file(
            "thread-local-entry",
            "hprof",
            &thread_local_entry_hprof_fixture(),
        );
        let result =
            analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
                .expect("ThreadLocalMap.Entry fixture 应解析成功");

        assert_eq!(result.reference_edge_stats.weak_like, 1);
        assert_eq!(result.reference_edge_stats.total(), 1);
        assert!(result.node_for_object_id(20).is_some());
        assert!(result.node_for_object_id(30).is_some());
        assert_eq!(result.node_for_object_id(10).unwrap().retained_size, 56);
        let _ = fs::remove_file(path);
    }

    /// 验证静态字段仍按强引用处理，避免 MAT 兼容修正误伤 class object 到缓存对象的边。
    #[test]
    fn 静态对象字段仍然是强引用() {
        let path = write_test_file("static-field", "hprof", &static_field_hprof_fixture());
        let result =
            analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
                .expect("静态字段 fixture 应解析成功");

        assert_eq!(result.reference_edge_stats.total(), 0);
        assert!(result.node_for_object_id(20).is_some());
        assert_eq!(result.node_for_object_id(100).unwrap().retained_size, 16);
        let _ = fs::remove_file(path);
    }

    /// 验证对象数组元素仍按强引用处理。
    #[test]
    fn 对象数组元素仍然是强引用() {
        let path = write_test_file(
            "object-array-root",
            "hprof",
            &object_array_root_hprof_fixture(),
        );
        let result =
            analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
                .expect("对象数组 fixture 应解析成功");

        assert_eq!(result.reference_edge_stats.total(), 0);
        assert!(result.node_for_object_id(20).is_some());
        assert_eq!(result.node_for_object_id(50).unwrap().retained_size, 40);
        let _ = fs::remove_file(path);
    }

    /// 验证 MAT 兼容模式会合成 class loader 到 class object 的强引用边，使 static 字段对象进入 dominator tree。
    #[test]
    fn classloader会保活其加载类的static字段对象() {
        let path = write_test_file(
            "class-loader-static",
            "hprof",
            &class_loader_static_hprof_fixture(),
        );
        let result =
            analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
                .expect("class loader static fixture 应解析成功");

        assert_eq!(result.synthetic_class_loader_edge_count, 1);
        assert_eq!(result.reference_edge_stats.total(), 0);
        assert!(result.node_for_object_id(100).is_some());
        assert!(result.node_for_object_id(20).is_some());
        assert_eq!(result.node_for_object_id(100).unwrap().retained_size, 16);
        let _ = fs::remove_file(path);
    }

    /// 验证 bootstrap/system class 会作为合成 root，使没有普通 class loader 对象的 static 字段仍可达。
    #[test]
    fn bootstrap_class会作为合成root保活static字段对象() {
        let path = write_test_file(
            "bootstrap-static",
            "hprof",
            &bootstrap_static_hprof_fixture(),
        );
        let result =
            analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
                .expect("bootstrap static fixture 应解析成功");

        assert_eq!(result.synthetic_class_loader_edge_count, 0);
        assert_eq!(result.synthetic_bootstrap_class_root_count, 2);
        assert!(result.node_for_object_id(100).is_some());
        assert!(result.node_for_object_id(20).is_some());
        assert_eq!(result.node_for_object_id(100).unwrap().retained_size, 16);
        let _ = fs::remove_file(path);
    }

    /// 验证 MAT 兼容 shallow size 不把 HPROF ID 宽度误当作堆内引用宽度。
    #[test]
    fn mat_size_model会区分id宽度和堆引用宽度() {
        let header4 = HprofHeader {
            label: "JAVA PROFILE 1.0.2".to_string(),
            identifier_size: 4,
            timestamp_millis: 0,
        };
        let header8 = HprofHeader {
            label: "JAVA PROFILE 1.0.2".to_string(),
            identifier_size: 8,
            timestamp_millis: 0,
        };
        let model4 = HprofSizeModel::mat_compatible(&header4, 1024);
        let compressed_model8 = HprofSizeModel::mat_compatible(&header8, 1024);
        let wide_model8 =
            HprofSizeModel::mat_compatible(&header8, HPROF_COMPRESSED_OOPS_HEAP_LIMIT);

        assert_eq!(model4.reference_size, 4);
        assert_eq!(compressed_model8.reference_size, 4);
        assert!(compressed_model8.compressed_oops);
        assert_eq!(wide_model8.reference_size, 8);
        assert!(!wide_model8.compressed_oops);
        assert_eq!(instance_shallow_size(&compressed_model8, 17), 32);
        assert_eq!(object_array_shallow_size(&compressed_model8, 2), 24);
        assert_eq!(
            primitive_array_shallow_size(&compressed_model8, FIELD_TYPE_BYTE, 3).unwrap(),
            24
        );
        assert_eq!(object_array_shallow_size(&wide_model8, 2), 32);
    }

    /// 验证重复对象 ID 会被视为损坏 dump，避免 streaming/sidecar 路径出现覆盖语义。
    #[test]
    fn 重复对象id会返回格式错误() {
        let header = HprofHeader {
            label: "JAVA PROFILE 1.0.2".to_string(),
            identifier_size: 8,
            timestamp_millis: 0,
        };
        let size_model = HprofSizeModel::mat_compatible(&header, 1024);
        let mut graph = HprofObjectGraph::new(header, size_model);
        let object = HprofHeapObject {
            id: 1,
            class_id: 100,
            shallow_size: 8,
            kind: HprofObjectKind::Instance,
        };
        graph
            .insert_object(object.clone(), Vec::new())
            .expect("首次插入对象应成功");
        let error = graph
            .insert_object(object, Vec::new())
            .expect_err("重复对象 ID 应被拒绝");
        assert!(error.to_string().contains("重复定义"));
    }

    /// 验证低内存索引会拒绝超过 `u32` 哨兵上限的节点数。
    #[test]
    fn 低内存节点索引超过上限会返回中文错误() {
        let error = hprof_node_id(HPROF_INVALID_NODE as usize, "对象数量")
            .expect_err("u32 哨兵值必须保留给无效节点");
        assert!(error.to_string().contains("对象数量"));
        assert!(error.to_string().contains("低内存索引上限"));
    }

    /// 验证大于多个报告阈值的解析进度保持单调增长。
    #[test]
    fn 大量子记录进度单调增长() {
        let path = write_test_file("progress", "hprof", &synthetic_chain_hprof_fixture(5000));
        let mut progresses = Vec::new();
        let result = analyze_hprof_dominator_tree(
            path.clone(),
            |progress| progresses.push(progress),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("合成链式 HPROF 应解析成功");

        let record_progresses = progresses
            .iter()
            .filter(|progress| progress.stage == HprofAnalysisStage::ReadingRecords)
            .collect::<Vec<_>>();
        assert!(record_progresses.len() >= 2);
        for window in record_progresses.windows(2) {
            assert!(window[1].bytes_read >= window[0].bytes_read);
            assert!(window[1].record_count >= window[0].record_count);
            assert!(window[1].object_count >= window[0].object_count);
            assert!(window[1].edge_count >= window[0].edge_count);
        }
        assert_eq!(result.total_objects, 5001);
        assert_eq!(result.edge_count, 4999);
        let _ = fs::remove_file(path);
    }

    /// 验证紧凑图生成和 LT 算法都会输出可展示的细分阶段进度。
    #[test]
    fn 计算阶段会报告紧凑图和lt细分进度() {
        let path = write_test_file(
            "phase-progress",
            "hprof",
            &synthetic_chain_hprof_fixture(32),
        );
        let mut progresses = Vec::new();
        analyze_hprof_dominator_tree(
            path.clone(),
            |progress| progresses.push(progress),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("合成 HPROF 应解析成功");

        for expected in [
            "添加紧凑图节点",
            "添加 GC Root 边",
            "添加对象引用边",
            "DFS 编号",
            "构建前驱索引",
            "LT 主循环",
            "修正 immediate dominator",
            "后序聚合 retained size",
        ] {
            assert!(
                progresses
                    .iter()
                    .any(|progress| progress.sub_message == expected),
                "缺少阶段进度：{expected}"
            );
        }
        assert!(progresses.iter().any(|progress| {
            progress.sub_message == "添加对象引用边"
                && progress.phase_total > 0
                && progress.phase_done == progress.phase_total
                && progress.phase_unit == "边"
        }));
        assert!(progresses.iter().any(|progress| {
            progress.sub_message == "LT 主循环"
                && progress.phase_total > 0
                && progress.phase_done == progress.phase_total
                && progress.phase_unit == "节点"
        }));
        let _ = fs::remove_file(path);
    }

    /// 验证完成结果会写入 sidecar，并且第二次打开可以直接从缓存恢复。
    #[test]
    fn sidecar缓存命中会恢复相同dominator结果() {
        let path = write_test_file("sidecar-cache", "hprof", &minimal_hprof_fixture());
        cleanup_test_file(&path);
        fs::write(&path, minimal_hprof_fixture()).expect("测试 HPROF 文件应能重新写入");

        let first =
            analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
                .expect("首次 HPROF 解析应成功");
        let sidecar_dir = test_sidecar_dir(&path);
        assert!(sidecar_dir.join("manifest.json").is_file());
        assert!(sidecar_dir.join("objects.bin").is_file());
        assert!(sidecar_dir.join("dominator.bin").is_file());
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(sidecar_dir.join("manifest.json")).unwrap())
                .expect("sidecar manifest 应是合法 JSON");
        assert_eq!(
            manifest["schema_version"], HPROF_SIDECAR_SCHEMA_VERSION,
            "sidecar v3 应让旧缓存按 schema 自动失效"
        );

        let mut progresses = Vec::new();
        let second = analyze_hprof_dominator_tree(
            path.clone(),
            |progress| progresses.push(progress),
            Arc::new(AtomicBool::new(false)),
        )
        .expect("第二次 HPROF 解析应从 sidecar 恢复");

        assert_eq!(first.total_objects, second.total_objects);
        assert_eq!(first.reachable_shallow_size, second.reachable_shallow_size);
        assert_eq!(first.top_object_ids, second.top_object_ids);
        assert_eq!(second.cache_status.as_deref(), Some("sidecar index 命中"));
        assert!(
            progresses
                .iter()
                .any(|progress| progress.stage == HprofAnalysisStage::LoadingCache)
        );
        assert!(
            progresses.iter().any(|progress| {
                progress.stage == HprofAnalysisStage::LoadingCache
                    && progress.sub_message.contains("对象摘要懒加载")
                    && progress.phase_unit == "文件"
                    && progress.phase_total == 1
            }),
            "缓存命中路径不应全量读取对象摘要，应只建立可见行懒加载读取器"
        );
        assert!(
            progresses.iter().any(|progress| {
                progress.stage == HprofAnalysisStage::LoadingCache
                    && progress.object_count == second.total_objects
                    && progress.class_count == second.total_classes
                    && progress.gc_root_count == second.gc_root_count
                    && progress.edge_count == second.edge_count
            }),
            "缓存命中路径应在读取期间恢复摘要计数，避免 UI 长时间显示 0"
        );
        assert!(
            progresses.iter().all(|progress| {
                progress.stage != HprofAnalysisStage::LoadingCache
                    || !progress.sub_message.starts_with("写入 ")
            }),
            "缓存命中路径只能展示读取进度，不能复用写入 sidecar 的文案"
        );
        cleanup_test_file(&path);
    }

    /// 验证 sidecar 缺少主体文件时会自动忽略旧缓存并重新解析。
    #[test]
    fn sidecar缺少文件会重新解析() {
        let path = write_test_file("sidecar-missing-file", "hprof", &minimal_hprof_fixture());
        cleanup_test_file(&path);
        fs::write(&path, minimal_hprof_fixture()).expect("测试 HPROF 文件应能重新写入");

        analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
            .expect("首次 HPROF 解析应成功");
        fs::remove_file(test_sidecar_dir(&path).join("objects.bin"))
            .expect("测试应能删除 sidecar 主体文件");
        let result =
            analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
                .expect("缺失 sidecar 文件时应重新解析成功");

        assert_ne!(result.cache_status.as_deref(), Some("sidecar index 命中"));
        cleanup_test_file(&path);
    }

    /// 验证损坏 sidecar 的长度字段不会触发异常大分配，而是忽略缓存并重新解析。
    #[test]
    fn sidecar损坏长度不会导致异常大分配() {
        let path = write_test_file("sidecar-corrupt-count", "hprof", &minimal_hprof_fixture());
        cleanup_test_file(&path);
        fs::write(&path, minimal_hprof_fixture()).expect("测试 HPROF 文件应能重新写入");

        analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
            .expect("首次 HPROF 解析应成功");
        let objects_path = test_sidecar_dir(&path).join("objects.bin");
        let mut bytes = fs::read(&objects_path).expect("测试应能读取 objects sidecar");
        let count_offset = 8 + b"LCHP-objects-v3".len();
        bytes[count_offset..count_offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        fs::write(&objects_path, bytes).expect("测试应能写入损坏 sidecar");

        let result =
            analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
                .expect("损坏 sidecar 应被忽略并重新解析成功");

        assert_ne!(result.cache_status.as_deref(), Some("sidecar index 命中"));
        cleanup_test_file(&path);
    }

    /// 默认忽略的 100MB 合成性能验证入口。
    ///
    /// 业务意图：
    /// - 该测试会写入较大临时文件并执行完整 dominator 计算，只用于本地手动比较优化前后阶段耗时，不应进入常规测试耗时。
    #[test]
    #[ignore]
    fn 合成100mb_hprof性能采样() {
        let target_bytes = 100 * 1024 * 1024usize;
        let instance_record_bytes = 1 + 4 + 4 + 4 + 4 + 4;
        let instance_count = (target_bytes / instance_record_bytes).min(u32::MAX as usize) as u32;
        let bytes = synthetic_chain_hprof_fixture(instance_count);
        let path = write_test_file("synthetic-100mb", "hprof", &bytes);
        let started_at = std::time::Instant::now();
        let result =
            analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
                .expect("100MB 合成 HPROF 应解析成功");
        eprintln!(
            "objects={} edges={} elapsed_ms={}",
            result.total_objects,
            result.edge_count,
            started_at.elapsed().as_millis()
        );
        let _ = fs::remove_file(path);
    }

    /// 构造 dominator 测试对象图。
    fn make_graph(objects: &[(u64, u64, u64, &[u64])], roots: &[u64]) -> HprofObjectGraph {
        let header = HprofHeader {
            label: "JAVA PROFILE 1.0.2".to_string(),
            identifier_size: 8,
            timestamp_millis: 0,
        };
        let size_model = HprofSizeModel::mat_compatible(&header, 1024 * 1024);
        let mut graph = HprofObjectGraph::new(header, size_model);
        graph.classes.insert(
            100,
            HprofClassInfo {
                instance_size: 1,
                name: Some("com.example.Node".to_string()),
                instance_fields: Vec::new(),
            },
        );
        for (id, class_id, shallow_size, references) in objects {
            graph
                .insert_object(
                    HprofHeapObject {
                        id: *id,
                        class_id: *class_id,
                        shallow_size: *shallow_size,
                        kind: HprofObjectKind::Instance,
                    },
                    references.to_vec(),
                )
                .expect("测试对象图不应包含重复对象 ID");
        }
        for root in roots {
            graph.gc_roots.push(HprofGcRoot {
                object_id: *root,
                kind: HprofGcRootKind::Unknown,
            });
        }
        graph
    }

    /// 测试中忽略计算阶段进度回调。
    fn ignore_compute_progress(
        _stage: HprofAnalysisStage,
        _message: &str,
        _sub_message: &str,
        _phase_done: u64,
        _phase_total: u64,
        _phase_unit: &'static str,
    ) {
    }

    /// 验证线性链 retained size 会沿 dominator tree 向上累计。
    #[test]
    fn 线性链会累计retained_size() {
        let graph = make_graph(
            &[(1, 100, 10, &[2]), (2, 100, 20, &[3]), (3, 100, 30, &[])],
            &[1],
        );
        let result = build_hprof_dominator_result(
            Path::new("linear.hprof"),
            graph,
            &AtomicBool::new(false),
            ignore_compute_progress,
        )
        .expect("线性 dominator 图应计算成功");

        assert_eq!(result.node_for_object_id(1).unwrap().retained_size, 60);
        assert_eq!(result.node_for_object_id(2).unwrap().retained_size, 50);
        assert_eq!(result.node_for_object_id(3).unwrap().retained_size, 30);
        assert_eq!(result.top_object_ids.first().copied(), Some(1));
    }

    /// 验证继承字段布局会按 HPROF INSTANCE_DUMP 的“子类字段在前、父类字段在后”顺序合并。
    #[test]
    fn 继承字段布局会合并父类字段() {
        let mut raw_classes = FxHashMap::default();
        raw_classes.insert(
            100,
            RawClassInfo {
                super_class_id: 0,
                class_loader_id: 0,
                static_fields: Vec::new(),
                instance_fields: vec![RawFieldDescriptor {
                    name_id: 1,
                    field_type: FIELD_TYPE_OBJECT,
                }],
            },
        );
        raw_classes.insert(
            200,
            RawClassInfo {
                super_class_id: 100,
                class_loader_id: 0,
                static_fields: Vec::new(),
                instance_fields: vec![RawFieldDescriptor {
                    name_id: 2,
                    field_type: FIELD_TYPE_INT,
                }],
            },
        );

        let layout = resolve_layout(&raw_classes, &mut FxHashMap::default(), 200)
            .expect("子类字段布局应能解析父类字段");

        assert_eq!(layout.len(), 2);
        assert_eq!(layout[0].field_type, FIELD_TYPE_INT);
        assert_eq!(layout[1].field_type, FIELD_TYPE_OBJECT);
    }

    /// 验证子类 primitive 字段位于父类 object 字段之前时，不会把 primitive 字节误读成对象 ID。
    #[test]
    fn 继承字段偏移按子类优先解析对象引用() {
        let path = write_test_file(
            "inherited-field-order",
            "hprof",
            &inherited_field_order_hprof_fixture(),
        );
        let result =
            analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
                .expect("继承字段顺序 fixture 应解析成功");

        assert!(result.node_for_object_id(20).is_some());
        assert_eq!(result.node_for_object_id(10).unwrap().retained_size, 40);
        let _ = fs::remove_file(path);
    }

    /// 返回线程详情属性值，便于测试锁定 MAT 风格属性表。
    fn thread_property_value<'a>(details: &'a HprofThreadDetails, name: &str) -> Option<&'a str> {
        details
            .properties
            .iter()
            .find(|property| property.name == name)
            .map(|property| property.value.as_str())
    }

    /// 验证 START_THREAD、STACK_TRACE 和 STACK_FRAME 可以生成线程详情。
    #[test]
    fn 线程详情会解析属性和堆栈() {
        let path = write_test_file(
            "thread-details",
            "hprof",
            &thread_details_hprof_fixture(true),
        );
        let result =
            analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
                .expect("线程详情 fixture 应解析成功");

        assert!(result.is_thread_object(10));
        let details = result
            .thread_details_for_object_id(10)
            .expect("线程对象应有详情");
        assert_eq!(details.thread_name, "worker-1");
        assert_eq!(thread_property_value(details, "Name"), Some("worker-1"));
        assert_eq!(thread_property_value(details, "Is Daemon"), Some("true"));
        assert_eq!(thread_property_value(details, "Priority"), Some("5"));
        assert_eq!(thread_property_value(details, "State value"), Some("0x291"));
        assert!(
            thread_property_value(details, "State")
                .unwrap_or_default()
                .contains("parked")
        );
        assert!(
            thread_property_value(details, "Context Class Loader")
                .unwrap_or_default()
                .contains("com.example.Loader @ 0x14")
        );
        assert_eq!(
            result.node_for_object_id(10).unwrap().name,
            "java.lang.Thread worker-1"
        );
        assert_eq!(details.stack_frames.len(), 1);
        assert_eq!(
            details.stack_frames[0].display,
            "at java.util.concurrent.locks.LockSupport.park(Ljava/lang/Object;)V (LockSupport.java:175)"
        );
        let _ = fs::remove_file(path);
    }

    /// 验证缺少 START_THREAD 时仍可通过 ROOT_THREAD_OBJECT 找到线程堆栈。
    #[test]
    fn root_thread_object可作为线程堆栈fallback() {
        let path = write_test_file(
            "thread-root-fallback",
            "hprof",
            &thread_details_hprof_fixture(false),
        );
        let result =
            analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
                .expect("ROOT_THREAD_OBJECT fallback fixture 应解析成功");

        let details = result
            .thread_details_for_object_id(10)
            .expect("线程对象应有详情");
        assert_eq!(details.thread_name, "<unknown>");
        assert_eq!(details.stack_frames.len(), 1);
        assert!(details.stack_message.is_none());
        let _ = fs::remove_file(path);
    }

    /// 验证 `java.lang.Thread` 子类同样会显示线程详情。
    #[test]
    fn thread子类也会生成线程详情() {
        let path = write_test_file("thread-subclass", "hprof", &thread_subclass_hprof_fixture());
        let result =
            analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
                .expect("Thread 子类 fixture 应解析成功");

        assert!(result.is_thread_object(11));
        let details = result
            .thread_details_for_object_id(11)
            .expect("Thread 子类应有详情");
        assert_eq!(details.class_name, "com.example.WorkerThread");
        assert_eq!(details.thread_name, "worker-sub");
        assert_eq!(thread_property_value(details, "Priority"), Some("5"));
        assert_eq!(
            details.stack_message.as_deref(),
            Some("该 HPROF 未包含此线程的堆栈信息")
        );
        let _ = fs::remove_file(path);
    }

    /// 验证菱形引用中共享子节点不会被单侧 retained size 误吞。
    #[test]
    fn 菱形引用不会把共享对象算进单侧retained() {
        let graph = make_graph(
            &[
                (1, 100, 10, &[2, 3]),
                (2, 100, 20, &[4]),
                (3, 100, 30, &[4]),
                (4, 100, 40, &[]),
            ],
            &[1],
        );
        let result = build_hprof_dominator_result(
            Path::new("diamond.hprof"),
            graph,
            &AtomicBool::new(false),
            ignore_compute_progress,
        )
        .expect("菱形引用 dominator 图应计算成功");

        assert_eq!(result.node_for_object_id(1).unwrap().retained_size, 100);
        assert_eq!(result.node_for_object_id(2).unwrap().retained_size, 20);
        assert_eq!(result.node_for_object_id(3).unwrap().retained_size, 30);
        let mut expanded = HashSet::new();
        expanded.insert(1);
        assert!(
            result
                .visible_rows(&expanded)
                .iter()
                .any(|row| { row.depth == 1 && row.node.object_id == 4 })
        );
    }

    /// 验证多个 GC root 和不可达对象会被分别统计。
    #[test]
    fn 多root和不可达对象会被统计() {
        let graph = make_graph(
            &[
                (1, 100, 10, &[2]),
                (2, 100, 20, &[]),
                (3, 100, 30, &[]),
                (4, 100, 40, &[]),
            ],
            &[1, 3],
        );
        let result = build_hprof_dominator_result(
            Path::new("roots.hprof"),
            graph,
            &AtomicBool::new(false),
            ignore_compute_progress,
        )
        .expect("多 root dominator 图应计算成功");

        assert_eq!(result.reachable_object_count, 3);
        assert_eq!(result.unreachable_object_count, 1);
        assert_eq!(result.unreachable_shallow_size, 40);
    }

    /// 验证展开集合可以把 Top 节点的直接支配子节点加入可见行。
    #[test]
    fn 展开集合会生成可见子行() {
        let graph = make_graph(
            &[(1, 100, 10, &[2]), (2, 100, 20, &[3]), (3, 100, 30, &[])],
            &[1],
        );
        let result = build_hprof_dominator_result(
            Path::new("visible.hprof"),
            graph,
            &AtomicBool::new(false),
            ignore_compute_progress,
        )
        .expect("可见行测试 dominator 图应计算成功");

        let collapsed_rows = result.visible_rows(&HashSet::new());
        let mut expanded = HashSet::new();
        expanded.insert(1);
        let expanded_rows = result.visible_rows(&expanded);

        assert!(!collapsed_rows.is_empty());
        assert!(expanded_rows.len() > collapsed_rows.len());
        assert!(expanded_rows.iter().any(|row| row.depth == 1));
    }

    /// 验证顶层 dominator 行只包含虚拟 Root 的直接支配子节点，不把子孙节点重复提升为顶层 Top retained。
    #[test]
    fn 顶层行只显示虚拟root直接子节点() {
        let graph = make_graph(
            &[(1, 100, 10, &[2]), (2, 100, 20, &[3]), (3, 100, 30, &[])],
            &[1],
        );
        let result = build_hprof_dominator_result(
            Path::new("top-root-children.hprof"),
            graph,
            &AtomicBool::new(false),
            ignore_compute_progress,
        )
        .expect("顶层 dominator 图应计算成功");

        assert_eq!(result.top_object_ids, vec![1]);
        let collapsed_rows = result.visible_rows(&HashSet::new());
        assert_eq!(collapsed_rows.len(), 1);
        assert_eq!(collapsed_rows[0].node.object_id, 1);
    }
}
