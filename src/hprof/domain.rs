//! HPROF 领域对象图和结果基础模型。
//!
//! 业务意图：
//! - 该模块承载 HPROF header、对象、类、GC root、引用强度统计和对象图结构，作为 parser、graph、dominator 与结果构造之间的共享边界。
//! - 领域模型不处理文件 I/O、sidecar 读写或 UI 展示事件，便于后续继续拆分解析、缓存和结果构造逻辑。
//!
//! 边界条件：
//! - MAT 兼容 shallow size、compressed oops 估算、合成 class loader 边和 bootstrap root 统计保持原语义。
//! - 方法可见性只提升到 crate 内部，避免新增公开 API。

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use super::*;

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
    pub(crate) fn mat_compatible(header: &HprofHeader, source_bytes: u64) -> Self {
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
    pub(crate) fn align_size(&self, size: u64) -> u64 {
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
pub(crate) enum HprofReferenceStrength {
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
    pub(crate) fn add_strength(&mut self, strength: HprofReferenceStrength) {
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
    pub(crate) fn merge(&mut self, other: Self) {
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
    pub(crate) object_indices: FxHashMap<HprofObjectId, usize>,
    /// 每个对象当前引用段在 `reference_targets` 中的起始偏移。
    ///
    /// 业务意图：
    /// - 大 dump 中绝大多数对象引用数很少，如果每个对象都持有独立 `Vec`，数百万空 Vec 的结构体开销会非常高；
    ///   这里把所有引用统一放入连续池，按对象下标保存 offset/len，降低首次解析匿名堆峰值。
    pub(crate) reference_offsets: Vec<usize>,
    /// 每个对象当前引用段长度。
    pub(crate) reference_lengths: Vec<usize>,
    /// 所有对象引用目标 ID 的连续池。
    pub(crate) reference_targets: Vec<HprofObjectId>,
    /// 类元数据，按类对象 ID 索引。
    pub(crate) classes: FxHashMap<HprofObjectId, HprofClassInfo>,
    /// GC Root 列表。
    pub(crate) gc_roots: Vec<HprofGcRoot>,
    /// 已收集 MAT 兼容对象引用数量。
    pub(crate) edge_count: usize,
    /// `java.lang.ref.Reference.referent` 的引用强度统计。
    pub(crate) reference_edge_stats: HprofReferenceEdgeStats,
    /// MAT 兼容模式合成的 class loader 到 class object 强引用边数量。
    pub(crate) synthetic_class_loader_edge_count: usize,
    /// MAT 兼容模式合成的 bootstrap/system class root 数量。
    pub(crate) synthetic_bootstrap_class_root_count: usize,
    /// 已收集对象 shallow size 总和。
    pub(crate) total_shallow_size: u64,
    /// UTF8 字符串表。
    pub(crate) strings: FxHashMap<HprofObjectId, String>,
    /// LOAD_CLASS 记录，值为 `(class_object_id, class_name_string_id)`。
    pub(crate) loaded_classes: FxHashMap<u32, (HprofObjectId, HprofObjectId)>,
    /// START_THREAD 和 ROOT_THREAD_OBJECT 提供的线程对象到线程序号映射。
    ///
    /// 业务意图：
    /// - MAT Thread Details 需要从线程对象跳转到 `STACK_TRACE`；HPROF 中二者通过 thread serial 间接关联。
    pub(crate) thread_serial_by_object_id: FxHashMap<HprofObjectId, u32>,
    /// START_THREAD 记录，按线程序号索引。
    pub(super) thread_starts: FxHashMap<u32, HprofThreadStartRecord>,
    /// ROOT_THREAD_OBJECT 记录，按线程对象 ID 索引。
    pub(super) thread_object_roots: FxHashMap<HprofObjectId, HprofThreadObjectRootRecord>,
    /// STACK_TRACE 记录，按 stack trace serial 索引。
    pub(super) stack_traces: FxHashMap<u32, HprofStackTraceRecord>,
    /// STACK_FRAME 记录，按 frame ID 索引。
    pub(super) stack_frames: FxHashMap<HprofObjectId, HprofStackFrameRecord>,
    /// 解析实例字段后得到的线程对象属性原始值。
    pub(super) thread_instances: FxHashMap<HprofObjectId, HprofThreadInstanceInfo>,
    /// 可从 `java.lang.String` 实例解析出的字符串值。
    ///
    /// 边界条件：
    /// - 仅用于线程名 fallback 等轻量展示；解析失败不影响 dominator tree 正确性。
    pub(crate) java_string_values: FxHashMap<HprofObjectId, String>,
}

impl HprofObjectGraph {
    /// 创建空对象图。
    pub(crate) fn new(header: HprofHeader, size_model: HprofSizeModel) -> Self {
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
    pub(crate) fn insert_object(
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
    pub(crate) fn update_object_payload(
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
    pub(crate) fn references_for_index(&self, object_index: usize) -> &[HprofObjectId] {
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
    pub(crate) fn clear_reference_pool(&mut self) {
        self.reference_offsets = Vec::new();
        self.reference_lengths = Vec::new();
        self.reference_targets = Vec::new();
    }

    /// 返回对象数量。
    pub(crate) fn object_count(&self) -> usize {
        self.objects.len()
    }

    /// 返回类数量。
    pub(crate) fn class_count(&self) -> usize {
        self.classes.len()
    }

    /// 按对象 ID 返回连续对象下标。
    pub(crate) fn object_index(&self, object_id: HprofObjectId) -> Option<usize> {
        self.object_indices.get(&object_id).copied()
    }

    /// 判断对象 ID 是否存在。
    pub(crate) fn contains_object_id(&self, object_id: HprofObjectId) -> bool {
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
    pub(crate) fn class_names_for_result(&self) -> FxHashMap<HprofObjectId, String> {
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
