// 日志正文单行渲染与标记辅助。
//
// 业务意图：
// - 从日志正文查看器中拆出 单行绘制、滚动条指标读取和标记行切换，让正文渲染、滚动、选区和菜单职责更清晰。
// - 本轮只移动方法边界并保留原有中文注释，不改变虚拟列表、分页日志、右键菜单或复制行为。

use super::*;

impl MainView {
    /// 按 tab 和方向取得当前滚动条的真实测量数据。
    ///
    /// 业务意图：
    /// - 渲染、按下和拖动都复用同一套测量函数；横向滚动条需要根据当前日志行数计算行号列宽。
    /// - 只有处于已解码状态的 tab 才可能产生横向滚动条，因为加载和失败状态没有正文列表。
    pub(in crate::app) fn log_scrollbar_metrics_for_tab(
        &self,
        tab: &OpenLogTab,
        axis: LogScrollbarAxis,
    ) -> Option<LogScrollbarMetrics> {
        match &tab.state {
            LogTabState::Ready { document } => match document.as_ref() {
                LogTabDocument::Paged(document) => match axis {
                    LogScrollbarAxis::Vertical => {
                        Self::paged_log_vertical_scrollbar_metrics(tab, document)
                    }
                    LogScrollbarAxis::Horizontal => Self::paged_log_horizontal_scrollbar_metrics(
                        tab,
                        document,
                        self.settings.log_viewer_font_size,
                    ),
                },
                LogTabDocument::InMemory(document) => match axis {
                    LogScrollbarAxis::Vertical => {
                        Self::log_vertical_scrollbar_metrics(&tab.scroll_handle)
                    }
                    LogScrollbarAxis::Horizontal => Self::log_horizontal_scrollbar_metrics(
                        &tab.scroll_handle,
                        Self::log_viewer_line_number_width(document.line_count()),
                    ),
                },
            },
            LogTabState::Loading { .. } | LogTabState::Failed { .. } => None,
        }
    }

    /// 取得日志正文滚动条视口在当前轴向上的窗口坐标起点。
    ///
    /// 业务意图：
    /// - 普通日志使用 `uniform_list` 的底层 `ScrollHandle` bounds。
    /// - 分页日志使用独立视口测量句柄，避免为了坐标换算重新依赖完整虚拟列表。
    pub(in crate::app) fn log_scrollbar_viewport_axis_origin_for_tab(
        tab: &OpenLogTab,
        axis: LogScrollbarAxis,
    ) -> Option<Pixels> {
        let bounds = match &tab.state {
            LogTabState::Ready { document } => match document.as_ref() {
                LogTabDocument::Paged(_) => tab.paged_viewport_handle.bounds(),
                LogTabDocument::InMemory(_) => tab.scroll_handle.0.borrow().base_handle.bounds(),
            },
            LogTabState::Loading { .. } | LogTabState::Failed { .. } => {
                tab.scroll_handle.0.borrow().base_handle.bounds()
            }
        };

        match axis {
            LogScrollbarAxis::Vertical if bounds.size.height > px(0.0) => Some(bounds.top()),
            LogScrollbarAxis::Horizontal if bounds.size.width > px(0.0) => Some(bounds.left()),
            LogScrollbarAxis::Vertical | LogScrollbarAxis::Horizontal => None,
        }
    }

    /// 取得虚拟列表视口在当前轴向上的窗口坐标起点。
    ///
    /// 业务意图：
    /// - 鼠标事件给出的是窗口坐标，而滚动条滑块位置是列表内部局部坐标，拖动换算前必须统一坐标系。
    /// - 左侧目录树和右侧日志正文都使用 `UniformListScrollHandle`，统一函数可以避免两个滚动条坐标换算出现偏差。
    pub(in crate::app) fn uniform_list_viewport_axis_origin(
        scroll_handle: &UniformListScrollHandle,
        axis: LogScrollbarAxis,
    ) -> Option<Pixels> {
        let state = scroll_handle.0.borrow();
        let bounds = state.base_handle.bounds();
        match axis {
            LogScrollbarAxis::Vertical if bounds.size.height > px(0.0) => Some(bounds.top()),
            LogScrollbarAxis::Horizontal if bounds.size.width > px(0.0) => Some(bounds.left()),
            LogScrollbarAxis::Vertical | LogScrollbarAxis::Horizontal => None,
        }
    }

    /// 渲染日志正文中的单行。
    ///
    /// 业务意图：
    /// - 行号固定宽度，正文使用等宽字体并保持不换行，符合日志查看器常见阅读习惯。
    /// - 普通列表横向滚动时整行会被 GPUI 平移，分页窗口化渲染时只平移正文；两个偏移由调用方分别传入。
    /// - 日志级别高亮只作用于等级关键字，不改变整行背景，避免大面积颜色干扰扫描。
    /// - 搜索结果跳转同时使用整行背景和正文片段高亮：整行背景负责定位当前行，片段高亮负责定位关键字列。
    pub(in crate::app) fn render_log_line(
        input: LogLineRenderData,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let LogLineRenderData {
            tab_id,
            line_index,
            line,
            display_line,
            highlights,
            line_number_width,
            font_size,
            horizontal_line_number_offset,
            horizontal_content_offset,
            search_highlighted,
            marked,
            suppress_hover,
            palette,
        } = input;
        let line_for_mouse_down = line.clone();
        let line_for_mouse_move = line.clone();

        div()
            .id(SharedString::from(format!(
                "log-line-{}-{}",
                tab_id, line_index
            )))
            .relative()
            .h(px(LOG_VIEWER_ROW_HEIGHT))
            .text_size(px(font_size))
            .line_height(px(LOG_VIEWER_ROW_HEIGHT))
            .font_family(LOG_VIEWER_FONT_FAMILY)
            .when(search_highlighted, |row| {
                row.bg(rgb(palette.search_highlight))
            })
            .when(!search_highlighted && !suppress_hover, |row| {
                row.hover(move |row| row.bg(rgb(palette.hover)))
            })
            .child(
                div()
                    .relative()
                    .left(horizontal_content_offset)
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(LOG_VIEWER_ROW_HEIGHT))
                    .pl(px(line_number_width + LOG_VIEWER_TEXT_LEFT_PADDING))
                    .pr_2()
                    .whitespace_nowrap()
                    .text_color(rgb(palette.text))
                    .child(StyledText::new(display_line).with_highlights(highlights)),
            )
            .child(
                div()
                    .absolute()
                    .left(horizontal_line_number_offset)
                    .top(px(0.0))
                    .flex()
                    .items_center()
                    .justify_end()
                    .h(px(LOG_VIEWER_ROW_HEIGHT))
                    .w(px(line_number_width))
                    .pr_2()
                    .text_right()
                    .text_color(rgb(if marked {
                        palette.accent
                    } else {
                        palette.muted_text
                    }))
                    .bg(rgb(if search_highlighted {
                        palette.search_highlight
                    } else {
                        palette.panel
                    }))
                    .border_r_1()
                    .border_color(rgb(palette.border))
                    .cursor_pointer()
                    .when(marked, |gutter| {
                        gutter.child(
                            div()
                                .absolute()
                                .left(px(7.0))
                                .top(px((LOG_VIEWER_ROW_HEIGHT - 6.0) / 2.0))
                                .w(px(6.0))
                                .h(px(6.0))
                                .rounded(px(999.0))
                                .bg(rgb(palette.accent)),
                        )
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                            view.toggle_log_line_marker(tab_id, line_index, context);
                            context.stop_propagation();
                        }),
                    )
                    .child((line_index + 1).to_string()),
            )
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, event: &MouseDownEvent, window, context| {
                    view.start_log_text_selection(
                        tab_id,
                        line_index,
                        &line_for_mouse_down,
                        event,
                        window,
                        context,
                    );
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.open_log_viewer_context_menu(
                        tab_id,
                        f32::from(event.position.x),
                        f32::from(event.position.y),
                        context,
                    );
                    context.stop_propagation();
                }),
            )
            .on_mouse_move(context.listener(
                move |view, event: &MouseMoveEvent, window, context| {
                    view.update_log_text_selection(
                        tab_id,
                        line_index,
                        &line_for_mouse_move,
                        event,
                        window,
                        context,
                    );
                },
            ))
    }

    /// 切换日志正文指定行的手动标记状态。
    ///
    /// 业务意图：
    /// - 行号 gutter 是标记入口，点击只影响当前 tab 的标记集合，不改变正文选区或搜索状态。
    /// - 取消刚刚作为 `F2` 起点的标记时清空跳转游标，下一次 `F2` 会重新从当前可视顶部寻找，避免指向已不存在的标记。
    ///
    /// 边界条件：
    /// - 后台加载完成前行号不会渲染；如果异步事件带着旧 tab ID 回来且 tab 已关闭，直接忽略。
    pub(in crate::app) fn toggle_log_line_marker(
        &mut self,
        tab_id: usize,
        line_index: usize,
        context: &mut Context<Self>,
    ) {
        let Some(tab) = self.log.open_tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };
        if !matches!(tab.state, LogTabState::Ready { .. }) {
            return;
        }

        let marked = Self::toggle_marked_line(&mut tab.marked_lines, line_index);
        if !marked && tab.last_marker_jump_line == Some(line_index) {
            tab.last_marker_jump_line = None;
        }
        self.log.log_viewer_context_menu = None;
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.search.search_results_context_menu = None;
        context.notify();
    }
}
