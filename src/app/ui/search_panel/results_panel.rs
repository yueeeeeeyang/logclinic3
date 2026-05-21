// 主窗口搜索结果面板方法。
//
// 业务意图：
// - 搜索结果面板属于主窗口底部工作区，与搜索独立窗口分离；这里集中维护结果分组、右键菜单、滚动条和跳转行渲染。
// - 当前通过独立 `impl MainView` 物理拆分，保持搜索任务状态和结果面板状态的私有访问不变。
//
// 边界条件：
// - 本阶段不改变搜索结果截断、展开/收起、右键菜单、滚动条拖动或点击跳转行为。

use super::*;

impl MainView {
    pub(in crate::app) fn render_search_results_panel(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(panel) = &self.search.search_results_panel else {
            return div().id("search-results-panel-empty").hidden();
        };
        let row_count = panel.rows.len();
        let panel_height = panel.height;
        let scroll_handle = panel.scroll_handle.clone();
        let palette = self.palette();

        div()
            .id("search-results-panel")
            .relative()
            .h(px(panel_height))
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .shadow_lg()
            .on_mouse_move(
                context.listener(|view, _event: &MouseMoveEvent, _window, _context| {
                    view.note_keyboard_scroll_region(KeyboardScrollRegion::SearchResults);
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, _event: &MouseDownEvent, _window, _context| {
                    view.note_keyboard_scroll_region(KeyboardScrollRegion::SearchResults);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|view, event: &MouseDownEvent, _window, context| {
                    view.note_keyboard_scroll_region(KeyboardScrollRegion::SearchResults);
                    view.open_search_results_context_menu(
                        f32::from(event.position.x),
                        f32::from(event.position.y),
                        context,
                    );
                    // 搜索结果面板本身已经处理右键菜单，事件不能继续冒泡到右侧工作区的其它浮层逻辑。
                    context.stop_propagation();
                }),
            )
            .child(self.render_search_results_resizer(context))
            .child(self.render_search_results_header(panel, palette, context))
            .child(if row_count == 0 {
                self.render_search_results_empty(panel, palette)
            } else {
                div()
                    .id("search-results-list-wrapper")
                    .relative()
                    .flex()
                    .flex_1()
                    .w_full()
                    .overflow_hidden()
                    .child(
                        uniform_list(
                            "search-results-list",
                            row_count,
                            context.processor(
                                move |view, range: std::ops::Range<usize>, _window, context| {
                                    let rows = view
                                        .search
                                        .search_results_panel
                                        .as_ref()
                                        .map(|panel| {
                                            range
                                                .filter_map(|index| panel.rows.get(index).cloned())
                                                .collect::<Vec<_>>()
                                        })
                                        .unwrap_or_default();

                                    rows.into_iter()
                                        .map(|row| {
                                            view.render_search_results_panel_row(row, context)
                                        })
                                        .collect::<Vec<_>>()
                                },
                            ),
                        )
                        .size_full()
                        .track_scroll(scroll_handle.clone()),
                    )
                    .child(self.render_search_results_scrollbar(
                        &scroll_handle,
                        row_count,
                        palette,
                        context,
                    ))
            })
    }

    /// 渲染搜索结果面板的纵向可见滚动条。
    ///
    /// 业务意图：
    /// - 结果面板中的命中可能远多于可见区域，滚动条既提示当前位置，也提供直接拖动入口。
    /// - 滑块和结果虚拟列表共享同一个滚动句柄，避免维护第二份滚动状态。
    pub(in crate::app) fn render_search_results_scrollbar(
        &self,
        scroll_handle: &UniformListScrollHandle,
        row_count: usize,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(metrics) = Self::search_results_scrollbar_metrics(scroll_handle)
            .or_else(|| Self::fallback_search_results_scrollbar_metrics(row_count))
        else {
            return div().id("search-results-scrollbar-empty").hidden();
        };

        div()
            .id("search-results-scrollbar")
            .absolute()
            .top(metrics.thumb_start)
            .right(px(SEARCH_RESULTS_SCROLLBAR_PADDING))
            .w(px(SEARCH_RESULTS_SCROLLBAR_WIDTH))
            .h(metrics.thumb_length)
            .rounded(px(SEARCH_RESULTS_SCROLLBAR_WIDTH / 2.0))
            .bg(rgb(palette.scrollbar))
            .cursor_pointer()
            .hover(move |thumb| thumb.bg(rgb(palette.scrollbar_hover)))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, event: &MouseDownEvent, _window, context| {
                    view.start_search_results_scrollbar_drag(event, context);
                    context.notify();
                    // 滚动条是覆盖在结果列表上的拖拽控件，按下事件必须止步于滑块，避免触发列表或浮层关闭逻辑。
                    context.stop_propagation();
                }),
            )
    }

    /// 计算搜索结果面板纵向滚动条滑块位置和高度。
    ///
    /// 业务意图：
    /// - 使用搜索结果虚拟列表的真实测量结果，保证滚轮滚动、结果展开/收起和滑块位置同源。
    /// - 内容高度不超过视口时不显示滚动条，避免空态或少量结果出现无效控件。
    pub(in crate::app) fn search_results_scrollbar_metrics(
        scroll_handle: &UniformListScrollHandle,
    ) -> Option<LogScrollbarMetrics> {
        let state = scroll_handle.0.borrow();
        let size = state.last_item_size?;
        let viewport_height = size.item.height;
        let content_height = size.contents.height;
        if viewport_height <= px(0.0) || content_height <= viewport_height {
            return None;
        }

        let max_scroll = content_height - viewport_height;
        let scroll_top = (-state.base_handle.offset().y).clamp(px(0.0), max_scroll);
        let track_start = px(SEARCH_RESULTS_SCROLLBAR_PADDING);
        let track_length = (viewport_height - track_start * 2.0).max(px(1.0));
        let min_thumb_length = px(SEARCH_RESULTS_SCROLLBAR_MIN_THUMB_HEIGHT).min(track_length);
        let thumb_length = (viewport_height * (viewport_height / content_height))
            .clamp(min_thumb_length, track_length);
        let movable_length = (track_length - thumb_length).max(px(0.0));
        let thumb_start = track_start + movable_length * (scroll_top / max_scroll);

        Some(LogScrollbarMetrics {
            thumb_start,
            thumb_length,
            track_start,
            track_length,
            max_scroll,
            max_scroll_px: f64::from(max_scroll),
        })
    }

    /// 在搜索结果列表首帧尚未写入布局测量时提供临时滚动条提示。
    ///
    /// 业务意图：
    /// - 大量搜索结果刚渲染出来时，虚拟列表需要一帧后才有真实测量；临时滑块能立即提示结果区域可滚动。
    /// - 该结果只用于视觉提示，`max_scroll` 为 0，因此不会参与拖动换算；真实测量完成后会被替换。
    pub(in crate::app) fn fallback_search_results_scrollbar_metrics(
        row_count: usize,
    ) -> Option<LogScrollbarMetrics> {
        if row_count <= 8 {
            return None;
        }

        Some(LogScrollbarMetrics {
            thumb_start: px(SEARCH_RESULTS_SCROLLBAR_PADDING),
            thumb_length: px(SEARCH_RESULTS_SCROLLBAR_MIN_THUMB_HEIGHT),
            track_start: px(SEARCH_RESULTS_SCROLLBAR_PADDING),
            track_length: px(SEARCH_RESULTS_SCROLLBAR_MIN_THUMB_HEIGHT),
            max_scroll: px(0.0),
            max_scroll_px: 0.0,
        })
    }

    /// 将搜索历史记录展平成虚拟列表行。
    ///
    /// 业务意图：
    /// - 搜索历史记录头、命中结果和错误明细都需要在同一个可滚动区域内显示。
    /// - 仅为展开的记录生成明细行，可以让用户保留多次搜索历史而不被旧结果淹没。
    /// - 该函数只在结果数据或展开状态变化时调用，不能放到虚拟列表滚动渲染路径中反复执行。
    pub(in crate::app) fn search_results_panel_rows_from_records(
        records: &[SearchHistoryRecord],
    ) -> Vec<SearchResultsPanelRow> {
        let mut rows = Vec::new();
        for (record_index, record) in records.iter().enumerate().rev() {
            rows.extend(Self::search_results_panel_rows_for_record(
                record_index,
                record,
            ));
        }
        rows
    }

    /// 将单条搜索历史记录展平成虚拟列表行。
    ///
    /// 业务意图：
    /// - 单文件搜索结果返回时只需要替换对应历史记录的行段，不应重建整个结果面板缓存。
    /// - 该函数让完整重建和局部替换共享同一套行生成规则，避免展开状态或错误行顺序出现分歧。
    pub(in crate::app) fn search_results_panel_rows_for_record(
        record_index: usize,
        record: &SearchHistoryRecord,
    ) -> Vec<SearchResultsPanelRow> {
        let mut rows = vec![SearchResultsPanelRow::RecordHeader { record_index }];

        if !record.expanded {
            return rows;
        }

        if record.results.is_empty() && record.errors.is_empty() {
            rows.push(SearchResultsPanelRow::Empty { record_index });
            return rows;
        }

        for group in Self::search_result_file_groups(record) {
            let expanded = record.expanded_file_keys.contains(&group.source_key);
            rows.push(SearchResultsPanelRow::FileHeader {
                record_index,
                source_key: group.source_key,
                full_path: group.full_path,
                result_count: group.result_indices.len(),
            });
            if expanded {
                rows.extend(group.result_indices.into_iter().map(|result_index| {
                    SearchResultsPanelRow::Result {
                        record_index,
                        result_index,
                    }
                }));
            }
        }
        rows.extend(
            (0..record.errors.len()).map(|error_index| SearchResultsPanelRow::Error {
                record_index,
                error_index,
            }),
        );
        rows
    }

    /// 替换搜索结果面板中单条历史记录对应的行段。
    ///
    /// 业务意图：
    /// - 目录搜索每完成一个文件都会回到 UI 线程，如果每次都从全部历史记录重建 `rows`，大量结果会把追加更新放大成 O(n²)。
    /// - 行模型按记录连续排列，因此可以定位该记录的起止行后做局部 `splice`。
    ///
    /// 边界条件：
    /// - 如果当前缓存中找不到该记录行段，说明记录刚创建或缓存被重置，退回完整重建保证 UI 正确。
    pub(in crate::app) fn replace_search_results_panel_rows_for_record(
        panel: &mut SearchResultsPanelState,
        record_index: usize,
    ) {
        let Some(record) = panel.records.get(record_index) else {
            return;
        };
        let replacement = Self::search_results_panel_rows_for_record(record_index, record);
        let Some(start) = panel
            .rows
            .iter()
            .position(|row| Self::search_results_panel_row_record_index(row) == record_index)
        else {
            panel.rows = Self::search_results_panel_rows_from_records(&panel.records);
            return;
        };
        let end = start
            + panel.rows[start..]
                .iter()
                .take_while(|row| Self::search_results_panel_row_record_index(row) == record_index)
                .count();
        panel.rows.splice(start..end, replacement);
    }

    /// 返回虚拟列表行所属的搜索历史记录下标。
    fn search_results_panel_row_record_index(row: &SearchResultsPanelRow) -> usize {
        match row {
            SearchResultsPanelRow::RecordHeader { record_index }
            | SearchResultsPanelRow::FileHeader { record_index, .. }
            | SearchResultsPanelRow::Result { record_index, .. }
            | SearchResultsPanelRow::Error { record_index, .. }
            | SearchResultsPanelRow::Empty { record_index } => *record_index,
        }
    }

    /// 按文件来源对搜索命中进行稳定分组。
    ///
    /// 业务意图：
    /// - 同一文件的命中需要集中展示并可独立展开/收起。
    /// - 分组顺序按首次命中的顺序保留，避免每次渲染排序造成结果跳动。
    pub(in crate::app) fn search_result_file_groups(
        record: &SearchHistoryRecord,
    ) -> Vec<SearchResultFileGroup> {
        if !record.result_groups.is_empty() || record.results.is_empty() {
            return record.result_groups.clone();
        }

        let mut groups: Vec<SearchResultFileGroup> = Vec::new();

        for (result_index, result) in record.results.iter().enumerate() {
            if let Some(group) = groups
                .iter_mut()
                .find(|group| group.source_key == result.source_key)
            {
                group.result_indices.push(result_index);
                continue;
            }

            groups.push(SearchResultFileGroup {
                source_key: result.source_key.clone(),
                full_path: Self::search_result_source_full_path(&result.source),
                result_indices: vec![result_index],
            });
        }

        groups
    }

    /// 返回搜索结果来源的完整展示路径。
    ///
    /// 业务意图：
    /// - 文件分组行需要单行显示完整来源，帮助用户区分同名日志和压缩包内同名成员。
    /// - 这里仅用于 UI 展示；实际打开和定位仍依赖 `LogFileSource` 和稳定键。
    pub(in crate::app) fn search_result_source_full_path(source: &LogFileSource) -> String {
        match source {
            LogFileSource::LocalFile { path } => path.display().to_string(),
            LogFileSource::ArchiveMember {
                archive_path,
                member_path,
                ..
            } => format!("{}/{}", archive_path.display(), member_path),
            LogFileSource::MaterializedArchiveMember {
                archive_path,
                member_path,
                ..
            } => format!("{}/{}", archive_path.display(), member_path),
            LogFileSource::NestedArchiveMember {
                outer_archive_path,
                archive_member_path,
                nested_member_path,
                ..
            } => format!(
                "{}/{}/{}",
                outer_archive_path.display(),
                archive_member_path,
                nested_member_path
            ),
        }
    }

    /// 打开搜索结果面板右键菜单。
    ///
    /// 业务意图：
    /// - 右键菜单位置需要贴近用户点击处，便于在大量结果中快速执行批量展开/收起。
    /// - 菜单渲染在右侧工作区内部，因此需要把窗口坐标转换成右侧局部坐标。
    pub(in crate::app) fn open_search_results_context_menu(
        &mut self,
        window_x: f32,
        window_y: f32,
        context: &mut Context<Self>,
    ) {
        let panel_x = (window_x - self.right_panel_left_offset()).max(0.0);
        let panel_y = (window_y - TOOLBAR_HEIGHT).max(0.0);
        self.search.search_results_context_menu = Some(SearchResultsContextMenu {
            x: panel_x,
            y: panel_y,
        });
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        context.notify();
    }

    /// 渲染搜索结果面板中的一行。
    ///
    /// 边界条件：
    /// - 后台搜索回调可能在渲染帧之间改变记录数量，因此每一行都必须重新通过下标安全读取。
    pub(in crate::app) fn render_search_results_panel_row(
        &self,
        row: SearchResultsPanelRow,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(panel) = self.search.search_results_panel.as_ref() else {
            return div().id("search-results-row-missing-panel").hidden();
        };

        match row {
            SearchResultsPanelRow::RecordHeader { record_index } => {
                let Some(record) = panel.records.get(record_index) else {
                    return div().id("search-results-row-missing-record").hidden();
                };
                self.render_search_history_record_row(record_index, record, context)
            }
            SearchResultsPanelRow::FileHeader {
                record_index,
                source_key,
                full_path,
                result_count,
            } => self.render_search_file_group_row(
                record_index,
                source_key,
                full_path,
                result_count,
                context,
            ),
            SearchResultsPanelRow::Result {
                record_index,
                result_index,
            } => {
                let Some(result) = panel
                    .records
                    .get(record_index)
                    .and_then(|record| record.results.get(result_index))
                    .cloned()
                else {
                    return div().id("search-results-row-missing-result").hidden();
                };
                self.render_search_result_row(result, context)
            }
            SearchResultsPanelRow::Error {
                record_index,
                error_index,
            } => {
                let Some(error) = panel
                    .records
                    .get(record_index)
                    .and_then(|record| record.errors.get(error_index))
                    .cloned()
                else {
                    return div().id("search-results-row-missing-error").hidden();
                };
                self.render_search_error_row(record_index, error)
            }
            SearchResultsPanelRow::Empty { record_index } => {
                let Some(record) = panel.records.get(record_index) else {
                    return div().id("search-results-row-missing-empty").hidden();
                };
                self.render_search_record_empty_row(record_index, record)
            }
        }
    }

    /// 渲染搜索结果面板右键菜单。
    ///
    /// 业务意图：
    /// - 搜索结果支持多层展开，右键菜单提供批量操作，避免用户逐条点击文件分组。
    /// - 菜单风格与 tab 右键菜单保持一致，避免在同一应用中出现两套交互语言。
    pub(in crate::app) fn render_search_results_context_menu(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(menu) = &self.search.search_results_context_menu else {
            return div().id("search-results-context-menu-empty").hidden();
        };
        let palette = self.palette();

        div()
            .id("search-results-context-menu")
            .absolute()
            .left(px(menu.x))
            .top(px(menu.y))
            .w(px(SEARCH_RESULTS_CONTEXT_MENU_WIDTH))
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.menu))
            .shadow_lg()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 菜单容器包含上下内边距；点到空白区域时也不能把事件交给下层搜索结果行。
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 菜单上再次右键只应作用于当前菜单层，避免重新打开或切换底层面板菜单。
                    context.stop_propagation();
                }),
            )
            .child(self.render_search_results_context_menu_item(
                SearchResultsContextMenuAction::ExpandAll,
                "展开全部",
                palette,
                context,
            ))
            .child(self.render_search_results_context_menu_item(
                SearchResultsContextMenuAction::CollapseAll,
                "收起全部",
                palette,
                context,
            ))
    }

    /// 渲染搜索结果右键菜单单项。
    ///
    /// 业务意图：
    /// - 菜单项点击后立即执行批量操作并收起菜单，保持和 tab 菜单一致的即时反馈。
    pub(in crate::app) fn render_search_results_context_menu_item(
        &self,
        action: SearchResultsContextMenuAction,
        label: &'static str,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!("search-results-menu-{label}")))
            .flex()
            .items_center()
            .h(px(SEARCH_RESULTS_CONTEXT_MENU_ITEM_HEIGHT))
            .px_3()
            .text_sm()
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |item| item.bg(rgb(palette.hover)))
            .child(label)
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.handle_search_results_context_menu_action(action, context);
                    // 菜单项命令执行后不允许 click 继续冒泡，保持与日志正文、目录树菜单一致。
                    context.stop_propagation();
                }),
            )
    }

    /// 执行搜索结果右键菜单命令。
    ///
    /// 业务意图：
    /// - 展开/收起会影响历史记录和文件分组两层状态，集中处理可以保证行缓存同步重建。
    pub(in crate::app) fn handle_search_results_context_menu_action(
        &mut self,
        action: SearchResultsContextMenuAction,
        context: &mut Context<Self>,
    ) {
        match action {
            SearchResultsContextMenuAction::ExpandAll => self.expand_all_search_results(),
            SearchResultsContextMenuAction::CollapseAll => self.collapse_all_search_results(),
        }
        self.search.search_results_context_menu = None;
        context.notify();
    }

    /// 展开搜索结果面板内所有历史记录和文件分组。
    ///
    /// 业务意图：
    /// - 用户在需要快速浏览全部命中时，可以一次性展开所有层级，不必逐个文件打开。
    /// - 展开后重建虚拟列表行缓存，保持滚动路径仍为 O(可见行数)。
    pub(in crate::app) fn expand_all_search_results(&mut self) {
        let Some(panel) = self.search.search_results_panel.as_mut() else {
            return;
        };

        for record in &mut panel.records {
            record.expanded = true;
            record.expanded_file_keys = Self::search_result_file_groups(record)
                .into_iter()
                .map(|group| group.source_key)
                .collect();
        }
        panel.rows = Self::search_results_panel_rows_from_records(&panel.records);
    }

    /// 收起搜索结果面板内所有历史记录和文件分组。
    ///
    /// 业务意图：
    /// - 当搜索结果过多造成扫描困难时，用户可以一次回到只有历史摘要的紧凑视图。
    pub(in crate::app) fn collapse_all_search_results(&mut self) {
        let Some(panel) = self.search.search_results_panel.as_mut() else {
            return;
        };

        for record in &mut panel.records {
            record.expanded = false;
            record.expanded_file_keys.clear();
        }
        panel.rows = Self::search_results_panel_rows_from_records(&panel.records);
    }

    /// 渲染搜索结果面板顶部拖拽条。
    pub(in crate::app) fn render_search_results_resizer(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("search-results-resizer")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .h(px(SEARCH_RESULTS_PANEL_RESIZER_HEIGHT))
            .cursor_row_resize()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, event: &MouseDownEvent, _window, context| {
                    view.start_search_results_resize(event);
                    // 分隔拖拽条覆盖在面板顶部，启动拖拽后不能继续触发面板或日志区的点击处理。
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染搜索结果面板标题栏。
    pub(in crate::app) fn render_search_results_header(
        &self,
        panel: &SearchResultsPanelState,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let search_count = panel.records.len();
        let result_count: usize = panel
            .records
            .iter()
            .map(|record| record.results.len())
            .sum();
        let error_count: usize = panel.records.iter().map(|record| record.errors.len()).sum();
        let running_count = panel
            .records
            .iter()
            .filter(|record| {
                !record.canceled && record.progress.searched_files < record.progress.total_files
            })
            .count();
        let summary = format!(
            "{} 次搜索 · {} 条命中 · {} 个错误{}",
            search_count,
            result_count,
            error_count,
            if running_count == 0 {
                String::new()
            } else {
                format!(" · {} 个进行中", running_count)
            }
        );

        div()
            .id("search-results-header")
            .flex()
            .items_center()
            .justify_between()
            .h(px(
                SEARCH_RESULT_ROW_HEIGHT + SEARCH_RESULTS_PANEL_HEADER_TOP_PADDING
            ))
            .pt(px(SEARCH_RESULTS_PANEL_HEADER_TOP_PADDING))
            .px_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(Self::render_lucide_icon(
                        Some(Icon::ListFilter),
                        14.0,
                        14.0,
                        palette.muted_text,
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child("搜索结果"),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(rgb(palette.muted_text))
                            .child(summary),
                    ),
            )
            .child(
                div()
                    .id("search-results-close")
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(24.0))
                    .h(px(24.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(move |button| button.bg(rgb(palette.hover)))
                    .child(Self::render_lucide_icon(
                        Some(Icon::X),
                        13.0,
                        13.0,
                        palette.muted_text,
                    ))
                    .on_click(
                        context.listener(|view, _event: &ClickEvent, _window, context| {
                            view.search.search_results_panel = None;
                            view.search.search_results_resize_drag = None;
                            view.search.search_results_scrollbar_drag = None;
                            view.search.search_results_context_menu = None;
                            view.log.log_viewer_context_menu = None;
                            context.notify();
                            // 关闭按钮位于搜索结果标题栏内部，按钮点击不应继续传给标题栏或面板。
                            context.stop_propagation();
                        }),
                    ),
            )
    }

    /// 渲染搜索结果空态。
    pub(in crate::app) fn render_search_results_empty(
        &self,
        panel: &SearchResultsPanelState,
        palette: AppThemePalette,
    ) -> gpui::Stateful<gpui::Div> {
        let message = if panel.records.is_empty() {
            "暂无搜索记录"
        } else {
            "暂无可显示的搜索结果"
        };

        div()
            .id("search-results-empty")
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .flex_1()
            .text_sm()
            .text_color(rgb(palette.muted_text))
            .child(Self::render_lucide_icon(
                Some(Icon::SearchX),
                26.0,
                24.0,
                palette.muted_text,
            ))
            .child(message)
    }

    /// 渲染单条搜索历史记录摘要。
    ///
    /// 业务意图：
    /// - 历史记录行展示查询词、范围、目标目录和命中统计，点击可展开或收起明细。
    /// - 最新搜索默认展开，旧搜索保留但折叠，便于对比不同关键字而不占满结果区域。
    pub(in crate::app) fn render_search_history_record_row(
        &self,
        record_index: usize,
        record: &SearchHistoryRecord,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let match_label = match record.match_mode {
            SearchMatchMode::Regex => record.match_mode.label(),
            SearchMatchMode::Literal if record.case_sensitive => "区分大小写",
            SearchMatchMode::Literal => "忽略大小写",
        };
        let target_label = record
            .directory_target
            .as_ref()
            .filter(|target| !target.is_empty())
            .map(|target| format!(" · {}", target))
            .unwrap_or_default();
        let summary = format!(
            "{}{} · {} · {} · {}/{} 文件",
            record.scope.label(),
            target_label,
            match_label,
            record.state_label(),
            record.progress.searched_files,
            record.progress.total_files
        );
        let count_label = format!(
            "{} 命中 · {} 错误",
            record.results.len(),
            record.errors.len()
        );
        let expanded = record.expanded;
        let palette = self.palette();

        div()
            .id(SharedString::from(format!(
                "search-history-record-{record_index}"
            )))
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .h(px(SEARCH_RESULT_ROW_HEIGHT))
            .px_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(if expanded {
                palette.selected
            } else {
                palette.surface
            }))
            .cursor_pointer()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .child(Self::render_lucide_icon(
                Some(if expanded {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                }),
                14.0,
                14.0,
                palette.muted_text,
            ))
            .child(
                div()
                    .min_w_0()
                    .w(px(280.0))
                    .flex_none()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child(record.query.clone()),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_xs()
                    .text_color(rgb(palette.muted_text))
                    .child(summary),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .text_xs()
                    .text_color(rgb(palette.muted_text))
                    .child(count_label),
            )
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    if let Some(panel) = view.search.search_results_panel.as_mut() {
                        let updated = if let Some(record) = panel.records.get_mut(record_index) {
                            record.expanded = !record.expanded;
                            true
                        } else {
                            false
                        };
                        if updated {
                            Self::replace_search_results_panel_rows_for_record(panel, record_index);
                        }
                    }
                    context.notify();
                }),
            )
    }

    /// 渲染某次搜索记录下的文件分组行。
    ///
    /// 业务意图：
    /// - 文件分组行承载“这个文件有多少命中”的摘要，并提供展开/收起入口。
    /// - 命中明细行只显示行号和预览，避免每条结果重复展示同一个文件名。
    pub(in crate::app) fn render_search_file_group_row(
        &self,
        record_index: usize,
        source_key: String,
        full_path: String,
        result_count: usize,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let expanded = self
            .search
            .search_results_panel
            .as_ref()
            .and_then(|panel| panel.records.get(record_index))
            .is_some_and(|record| record.expanded_file_keys.contains(&source_key));
        let source_key_for_click = source_key.clone();
        let palette = self.palette();

        div()
            .id(SharedString::from(format!(
                "search-file-group-{record_index}-{}",
                source_key
            )))
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .h(px(SEARCH_RESULT_ROW_HEIGHT))
            .pl(px(30.0))
            .pr_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .cursor_pointer()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .child(Self::render_lucide_icon(
                Some(if expanded {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                }),
                13.0,
                13.0,
                palette.muted_text,
            ))
            .child(Self::render_lucide_icon(
                Some(Icon::FileText),
                14.0,
                14.0,
                palette.muted_text,
            ))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_sm()
                    .text_color(rgb(palette.text))
                    .child(full_path),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(rgb(palette.muted_text))
                    .child(format!("{result_count} 条")),
            )
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    if let Some(panel) = view.search.search_results_panel.as_mut() {
                        let updated = if let Some(record) = panel.records.get_mut(record_index) {
                            if !record.expanded_file_keys.remove(&source_key_for_click) {
                                record
                                    .expanded_file_keys
                                    .insert(source_key_for_click.clone());
                            }
                            true
                        } else {
                            false
                        };
                        if updated {
                            Self::replace_search_results_panel_rows_for_record(panel, record_index);
                        }
                    }
                    context.notify();
                }),
            )
    }

    /// 渲染单条搜索结果。
    ///
    /// 业务意图：
    /// - 文件名由上层文件分组展示，结果行只展示行号和命中预览；点击后打开对应文件并滚动到命中行。
    /// - 命中片段只用文字颜色高亮，保持和日志正文高亮策略一致。
    pub(in crate::app) fn render_search_result_row(
        &self,
        result: SearchResultItem,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (preview_text, preview_match_range) = Self::search_result_preview_text_and_range(
            &result.line_text,
            result.match_range.clone(),
        );
        let palette = self.palette();
        let highlight_style = gpui::HighlightStyle {
            color: Some(rgb(palette.error).into()),
            font_weight: Some(FontWeight::SEMIBOLD),
            background_color: None,
            ..Default::default()
        };
        let highlights = vec![(preview_match_range, highlight_style)];
        let line_number = result.line_index + 1;
        let result_for_click = result.clone();

        div()
            .id(SharedString::from(format!(
                "search-result-{}-{}",
                result.source_key, line_number
            )))
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .h(px(SEARCH_RESULT_ROW_HEIGHT))
            .pl(px(58.0))
            .pr_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .cursor_pointer()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .child(
                div()
                    .flex_none()
                    .w(px(48.0))
                    .text_right()
                    .text_xs()
                    .text_color(rgb(palette.muted_text))
                    .child(line_number.to_string()),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(1.0))
                    .h(px(18.0))
                    .bg(rgb(palette.border)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(self.settings.log_viewer_font_size))
                    .font_family(LOG_VIEWER_FONT_FAMILY)
                    .text_color(rgb(palette.text))
                    .child(StyledText::new(preview_text).with_highlights(highlights)),
            )
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.open_search_result(result_for_click.clone(), context);
                }),
            )
    }

    /// 生成搜索结果预览文本和对应的命中高亮范围。
    ///
    /// 业务意图：
    /// - 结果面板只用于快速扫描命中上下文，行首缩进和行尾空白会占用空间并造成视觉噪声，因此展示时去掉前后空白。
    /// - 跳转、复制和真实搜索结果仍使用 `SearchResultItem` 中的原始行文本与原始命中范围，避免改变业务定位语义。
    ///
    /// 边界条件：
    /// - `trim` 可能移除中文前后的 ASCII 或 Unicode 空白，命中范围必须按移除的 UTF-8 字节数平移。
    /// - 正常搜索命中一定在非空查询上，因此 trim 后命中不应为空；若遇到异常范围，回退到空范围，避免 `StyledText` 越界。
    pub(in crate::app) fn search_result_preview_text_and_range(
        line_text: &str,
        match_range: Range<usize>,
    ) -> (String, Range<usize>) {
        let trimmed_start = line_text.trim_start();
        let leading_bytes = line_text.len() - trimmed_start.len();
        let trimmed = trimmed_start.trim_end();
        let preview_text = trimmed.to_string();
        let preview_len = preview_text.len();

        let start = match_range
            .start
            .saturating_sub(leading_bytes)
            .min(preview_len);
        let end = match_range
            .end
            .saturating_sub(leading_bytes)
            .min(preview_len);
        let range = if start <= end { start..end } else { 0..0 };

        (preview_text, range)
    }

    /// 渲染搜索中的单文件错误。
    ///
    /// 业务意图：
    /// - 目录搜索不能因为单个文件读取失败而中断；错误行让用户知道哪些文件没有被覆盖。
    pub(in crate::app) fn render_search_error_row(
        &self,
        record_index: usize,
        error: SearchFileError,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = self.palette();

        div()
            .id(SharedString::from(format!(
                "search-error-{record_index}-{}",
                error.file_name
            )))
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .h(px(SEARCH_RESULT_ROW_HEIGHT))
            .pl(px(32.0))
            .pr_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(Self::render_lucide_icon(
                Some(Icon::FileX),
                14.0,
                14.0,
                palette.error,
            ))
            .child(
                div()
                    .w(px(220.0))
                    .flex_none()
                    .truncate()
                    .text_sm()
                    .text_color(rgb(palette.text))
                    .child(error.file_name),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(rgb(palette.error))
                    .child(error.message),
            )
    }

    /// 渲染展开搜索记录时的空态明细行。
    pub(in crate::app) fn render_search_record_empty_row(
        &self,
        record_index: usize,
        record: &SearchHistoryRecord,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = self.palette();
        let message = if record.canceled {
            "搜索已取消，取消前没有产生匹配结果"
        } else if record.progress.searched_files < record.progress.total_files {
            "正在搜索，请稍候..."
        } else {
            "没有找到匹配结果"
        };

        div()
            .id(SharedString::from(format!(
                "search-record-empty-{record_index}"
            )))
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .h(px(SEARCH_RESULT_ROW_HEIGHT))
            .pl(px(32.0))
            .pr_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .text_sm()
            .text_color(rgb(palette.muted_text))
            .child(Self::render_lucide_icon(
                Some(Icon::SearchX),
                14.0,
                14.0,
                palette.muted_text,
            ))
            .child(message)
    }

    /// 渲染弹层关闭遮罩。
    ///
    /// 业务意图：
    /// - 编码下拉框或 tab 右键菜单打开后，用户点击右侧工作区其它位置应关闭弹层。
    /// - 遮罩放在内容之上、菜单之下，既能接收空白区域点击，又不会挡住菜单项点击。
    pub(in crate::app) fn render_popup_dismiss_overlay(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if self.log.tab_context_menu.is_none()
            && self.log.encoding_dropdown_menu.is_none()
            && self.search.search_results_context_menu.is_none()
            && self.log.log_viewer_context_menu.is_none()
        {
            return div().id("popup-dismiss-overlay-empty").hidden();
        }

        div()
            .id("popup-dismiss-overlay")
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 透明遮罩位于内容之上、菜单之下；先吃掉按下事件，避免底层日志行开始选择或滚动条开始拖动。
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    // 右键通常不会产生普通 click；这里直接关闭浮层并截断，防止右键穿透到底层重新打开其它菜单。
                    view.log.tab_context_menu = None;
                    view.log.encoding_dropdown_menu = None;
                    view.search.search_results_context_menu = None;
                    view.log.log_viewer_context_menu = None;
                    context.notify();
                    context.stop_propagation();
                }),
            )
            .on_click(
                context.listener(|view, _event: &ClickEvent, _window, context| {
                    view.log.tab_context_menu = None;
                    view.log.encoding_dropdown_menu = None;
                    view.search.search_results_context_menu = None;
                    view.log.log_viewer_context_menu = None;
                    context.notify();
                    // click 也必须在遮罩层截止，避免同一次点击落到遮罩后方的新目标。
                    context.stop_propagation();
                }),
            )
    }
}
