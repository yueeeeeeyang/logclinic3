//! HPROF dominator 图构建子模块。
//!
//! 业务意图：
//! - 该模块集中维护从对象图到低内存紧凑邻接表的转换、idom 子节点表构建和 retained size 聚合。
//! - parser 负责生成 `HprofObjectGraph`，dominator 算法负责计算 immediate dominator，本模块连接两者并保留进度上报。
//!
//! 边界条件：
//! - 大 dump 中对象和引用边数量可能达到百万级，偏移表继续使用紧凑连续数组，避免额外图结构复制。
//! - GC Root 去重、不可达对象、取消检查和阶段文案保持原行为不变。

use super::*;

/// 紧凑引用图，给 LT 算法使用。
///
/// 业务意图：
/// - dominator 计算只需要连续对象下标和出边目标；移除 petgraph 实体图可以避免为同一批节点和边额外复制一份内存。
pub(super) struct HprofCompactAdjacency {
    /// 虚拟 Root 指向的 GC Root 对象下标。
    pub(super) root_indices: Vec<HprofNodeId>,
    /// 每个对象出边在 `targets` 中的起始偏移。
    pub(super) offsets: Vec<HprofNodeId>,
    /// 所有对象出边目标对象下标。
    pub(super) targets: Vec<HprofNodeId>,
}

/// 连续对象下标上的紧凑 dominator 子节点表。
pub(super) struct HprofCompactChildren {
    /// 每个对象直接支配子节点在 `indices` 中的起始偏移。
    pub(super) offsets: Vec<usize>,
    /// 所有直接支配子节点对象下标。
    pub(super) indices: Vec<usize>,
    /// 由虚拟 Root 直接支配的对象下标。
    pub(super) root_children: Vec<usize>,
}

impl HprofCompactChildren {
    /// 返回指定对象下标的直接支配子节点切片。
    pub(super) fn children(&self, object_index: usize) -> &[usize] {
        let start = self.offsets.get(object_index).copied().unwrap_or(0);
        let end = self.offsets.get(object_index + 1).copied().unwrap_or(start);
        &self.indices[start..end]
    }

    /// 返回指定对象下标的直接支配子节点数量。
    pub(super) fn child_count(&self, object_index: usize) -> usize {
        self.children(object_index).len()
    }
}

/// LT 算法输出的对象下标级 dominator 结果。
pub(super) struct HprofDominatorIndexResult {
    /// 对象是否可从虚拟 Root 到达。
    pub(super) reachable: Vec<bool>,
    /// 对象的 immediate dominator；`None` 表示由虚拟 Root 直接支配或不可达。
    pub(super) parent_by_index: Vec<Option<usize>>,
}

/// 构建 LT 使用的紧凑邻接表。
pub(super) fn build_compact_adjacency<F>(
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

/// 根据 idom 结果构建紧凑子节点表。
pub(super) fn build_compact_children<F>(
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
pub(super) fn sort_compact_children(
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
pub(super) fn compute_retained_sizes_by_index<F>(
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
