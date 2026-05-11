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
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

#[path = "constants.rs"]
mod constants;
pub(crate) use self::constants::*;

#[path = "progress.rs"]
mod progress;
pub(crate) use self::progress::*;

#[path = "types.rs"]
mod types;
pub(crate) use self::types::*;

#[path = "domain.rs"]
mod domain;
pub(crate) use self::domain::*;

#[path = "result.rs"]
mod result;

/// HPROF sidecar 缓存读写。
///
/// 业务意图：
/// - 大 dump 的 dominator 计算成本很高；完成后把 UI 所需的紧凑结果写到源文件同目录，后续打开可以跳过解析和 LT 计算。
/// - manifest 使用 JSON 方便人工诊断，主体数据使用稳定手写二进制，避免 serde 对大 Vec 产生额外中间分配。
#[path = "sidecar.rs"]
mod hprof_cache;

#[path = "graph.rs"]
mod graph;
use self::graph::{HprofCompactAdjacency, HprofDominatorIndexResult};

#[path = "error.rs"]
mod error;
pub(crate) use self::error::HprofError;

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

    let graph =
        parser::parse_hprof_object_graph(&path, progress, &mut progress_reporter, &cancel_flag)?;
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

#[path = "input.rs"]
mod input;
use self::input::HprofInputBytes;

#[path = "parser.rs"]
mod parser;
use self::parser::{
    HprofStackFrameRecord, HprofStackTraceRecord, HprofThreadInstanceInfo,
    HprofThreadObjectRootRecord, HprofThreadStartRecord, RawClassInfo, RawFieldDescriptor,
    ResolvedReferenceField, ResolvedReferenceLayout,
};

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
    let adjacency = graph::build_compact_adjacency(&graph, cancel_flag, &mut stage_reporter)?;
    graph.clear_reference_pool();

    let dominator_indices = dominator::compute_lengauer_tarjan_dominators(
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
    let mut compact_children = graph::build_compact_children(
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
    let retained_sizes = graph::compute_retained_sizes_by_index(
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

    graph::sort_compact_children(&graph, &retained_sizes, &mut compact_children);

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
        thread_details::build_thread_details(&graph, &retained_sizes, &dominator_indices.reachable);

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

#[path = "thread_details.rs"]
mod thread_details;

#[path = "dominator.rs"]
mod dominator;

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
#[path = "core_tests.rs"]
mod tests;
