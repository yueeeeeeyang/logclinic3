// 左侧日志目录树、树菜单和另存为方法。
//
// 业务意图：
// - 该文件集中维护左侧树渲染、多选、展开/收起、右键菜单、另存为冲突检测和保存任务。
// - 当前通过独立 `impl MainView` 保持行为不变，同时把目录树功能域从应用根文件中分离出来。
//
// 边界条件：
// - 本阶段不改变单击打开、目录展开、多选规则、压缩包节点处理或另存为跳过/覆盖策略。

use super::*;

impl MainView {
    pub(super) fn render_log_tree_panel(&self, context: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette();

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(palette.panel))
            .child(self.render_log_tree_header())
            .child(self.render_log_tree_body(context))
            .child(self.render_log_tree_context_menu_dismiss_overlay(context))
            .child(self.render_log_tree_context_menu(context))
    }

    /// 渲染左侧日志目录树标题。
    ///
    /// 业务意图：
    /// - 用户要求去除“日志目录”文字，因此左侧只保留目录树图标作为轻量语义提示。
    /// - 右侧摘要展示真实加载结果的节点规模和错误数量，方便用户确认加载范围。
    ///
    /// 边界条件：
    /// - 当前标题不显示真实绝对路径，避免在路径脱敏和悬浮提示规则未定义前挤压窄面板。
    pub(super) fn render_log_tree_header(&self) -> impl IntoElement {
        let palette = self.palette();
        let summary = match &self.log.load_state {
            LogTreeLoadState::Loaded(tree_state) => tree_state.summary().to_string(),
            LogTreeLoadState::Empty
            | LogTreeLoadState::Loading { .. }
            | LogTreeLoadState::Failed { .. } => String::new(),
        };

        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(LOG_TREE_HEADER_HEIGHT))
            .px(px(LOG_TREE_ROW_HORIZONTAL_PADDING))
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(px(LOG_TREE_FONT_SIZE))
                    .text_color(rgb(palette.text))
                    .child(Self::render_lucide_icon(
                        Some(Icon::FolderTree),
                        LOG_TREE_ITEM_ICON_WIDTH,
                        LOG_TREE_ITEM_ICON_SIZE,
                        palette.muted_text,
                    )),
            )
            .child(
                div()
                    .text_size(px(LOG_TREE_FONT_SIZE))
                    .text_color(rgb(palette.muted_text))
                    .truncate()
                    .child(summary),
            )
    }

    /// 渲染左侧日志目录树主体区域。
    ///
    /// 业务意图：
    /// - 只渲染真实加载完成后的目录树；未加载、加载中或加载失败时左侧栏不会显示。
    /// - 文件数量较多时使用 GPUI `uniform_list` 只渲染当前可见区间，避免滚动时为所有节点创建元素。
    ///
    /// 边界条件：
    /// - 目录树行高固定为 `LOG_TREE_ROW_HEIGHT`，符合 `uniform_list` 对等高元素的要求。
    /// - 虚拟列表数量来自当前可见行缓存，展开/收起后会重新计算并驱动列表更新。
    pub(super) fn render_log_tree_body(&self, context: &mut Context<Self>) -> impl IntoElement {
        let visible_row_count = match &self.log.load_state {
            LogTreeLoadState::Loaded(tree_state) => tree_state.visible_rows.len(),
            LogTreeLoadState::Empty
            | LogTreeLoadState::Loading { .. }
            | LogTreeLoadState::Failed { .. } => 0,
        };

        div()
            .id("log-tree-body")
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .overflow_hidden()
            .on_mouse_move(
                context.listener(|view, _event: &MouseMoveEvent, _window, _context| {
                    view.note_keyboard_scroll_region(KeyboardScrollRegion::LogTree);
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, _event: &MouseDownEvent, _window, _context| {
                    view.note_keyboard_scroll_region(KeyboardScrollRegion::LogTree);
                }),
            )
            .child(
                uniform_list(
                    "log-tree-virtual-list",
                    visible_row_count,
                    context.processor(|view, range: std::ops::Range<usize>, _window, context| {
                        // 先复制当前可见区间的数据，再渲染元素，避免同时持有 `load_state` 的不可变借用和
                        // 需要注册点击监听的可变 `Context`，这是 Rust 借用规则下最清晰的分界。
                        let range_start = range.start;
                        let rows: Vec<LoadedLogTreeRow> = match &view.log.load_state {
                            LogTreeLoadState::Loaded(tree_state) => range
                                .filter_map(|index| tree_state.visible_rows.get(index).cloned())
                                .collect(),
                            LogTreeLoadState::Empty
                            | LogTreeLoadState::Loading { .. }
                            | LogTreeLoadState::Failed { .. } => Vec::new(),
                        };

                        rows.iter()
                            .enumerate()
                            .map(|(offset, row)| {
                                view.render_loaded_log_tree_row(range_start + offset, row, context)
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .size_full()
                .track_scroll(self.log.log_tree_scroll_handle.clone()),
            )
            .child(self.render_log_tree_scrollbar(visible_row_count, context))
    }

    /// 渲染左侧目录树的纵向可见滚动条。
    ///
    /// 业务意图：
    /// - 左侧目录树文件较多时必须显示滚动位置，并支持鼠标拖动快速定位。
    /// - 滚动条复用目录树虚拟列表的 `UniformListScrollHandle`，确保滚轮滚动、虚拟渲染和滑块位置保持同源。
    ///
    /// 边界条件：
    /// - 只有内容高度超过视口时显示；首帧尚未完成测量但节点明显较多时，会显示一个临时滑块提示可滚动。
    pub(super) fn render_log_tree_scrollbar(
        &self,
        visible_row_count: usize,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(metrics) = Self::log_tree_scrollbar_metrics(&self.log.log_tree_scroll_handle)
            .or_else(|| Self::fallback_log_tree_scrollbar_metrics(visible_row_count))
        else {
            return div().id("log-tree-scrollbar-empty").hidden();
        };

        div()
            .id("log-tree-scrollbar")
            .absolute()
            .top(metrics.thumb_start)
            .right(px(LOG_TREE_SCROLLBAR_PADDING))
            .w(px(LOG_TREE_SCROLLBAR_WIDTH))
            .h(metrics.thumb_length)
            .rounded(px(LOG_TREE_SCROLLBAR_WIDTH / 2.0))
            .bg(rgb(self.palette().scrollbar))
            .cursor_pointer()
            .hover({
                let palette = self.palette();
                move |thumb| thumb.bg(rgb(palette.scrollbar_hover))
            })
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, event: &MouseDownEvent, _window, context| {
                    view.start_log_tree_scrollbar_drag(event);
                    context.notify();
                    // 目录树滚动条覆盖在节点列表上，拖动入口必须消费按下事件，避免误选中背后的节点。
                    context.stop_propagation();
                }),
            )
    }

    /// 计算左侧目录树滚动条滑块位置和高度。
    ///
    /// 业务意图：
    /// - 使用虚拟列表上一次布局记录的视口高度、内容高度和滚动偏移计算滑块，让滚轮滚动和滑块位置同步。
    /// - 目录树行高固定，因此 `UniformListScrollHandle` 的测量结果可以稳定反映完整内容高度。
    pub(super) fn log_tree_scrollbar_metrics(
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
        let track_start = px(LOG_TREE_SCROLLBAR_PADDING);
        let track_length = (viewport_height - track_start * 2.0).max(px(1.0));
        let min_thumb_length = px(LOG_TREE_SCROLLBAR_MIN_THUMB_HEIGHT).min(track_length);
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

    /// 在目录树首帧尚未完成测量时提供临时滚动条提示。
    ///
    /// 业务意图：
    /// - 大目录刚加载完成时，虚拟列表需要一帧后才写入真实测量；临时滑块可以立即告诉用户左侧列表可滚动。
    /// - 该结果只用于视觉提示，真实布局完成后会被 `log_tree_scrollbar_metrics` 替换。
    pub(super) fn fallback_log_tree_scrollbar_metrics(
        visible_row_count: usize,
    ) -> Option<LogScrollbarMetrics> {
        if visible_row_count <= 24 {
            return None;
        }

        Some(LogScrollbarMetrics {
            thumb_start: px(LOG_TREE_SCROLLBAR_PADDING),
            thumb_length: px(LOG_TREE_SCROLLBAR_MIN_THUMB_HEIGHT),
            track_start: px(LOG_TREE_SCROLLBAR_PADDING),
            track_length: px(LOG_TREE_SCROLLBAR_MIN_THUMB_HEIGHT),
            max_scroll: px(0.0),
            max_scroll_px: 0.0,
        })
    }

    /// 渲染真实加载结果中的单行节点。
    ///
    /// 业务意图：
    /// - 根据加载模块提供的节点类型选择图标、颜色和展开占位，保持 UI 逻辑不反向解析文件名。
    /// - 文件夹和压缩包节点支持点击展开/收起，普通文件节点支持点击打开到右侧 tab。
    /// - 真实结果只展示名称和短元信息，不展示绝对路径或压缩包内部完整路径。
    ///
    /// 边界条件：
    /// - 展开状态由 `LoadedLogTreeState` 保存；行号来自虚拟列表可见区间，不能作为展开键使用。
    /// - 错误详情暂不展开显示，只保存在加载结果中，后续可接入悬浮提示或状态面板。
    pub(super) fn render_loaded_log_tree_row(
        &self,
        visible_index: usize,
        row: &LoadedLogTreeRow,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        // 性能约束：虚拟列表渲染会频繁调用本函数，只有压缩包根节点才需要扫描子树判断是否可直接打开。
        // 普通文件、目录和错误节点不能进入该线性扫描路径，否则大目录滚动时会把每行渲染放大为 O(整棵树)。
        let direct_archive_source = if row.kind == LogTreeEntryKind::Archive {
            self.single_file_archive_source(row.id)
        } else {
            None
        };
        let row_source = row.source.clone().or(direct_archive_source);
        let can_toggle = row.has_children && row_source.is_none();
        let is_expanded = self.is_log_tree_node_expanded(row.id);
        let expand_icon = if can_toggle {
            Some(if is_expanded {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            })
        } else {
            None
        };
        let palette = self.palette();
        let (item_icon, icon_color) = Self::loaded_log_tree_icon(row.kind, palette);

        Self::render_log_tree_row(
            LogTreeRowRenderData {
                node_id: row.id,
                depth: row.depth,
                expand_icon,
                item_icon,
                icon_color,
                can_toggle,
                source: row_source,
                visible_index,
                selected: self.log.log_tree_selected_node_ids.contains(&row.id),
                label: row.label.clone(),
                meta: row.meta.clone(),
            },
            palette,
            self.effective_theme(),
            context,
        )
    }

    /// 渲染左侧日志目录树中的通用单行节点。
    ///
    /// 业务意图：
    /// - 真实节点共享同一行模板，避免文本截断、缩进和元信息样式分叉。
    /// - 可展开节点在这里绑定展开事件，可打开文件节点在这里绑定右侧 tab 打开事件。
    /// - 行根节点必须占满虚拟列表宽度，让 hover 激活态覆盖整行，而不是只包住文件名和大小文本。
    ///
    /// 边界条件：
    /// - 文件来源来自加载层，不从展示文案反推真实路径，避免目录同名或压缩包路径分隔符差异导致误读。
    pub(super) fn render_log_tree_row(
        row_data: LogTreeRowRenderData,
        palette: AppThemePalette,
        theme: EffectiveTheme,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let LogTreeRowRenderData {
            node_id,
            depth,
            expand_icon,
            item_icon,
            icon_color,
            can_toggle,
            source,
            visible_index,
            selected,
            label,
            meta,
        } = row_data;
        let left_padding = LOG_TREE_ROW_HORIZONTAL_PADDING + depth as f32 * LOG_TREE_ROW_INDENT;
        let source_for_left_click = source.clone();
        let source_for_right_click = source.clone();
        let background = if selected {
            palette.selected
        } else {
            palette.panel
        };
        let hover_background = Self::log_tree_row_hover_background(selected, theme);

        div()
            .id(SharedString::from(format!("log-tree-row-{}", node_id)))
            .flex()
            .items_center()
            .gap_1()
            .h(px(LOG_TREE_ROW_HEIGHT))
            .w_full()
            .min_w_0()
            .pl(px(left_padding))
            .pr(px(LOG_TREE_ROW_HORIZONTAL_PADDING))
            .text_size(px(LOG_TREE_FONT_SIZE))
            .text_color(rgb(if selected {
                palette.accent
            } else {
                palette.text
            }))
            .bg(rgb(background))
            .cursor_pointer()
            .hover(move |tree_row| tree_row.bg(rgb(hover_background)))
            .child(Self::render_lucide_icon(
                expand_icon,
                LOG_TREE_CHEVRON_WIDTH,
                LOG_TREE_CHEVRON_SIZE,
                palette.muted_text,
            ))
            .child(Self::render_lucide_icon(
                Some(item_icon),
                LOG_TREE_ITEM_ICON_WIDTH,
                LOG_TREE_ITEM_ICON_SIZE,
                icon_color,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .flex_1()
                    .min_w_0()
                    .child(div().min_w_0().truncate().child(label))
                    .child(Self::render_log_tree_meta(meta.as_deref(), palette)),
            )
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.handle_log_tree_left_mouse_down(
                        node_id,
                        visible_index,
                        source_for_left_click.clone(),
                        can_toggle,
                        event,
                        context,
                    );
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.open_log_tree_context_menu(
                        node_id,
                        visible_index,
                        source_for_right_click.clone(),
                        f32::from(event.position.x),
                        f32::from(event.position.y),
                        event,
                        context,
                    );
                }),
            )
    }

    /// 返回左侧目录树行的悬浮背景色。
    ///
    /// 业务意图：
    /// - 左侧树面板本身使用浅灰/深灰背景，通用 hover 色在明亮主题下和面板背景过于接近，会导致用户看不清鼠标悬浮行。
    /// - 选中行和普通行分别使用更明确的悬浮色，保证“当前选中”和“鼠标所在”两个状态都能被辨认。
    pub(super) fn log_tree_row_hover_background(selected: bool, theme: EffectiveTheme) -> u32 {
        match (theme, selected) {
            // 左侧树 hover 需要比面板背景更明确，但不能过亮或过饱和，否则长时间扫目录会刺眼。
            (EffectiveTheme::Light, false) => 0xe8edf3,
            (EffectiveTheme::Light, true) => 0xd8ebff,
            (EffectiveTheme::Dark, false) => 0x262d35,
            (EffectiveTheme::Dark, true) => 0x1e4562,
        }
    }

    /// 处理左侧目录树左键按下。
    ///
    /// 业务意图：
    /// - 普通单击会选中并立即打开日志，或展开/收起目录和多文件压缩包节点。
    /// - Shift 和 Ctrl/Command 多选都基于当前可见行顺序，符合常见文件树操作习惯。
    ///
    /// 边界条件：
    /// - 错误节点或不可打开节点也允许选中，方便用户保持视觉上下文；后续文件操作会只筛选可读取来源。
    /// - 带修饰键的点击只更新多选集合，不触发打开或展开，避免用户批量选择时意外切换右侧日志。
    /// - 双击事件的第二次按下不再重复执行主动作，避免目录被“展开后立刻收起”。
    pub(super) fn handle_log_tree_left_mouse_down(
        &mut self,
        node_id: usize,
        visible_index: usize,
        source: Option<LogFileSource>,
        can_toggle: bool,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        self.note_keyboard_scroll_region(KeyboardScrollRegion::LogTree);
        self.update_log_tree_selection_for_click(
            node_id,
            visible_index,
            event.modifiers.shift,
            event.modifiers.control || event.modifiers.platform,
        );
        self.log.log_tree_context_menu = None;
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
        self.search.search_results_context_menu = None;

        match Self::log_tree_primary_action_for_click(
            source.is_some(),
            can_toggle,
            event.modifiers.shift,
            event.modifiers.control || event.modifiers.platform,
            event.click_count,
        ) {
            LogTreePrimaryClickAction::OpenSource => {
                let Some(source) = source else {
                    context.notify();
                    return;
                };
                self.open_log_file(source, context);
            }
            LogTreePrimaryClickAction::ToggleNode => {
                self.toggle_log_tree_node(node_id, context);
            }
            LogTreePrimaryClickAction::None => {}
        }
        context.notify();
    }

    /// 计算目录树普通点击是否要触发打开或展开。
    ///
    /// 业务意图：
    /// - 用户要求日志和目录都从双击改为单击触发，但左侧树仍要保留多选能力。
    /// - 只有第一次普通左键按下会执行主动作；双击产生的第二次事件会被忽略，避免目录状态来回翻转。
    ///
    /// 边界条件：
    /// - 同时存在文件来源和可展开子节点时优先打开文件，用于单文件压缩包按文件本身处理的场景。
    /// - Shift、Ctrl、Command 任一修饰键存在时不触发主动作，确保多选行为只改变选择集合。
    pub(super) fn log_tree_primary_action_for_click(
        has_source: bool,
        can_toggle: bool,
        shift: bool,
        multi_select_modifier: bool,
        click_count: usize,
    ) -> LogTreePrimaryClickAction {
        if shift || multi_select_modifier || click_count != 1 {
            return LogTreePrimaryClickAction::None;
        }
        if has_source {
            LogTreePrimaryClickAction::OpenSource
        } else if can_toggle {
            LogTreePrimaryClickAction::ToggleNode
        } else {
            LogTreePrimaryClickAction::None
        }
    }

    /// 根据鼠标点击和修饰键更新目录树选择集合。
    ///
    /// 业务意图：
    /// - 将多选规则拆成纯状态逻辑，避免渲染事件中混入范围计算细节。
    /// - Ctrl/Command 点击用于增删单个节点，Shift 点击用于从锚点到当前行的连续选择。
    pub(super) fn update_log_tree_selection_for_click(
        &mut self,
        node_id: usize,
        visible_index: usize,
        shift: bool,
        additive: bool,
    ) {
        let visible_node_ids = self.visible_log_tree_node_ids();
        Self::apply_log_tree_selection_click(
            &mut self.log.log_tree_selected_node_ids,
            &mut self.log.log_tree_selection_anchor,
            &visible_node_ids,
            node_id,
            visible_index,
            shift,
            additive,
        );
    }

    /// 应用左侧树选择规则。
    ///
    /// 边界条件：
    /// - Shift 点击但锚点已经不可见时，退化为普通单击，避免选择隐藏折叠节点。
    /// - Ctrl/Command 与 Shift 同时按下时保留既有选择并追加范围，符合多数桌面文件管理器行为。
    pub(super) fn apply_log_tree_selection_click(
        selected_node_ids: &mut HashSet<usize>,
        selection_anchor: &mut Option<usize>,
        visible_node_ids: &[usize],
        node_id: usize,
        visible_index: usize,
        shift: bool,
        additive: bool,
    ) {
        if shift {
            let anchor_index = selection_anchor
                .and_then(|anchor_id| visible_node_ids.iter().position(|id| *id == anchor_id))
                .unwrap_or(visible_index);
            if !additive {
                selected_node_ids.clear();
            }
            let start = anchor_index.min(visible_index);
            let end = anchor_index.max(visible_index);
            for id in visible_node_ids.iter().skip(start).take(end - start + 1) {
                selected_node_ids.insert(*id);
            }
            return;
        }

        *selection_anchor = Some(node_id);
        if additive {
            if !selected_node_ids.remove(&node_id) {
                selected_node_ids.insert(node_id);
            }
        } else {
            selected_node_ids.clear();
            selected_node_ids.insert(node_id);
        }
    }

    /// 返回当前可见目录树行的节点 ID。
    ///
    /// 业务意图：
    /// - Shift 多选只能覆盖当前用户可见的连续行，折叠隐藏的子节点不参与范围计算。
    pub(super) fn visible_log_tree_node_ids(&self) -> Vec<usize> {
        match &self.log.load_state {
            LogTreeLoadState::Loaded(tree_state) => {
                tree_state.visible_rows.iter().map(|row| row.id).collect()
            }
            LogTreeLoadState::Empty
            | LogTreeLoadState::Loading { .. }
            | LogTreeLoadState::Failed { .. } => Vec::new(),
        }
    }

    /// 打开左侧目录树右键菜单。
    ///
    /// 业务意图：
    /// - 右键已选中文件时保留当前多选集合；右键未选中行时先把该行切换为唯一选择。
    /// - 菜单命令随后统一作用于当前选择中的可读取文件，目录和错误节点会被自动忽略。
    /// - 鼠标事件坐标来自主窗口，左侧固定大导航不属于目录树面板，定位菜单时必须先扣除导航宽度。
    /// - 参数直接来自鼠标事件和树节点快照，保持显式传入可以避免菜单打开时再次按 ID 查询易变行。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn open_log_tree_context_menu(
        &mut self,
        node_id: usize,
        visible_index: usize,
        source: Option<LogFileSource>,
        window_x: f32,
        window_y: f32,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        self.note_keyboard_scroll_region(KeyboardScrollRegion::LogTree);
        if !self.log.log_tree_selected_node_ids.contains(&node_id) {
            self.update_log_tree_selection_for_click(
                node_id,
                visible_index,
                event.modifiers.shift,
                event.modifiers.control || event.modifiers.platform,
            );
        }
        self.log.log_tree_context_menu = Some(LogTreeContextMenu {
            node_id,
            source,
            x: Self::log_tree_context_menu_x(window_x, self.left_panel_width),
            y: (window_y - TOOLBAR_HEIGHT).max(0.0),
        });
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.search.search_results_context_menu = None;
        context.notify();
    }

    /// 渲染左侧目录树右键菜单。
    ///
    /// 业务意图：
    /// - 菜单提供面向文件集合的操作；视觉上跟随当前主题，行为上不依赖平台系统菜单。
    /// - 即使右键落在目录节点上，菜单仍展示，但命令执行时只处理当前选择中的文件来源。
    pub(super) fn render_log_tree_context_menu(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(menu) = &self.log.log_tree_context_menu else {
            return div().id("log-tree-context-menu-empty").hidden();
        };
        let palette = self.palette();
        let node_id = menu.node_id;
        let fallback_source = menu.source.clone();

        div()
            .id("log-tree-context-menu")
            .absolute()
            .left(px(menu.x))
            .top(px(menu.y))
            .w(px(LOG_TREE_CONTEXT_MENU_WIDTH))
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.menu))
            .shadow_lg()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 菜单覆盖在目录树行之上，按下事件必须在菜单层截止，避免继续冒泡到背后的树行导致误选其它文件。
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 右键点在菜单上也只应作用于菜单本身，不能重新打开或切换背后的目录树菜单目标。
                    context.stop_propagation();
                }),
            )
            .child(self.render_log_tree_context_menu_item(
                node_id,
                fallback_source.clone(),
                LogTreeContextMenuAction::SaveAs,
                "另存为...",
                Icon::Save,
                palette,
                context,
            ))
            .child(self.render_log_tree_context_menu_item(
                node_id,
                fallback_source,
                LogTreeContextMenuAction::AnalyzeThreads,
                "线程日志分析",
                Icon::ChartNoAxesCombined,
                palette,
                context,
            ))
    }

    /// 渲染左侧目录树右键菜单的透明关闭遮罩。
    ///
    /// 业务意图：
    /// - 目录树右键菜单打开后，第一次点到菜单外部应只关闭菜单，不能同时选中、展开或打开底层节点。
    /// - 遮罩放在树内容之上、菜单之下，菜单项仍可点击，菜单外区域则统一消费鼠标事件。
    pub(super) fn render_log_tree_context_menu_dismiss_overlay(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if self.log.log_tree_context_menu.is_none() {
            return div()
                .id("log-tree-context-menu-dismiss-overlay-empty")
                .hidden();
        }

        div()
            .id("log-tree-context-menu-dismiss-overlay")
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 左键按下先在遮罩层截止，避免底层目录节点在菜单关闭的同一次操作中被误选。
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    // 右键通常不触发普通 click；这里直接关闭旧菜单并截断，避免穿透到其它节点重新开菜单。
                    view.log.log_tree_context_menu = None;
                    context.notify();
                    context.stop_propagation();
                }),
            )
            .on_click(
                context.listener(|view, _event: &ClickEvent, _window, context| {
                    view.log.log_tree_context_menu = None;
                    context.notify();
                    // click 仍需截断，防止遮罩消失后同一次点击被底层树节点处理。
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染左侧目录树右键菜单单项。
    ///
    /// 业务意图：
    /// - 菜单项点击后进入统一命令分发，避免另存为和分析各自重复收起菜单、筛选选中来源。
    /// - 渲染 helper 需要同时拿到节点、动作、图标和主题，当前保持显式参数以减少临时结构类型扩散。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_log_tree_context_menu_item(
        &self,
        node_id: usize,
        fallback_source: Option<LogFileSource>,
        action: LogTreeContextMenuAction,
        label: &'static str,
        icon: Icon,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!(
                "log-tree-menu-{}-{}",
                node_id, label
            )))
            .flex()
            .items_center()
            .gap_2()
            .h(px(LOG_TREE_CONTEXT_MENU_ITEM_HEIGHT))
            .px_3()
            .text_sm()
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |item| item.bg(rgb(palette.hover)))
            .child(Self::render_lucide_icon(
                Some(icon),
                16.0,
                15.0,
                palette.muted_text,
            ))
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, _event: &MouseDownEvent, window, context| {
                    view.handle_log_tree_context_menu_action(
                        action,
                        fallback_source.clone(),
                        window,
                        context,
                    );
                    // 菜单项命令执行后不允许事件继续落到背后的树行，否则会改变当前选中集合。
                    context.stop_propagation();
                }),
            )
    }

    /// 执行左侧目录树右键菜单命令。
    ///
    /// 业务意图：
    /// - 所有命令都基于当前多选集合中的可读取文件；目录、压缩包目录和错误节点不参与文件操作。
    pub(super) fn handle_log_tree_context_menu_action(
        &mut self,
        action: LogTreeContextMenuAction,
        fallback_source: Option<LogFileSource>,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let mut sources = self.selected_log_tree_file_sources();
        if sources.is_empty()
            && let Some(fallback_source) = fallback_source
        {
            sources.push(fallback_source);
        }
        self.log.log_tree_context_menu = None;
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.search.search_results_context_menu = None;

        match action {
            LogTreeContextMenuAction::SaveAs => {
                self.save_selected_log_tree_sources_as(sources, context);
            }
            LogTreeContextMenuAction::AnalyzeThreads => {
                self.open_thread_analysis_for_sources(sources, window, context);
            }
        }
        context.notify();
    }

    /// 返回当前目录树选择中的可读取日志来源。
    ///
    /// 业务意图：
    /// - 多选允许包含目录和错误节点，但“另存为”和“线程日志分析”只能处理真实文件。
    /// - 按加载树原始顺序返回，保证批量保存和分析结果稳定。
    pub(super) fn selected_log_tree_file_sources(&self) -> Vec<LogFileSource> {
        let LogTreeLoadState::Loaded(tree_state) = &self.log.load_state else {
            return Vec::new();
        };
        tree_state
            .tree
            .rows
            .iter()
            .filter(|row| self.log.log_tree_selected_node_ids.contains(&row.id))
            .filter_map(|row| {
                row.source.clone().or_else(|| {
                    (row.kind == LogTreeEntryKind::Archive)
                        .then(|| tree_state.single_file_source_for_archive(row.id))
                        .flatten()
                })
            })
            .collect()
    }

    /// 将当前选择的日志来源另存为到用户选择的目录。
    ///
    /// 业务意图：
    /// - 用户要求保存当前多选的所有文件；本地文件按原文件名写入目标目录，压缩包内部文件保留成员路径层级。
    /// - 选择目录通过 GPUI 系统路径选择器完成，保证 macOS 和 Windows 使用平台原生交互。
    ///
    /// 边界条件：
    /// - 没有选中文件时直接忽略，避免打开一个无法产生结果的目录选择器。
    /// - 写入失败不影响其它文件；后台结果只统计数量，后续如需详细失败列表可接入状态面板。
    pub(super) fn save_selected_log_tree_sources_as(
        &mut self,
        sources: Vec<LogFileSource>,
        context: &mut Context<Self>,
    ) {
        self.save_log_sources_as(sources, context);
    }

    /// 将指定日志来源另存为到用户选择的目录。
    ///
    /// 业务意图：
    /// - 左侧树批量另存为和日志正文当前文件另存为使用同一管线，避免普通文件、压缩包成员和超大日志保存规则分叉。
    /// - 选择目录通过 GPUI 系统路径选择器完成，保证 macOS 和 Windows 使用平台原生交互。
    ///
    /// 边界条件：
    /// - 没有来源时直接忽略，避免打开一个无法产生结果的目录选择器。
    /// - 保存放到后台执行；失败只统计数量，不阻塞日志查看，也不影响其它文件继续保存。
    pub(super) fn save_log_sources_as(
        &mut self,
        sources: Vec<LogFileSource>,
        context: &mut Context<Self>,
    ) {
        if sources.is_empty() {
            return;
        }

        context
            .spawn(async move |view, app| {
                let options = PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: Some("选择另存为目录".into()),
                };
                let receiver = match app.update(|app| app.prompt_for_paths(options)) {
                    Ok(receiver) => receiver,
                    Err(_) => return,
                };
                let target_directory = match receiver.await {
                    Ok(Ok(Some(paths))) => paths.into_iter().next(),
                    Ok(Ok(None)) | Ok(Err(_)) | Err(_) => None,
                };
                let Some(target_directory) = target_directory else {
                    return;
                };

                view.update(app, |view, context| {
                    let conflicts = Self::save_target_conflicts(&sources, &target_directory);
                    if let Some(first_conflict_path) = conflicts.first().cloned() {
                        view.log.save_overwrite_confirm_dialog = Some(SaveOverwriteConfirmDialog {
                            sources,
                            target_directory,
                            conflict_count: conflicts.len(),
                            first_conflict_path,
                        });
                        view.log.log_tree_context_menu = None;
                        view.log.log_viewer_context_menu = None;
                    } else {
                        view.spawn_save_log_sources_to_directory(
                            sources,
                            target_directory,
                            SaveConflictPolicy::OverwriteExisting,
                            context,
                        );
                    }
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 启动后台另存为任务。
    ///
    /// 业务意图：
    /// - 选择目录、同名确认和右键菜单都可能触发真正保存；集中封装后台任务可以保证统计、菜单清理和 UI 刷新一致。
    pub(super) fn spawn_save_log_sources_to_directory(
        &mut self,
        sources: Vec<LogFileSource>,
        target_directory: PathBuf,
        conflict_policy: SaveConflictPolicy,
        context: &mut Context<Self>,
    ) {
        context
            .spawn(async move |view, app| {
                let result = app
                    .background_executor()
                    .spawn(async move {
                        Self::save_log_sources_to_directory(
                            &sources,
                            &target_directory,
                            conflict_policy,
                        )
                    })
                    .await;

                view.update(app, |view, context| {
                    // 当前没有全局状态栏；这里只保留静默完成策略，避免批量保存失败影响日志查看流程。
                    // 统计结果通过局部变量消费，确保后台错误不会被误认为需要中断 UI。
                    let _ = (
                        result.saved_count,
                        result.skipped_count,
                        result.failed_count,
                    );
                    view.log.log_tree_context_menu = None;
                    view.log.log_viewer_context_menu = None;
                    view.log.save_overwrite_confirm_dialog = None;
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 把多个日志来源写入目标目录。
    ///
    /// 业务意图：
    /// - 所有来源都直接保存到目标目录下，只保留最终文件名，不保留本地父目录或压缩包内部目录层级。
    /// - 使用分页物化管线把来源转换成可复制的本地文件，避免 10GB+ 日志另存为时把完整内容读入内存。
    ///
    /// 边界条件：
    /// - 目标路径的父目录会按需创建；权限不足、同名目录冲突或源文件消失都会记为单文件失败。
    /// - 如果目标文件已存在，按用户在确认弹窗中选择的策略跳过或覆盖；覆盖会调用 `fs::copy` 直接替换文件内容。
    pub(super) fn save_log_sources_to_directory(
        sources: &[LogFileSource],
        target_directory: &Path,
        conflict_policy: SaveConflictPolicy,
    ) -> SaveSelectedLogsResult {
        let mut saved_count = 0usize;
        let mut skipped_count = 0usize;
        let mut failed_count = 0usize;
        for source in sources {
            let relative_path = Self::save_relative_path_for_source(source);
            let target_path = target_directory.join(relative_path);
            if target_path.exists() && conflict_policy == SaveConflictPolicy::SkipExisting {
                skipped_count += 1;
                continue;
            }
            let write_result = (|| -> Result<(), LogContentError> {
                if let Some(parent) = target_path.parent() {
                    fs::create_dir_all(parent).map_err(|error| {
                        LogContentError::new(format!(
                            "无法创建目录 {}：{}",
                            parent.display(),
                            error
                        ))
                    })?;
                }
                let materialized = materialize_source_for_paging(source)?;
                let should_cleanup_materialized =
                    Self::should_cleanup_saved_materialized_source(source, &materialized.temp_path);
                let copy_result =
                    fs::copy(&materialized.temp_path, &target_path).map_err(|error| {
                        LogContentError::new(format!(
                            "无法写入文件 {}：{}",
                            target_path.display(),
                            error
                        ))
                    });
                if should_cleanup_materialized {
                    cleanup_materialized_file(&materialized.temp_path);
                }
                copy_result?;
                Ok(())
            })();
            if write_result.is_ok() {
                saved_count += 1;
            } else {
                failed_count += 1;
            }
        }

        SaveSelectedLogsResult {
            saved_count,
            skipped_count,
            failed_count,
        }
    }

    /// 计算本次另存为会命中的已有目标路径。
    ///
    /// 业务意图：
    /// - 在真正写入前先发现同名文件，才能弹出“跳过/覆盖”确认，而不是后台任务静默覆盖。
    ///
    /// 边界条件：
    /// - 这里按最终目标路径是否存在判断，包括同名普通文件、目录或符号链接；同名目录后续即使选择覆盖也会按写入失败统计。
    pub(super) fn save_target_conflicts(
        sources: &[LogFileSource],
        target_directory: &Path,
    ) -> Vec<PathBuf> {
        sources
            .iter()
            .map(|source| target_directory.join(Self::save_relative_path_for_source(source)))
            .filter(|target_path| target_path.exists())
            .collect()
    }

    /// 处理另存为同名文件确认弹窗的用户选择。
    ///
    /// 业务意图：
    /// - 用户点击“跳过”或“覆盖”后，当前弹窗对应的保存任务才可以继续进入后台复制阶段。
    ///
    /// 边界条件：
    /// - 弹窗可能已被其它状态变化清空；此时点击事件直接忽略，避免重复启动保存任务。
    pub(super) fn handle_save_overwrite_choice(
        &mut self,
        conflict_policy: SaveConflictPolicy,
        context: &mut Context<Self>,
    ) {
        let Some(dialog) = self.log.save_overwrite_confirm_dialog.take() else {
            return;
        };
        self.spawn_save_log_sources_to_directory(
            dialog.sources,
            dialog.target_directory,
            conflict_policy,
            context,
        );
        context.notify();
    }

    /// 返回日志来源另存为时使用的相对路径。
    ///
    /// 业务意图：
    /// - 本地文件、压缩包成员、已物化 7z 成员和嵌套压缩包成员都只保留最终文件名，满足“直接保存到目标目录”的要求。
    ///
    /// 边界条件：
    /// - 如果不同来源最终文件名相同，会映射到同一个目标文件，并继续触发同名文件“跳过/覆盖”确认。
    /// - 来源路径可能异常为空或以分隔符结尾，此时回退为 `log.txt`，避免生成空目标路径。
    pub(super) fn save_relative_path_for_source(source: &LogFileSource) -> PathBuf {
        pub(super) fn file_name_from_member_path(member_path: &str) -> PathBuf {
            member_path
                .split('/')
                .rfind(|part| !part.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("log.txt"))
        }

        match source {
            LogFileSource::LocalFile { path } => path
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("log.txt")),
            LogFileSource::ArchiveMember { member_path, .. } => {
                file_name_from_member_path(member_path)
            }
            LogFileSource::MaterializedArchiveMember { member_path, .. } => {
                file_name_from_member_path(member_path)
            }
            LogFileSource::NestedArchiveMember {
                nested_member_path, ..
            } => file_name_from_member_path(nested_member_path),
        }
    }

    /// 判断另存为结束后是否需要删除物化文件。
    ///
    /// 业务意图：
    /// - 普通本地日志由原文件直接复制，绝不能删除用户原文件。
    /// - 压缩包成员和单文件压缩包会先写入临时目录，复制完成或失败后都应清理，避免长期占用磁盘。
    pub(super) fn should_cleanup_saved_materialized_source(
        source: &LogFileSource,
        temp_path: &Path,
    ) -> bool {
        match source {
            LogFileSource::LocalFile { path } => temp_path != path,
            LogFileSource::MaterializedArchiveMember {
                temp_path: source_temp_path,
                ..
            } => temp_path != source_temp_path,
            LogFileSource::ArchiveMember { .. } | LogFileSource::NestedArchiveMember { .. } => true,
        }
    }
}
