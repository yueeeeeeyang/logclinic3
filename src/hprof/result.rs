//! HPROF dominator 结果展示辅助模块。
//!
//! 业务意图：
//! - 该模块集中维护 `HprofDominatorResult` 到 UI 可见行、节点展示名和线程详情查询的转换。
//! - 结果对象可能来自刚计算出的内存摘要，也可能来自 sidecar 缓存；这里统一处理懒加载和损坏缓存降级。
//!
//! 边界条件：
//! - 展开状态只影响可见行拼装，不改变 dominator 结果本身。
//! - sidecar 摘要读取失败时返回缺失行，避免渲染路径 panic。

use std::collections::HashSet;

use super::*;

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
    pub(super) fn summary_at_index(
        &self,
        summary_index: usize,
    ) -> Option<HprofDominatorObjectSummary> {
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
    pub(super) fn summary_count(&self) -> usize {
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
