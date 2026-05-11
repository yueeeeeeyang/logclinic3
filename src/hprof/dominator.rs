//! HPROF dominator 算法子模块。
//!
//! 业务意图：
//! - 该模块集中维护 Lengauer-Tarjan immediate dominator 计算和算法热路径辅助函数。
//! - 父模块负责构建紧凑引用图和 UI 结果行，本模块只处理对象下标级支配关系。
//!
//! 边界条件：
//! - 算法会处理虚拟 Root、不可达对象和上百万对象的大图；数组索引必须继续使用低内存 `u32` 节点编号。
//! - 计算阶段必须保留原有取消检查和细分进度，避免大 dump 分析无法中断。

use super::*;

/// 使用 Lengauer-Tarjan 算法计算对象下标级 immediate dominator。
pub(super) fn compute_lengauer_tarjan_dominators<F>(
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
