// 日志正文滚动条和分页滚动辅助。
//
// 业务意图：
// - 从日志正文查看器中拆出 纵横滚动条、分页视口和滚轮换算，让正文渲染、滚动、选区和菜单职责更清晰。
// - 本轮只移动方法边界并保留原有中文注释，不改变虚拟列表、分页日志、右键菜单或复制行为。

use super::*;

impl MainView {
    /// 根据总行数计算行号列宽度。
    ///
    /// 业务意图：
    /// - 小文件不需要占用上一版 68px 宽度；行数增长时再按数字位数适度增加。
    /// - 该宽度同时用于行号背景和每一行的行号单元格，保证未填满高度时背景也能对齐。
    pub(in crate::app) fn log_viewer_line_number_width(line_count: usize) -> f32 {
        let digit_count = line_count.max(1).to_string().len() as f32;
        (digit_count * LOG_VIEWER_LINE_NUMBER_DIGIT_WIDTH + LOG_VIEWER_LINE_NUMBER_PADDING_WIDTH)
            .clamp(
                LOG_VIEWER_LINE_NUMBER_MIN_WIDTH,
                LOG_VIEWER_LINE_NUMBER_MAX_WIDTH,
            )
    }

    /// 渲染日志正文的纵向可见滚动条。
    ///
    /// 业务意图：
    /// - GPUI `uniform_list` 已经支持滚轮滚动，但长日志需要稳定可见的滚动位置提示和鼠标拖动入口。
    /// - 滑块与列表共享同一个 `UniformListScrollHandle`，拖动时直接写入列表底层滚动偏移，不复制日志行数据。
    pub(in crate::app) fn render_log_vertical_scrollbar(
        &self,
        tab_id: usize,
        scroll_handle: &UniformListScrollHandle,
        line_count: usize,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = self.palette();
        let Some(metrics) = Self::log_vertical_scrollbar_metrics(scroll_handle)
            .or_else(|| Self::fallback_log_vertical_scrollbar_metrics(line_count))
        else {
            return div().id("log-vertical-scrollbar-empty").hidden();
        };

        div()
            .id(SharedString::from(format!(
                "log-vertical-scrollbar-{}",
                tab_id
            )))
            .absolute()
            .top(metrics.thumb_start)
            .right(px(LOG_VIEWER_SCROLLBAR_PADDING))
            .w(px(LOG_VIEWER_SCROLLBAR_WIDTH))
            .h(metrics.thumb_length)
            .rounded(px(LOG_VIEWER_SCROLLBAR_WIDTH / 2.0))
            .bg(rgb(palette.scrollbar))
            .cursor_pointer()
            .hover(move |thumb| thumb.bg(rgb(palette.scrollbar_hover)))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.start_log_scrollbar_drag(
                        tab_id,
                        LogScrollbarAxis::Vertical,
                        event,
                        context,
                    );
                    context.notify();
                    // 日志滚动条覆盖在正文行上，按下滑块不应同时开始正文文本选择或触发浮层关闭。
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染日志正文的横向可见滚动条。
    ///
    /// 业务意图：
    /// - 日志行可能包含长 JSON、堆栈或配置片段，正文宽度超过视口时必须提供横向滚动提示。
    /// - 横向滚动条只在实际测量到内容超宽后显示，避免普通短日志底部出现无效控件。
    pub(in crate::app) fn render_log_horizontal_scrollbar(
        &self,
        tab_id: usize,
        scroll_handle: &UniformListScrollHandle,
        line_number_width: f32,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = self.palette();
        let Some(metrics) =
            Self::log_horizontal_scrollbar_metrics(scroll_handle, line_number_width)
        else {
            return div().id("log-horizontal-scrollbar-empty").hidden();
        };

        div()
            .id(SharedString::from(format!(
                "log-horizontal-scrollbar-{}",
                tab_id
            )))
            .absolute()
            .left(metrics.thumb_start)
            .bottom(px(LOG_VIEWER_SCROLLBAR_PADDING))
            .w(metrics.thumb_length)
            .h(px(LOG_VIEWER_SCROLLBAR_WIDTH))
            .rounded(px(LOG_VIEWER_SCROLLBAR_WIDTH / 2.0))
            .bg(rgb(palette.scrollbar))
            .cursor_pointer()
            .hover(move |thumb| thumb.bg(rgb(palette.scrollbar_hover)))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.start_log_scrollbar_drag(
                        tab_id,
                        LogScrollbarAxis::Horizontal,
                        event,
                        context,
                    );
                    context.notify();
                    // 横向滚动条同样位于正文上方，拖动入口需要阻断事件继续传递。
                    context.stop_propagation();
                }),
            )
    }

    /// 按 tab 当前文档类型渲染日志纵向滚动条。
    ///
    /// 业务意图：
    /// - 分页日志的滚动位置由应用侧 `f64` 状态维护，不能再读取 `uniform_list` 的完整内容高度。
    /// - 滚动条仍复用同一套拖动入口，让普通模式和分页模式的交互保持一致。
    pub(in crate::app) fn render_log_vertical_scrollbar_for_tab(
        &self,
        tab: &OpenLogTab,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = self.palette();
        let tab_id = tab.id;
        let Some(metrics) = self
            .log_scrollbar_metrics_for_tab(tab, LogScrollbarAxis::Vertical)
            .or_else(|| match &tab.state {
                LogTabState::Ready { document } => match document.as_ref() {
                    LogTabDocument::Paged(document) => {
                        Self::fallback_log_vertical_scrollbar_metrics(document.line_count())
                    }
                    LogTabDocument::InMemory(_) => None,
                },
                LogTabState::Loading { .. } | LogTabState::Failed { .. } => None,
            })
        else {
            return div().id("log-vertical-scrollbar-empty").hidden();
        };

        div()
            .id(SharedString::from(format!(
                "log-vertical-scrollbar-{}",
                tab_id
            )))
            .absolute()
            .top(metrics.thumb_start)
            .right(px(LOG_VIEWER_SCROLLBAR_PADDING))
            .w(px(LOG_VIEWER_SCROLLBAR_WIDTH))
            .h(metrics.thumb_length)
            .rounded(px(LOG_VIEWER_SCROLLBAR_WIDTH / 2.0))
            .bg(rgb(palette.scrollbar))
            .cursor_pointer()
            .hover(move |thumb| thumb.bg(rgb(palette.scrollbar_hover)))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.start_log_scrollbar_drag(
                        tab_id,
                        LogScrollbarAxis::Vertical,
                        event,
                        context,
                    );
                    context.notify();
                    // 分页日志滚动条和普通日志一样覆盖正文区域，避免按下时穿透到日志行。
                    context.stop_propagation();
                }),
            )
    }

    /// 按 tab 当前文档类型渲染日志横向滚动条。
    ///
    /// 业务意图：
    /// - 分页日志使用最长行估算横向内容宽度，只把可见行放进布局树，避免超大行数触发布局精度问题。
    pub(in crate::app) fn render_log_horizontal_scrollbar_for_tab(
        &self,
        tab: &OpenLogTab,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = self.palette();
        let tab_id = tab.id;
        let Some(metrics) = self.log_scrollbar_metrics_for_tab(tab, LogScrollbarAxis::Horizontal)
        else {
            return div().id("log-horizontal-scrollbar-empty").hidden();
        };

        div()
            .id(SharedString::from(format!(
                "log-horizontal-scrollbar-{}",
                tab_id
            )))
            .absolute()
            .left(metrics.thumb_start)
            .bottom(px(LOG_VIEWER_SCROLLBAR_PADDING))
            .w(metrics.thumb_length)
            .h(px(LOG_VIEWER_SCROLLBAR_WIDTH))
            .rounded(px(LOG_VIEWER_SCROLLBAR_WIDTH / 2.0))
            .bg(rgb(palette.scrollbar))
            .cursor_pointer()
            .hover(move |thumb| thumb.bg(rgb(palette.scrollbar_hover)))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.start_log_scrollbar_drag(
                        tab_id,
                        LogScrollbarAxis::Horizontal,
                        event,
                        context,
                    );
                    context.notify();
                    // 分页日志横向滚动条启动拖拽后应独占本次鼠标按下事件。
                    context.stop_propagation();
                }),
            )
    }

    /// 计算日志纵向滚动条滑块位置和高度。
    ///
    /// 业务意图：
    /// - 使用 `UniformListScrollHandle` 上一次布局记录的视口高度、内容高度和滚动偏移估算滑块。
    /// - 如果内容没有超过视口，则不显示滚动条，避免空文件或短日志出现无意义控件。
    pub(in crate::app) fn log_vertical_scrollbar_metrics(
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
        let track_start = px(LOG_VIEWER_SCROLLBAR_PADDING);
        let track_length = (viewport_height - track_start * 2.0).max(px(1.0));
        let min_thumb_length = px(LOG_VIEWER_SCROLLBAR_MIN_THUMB_HEIGHT).min(track_length);
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

    /// 计算日志横向滚动条滑块位置和宽度。
    ///
    /// 业务意图：
    /// - 横向滚动基于 `uniform_list` 的非受限宽度测量结果，内容宽于视口时才显示自绘滚动条。
    /// - 轨道从正文区域开始，避开左侧行号列，视觉上更接近日志正文的实际可滚动内容。
    pub(in crate::app) fn log_horizontal_scrollbar_metrics(
        scroll_handle: &UniformListScrollHandle,
        line_number_width: f32,
    ) -> Option<LogScrollbarMetrics> {
        let state = scroll_handle.0.borrow();
        let size = state.last_item_size?;
        let viewport_width = size.item.width;
        let measured_content_width = size.contents.width;
        let measured_max_scroll = (measured_content_width - viewport_width).max(px(0.0));
        let handle_max_scroll = state.base_handle.max_offset().width;
        let max_scroll = handle_max_scroll.max(measured_max_scroll);
        if viewport_width <= px(0.0) || max_scroll <= px(0.0) {
            return None;
        }

        let content_width = viewport_width + max_scroll;
        let scroll_left = (-state.base_handle.offset().x).clamp(px(0.0), max_scroll);
        let track_start = px(line_number_width + LOG_VIEWER_SCROLLBAR_PADDING);
        let track_right_padding =
            px(LOG_VIEWER_SCROLLBAR_WIDTH + LOG_VIEWER_SCROLLBAR_PADDING * 2.0);
        let track_length = (viewport_width - track_start - track_right_padding).max(px(1.0));
        let min_thumb_length = px(LOG_VIEWER_SCROLLBAR_MIN_THUMB_HEIGHT).min(track_length);
        let thumb_length =
            (track_length * (viewport_width / content_width)).clamp(min_thumb_length, track_length);
        let movable_length = (track_length - thumb_length).max(px(0.0));
        let thumb_start = track_start + movable_length * (scroll_left / max_scroll);

        Some(LogScrollbarMetrics {
            thumb_start,
            thumb_length,
            track_start,
            track_length,
            max_scroll,
            max_scroll_px: f64::from(max_scroll),
        })
    }

    /// 计算分页日志当前视口需要渲染的行数。
    ///
    /// 业务意图：
    /// - 分页模式只把视口附近行放进布局树，行数必须足以覆盖当前窗口高度和小幅滚动缓冲。
    /// - 视口尺寸首帧为空时使用固定保守值，避免布局回填前显示空白。
    pub(in crate::app) fn paged_log_visible_row_capacity(viewport_height: Pixels) -> usize {
        if viewport_height > px(0.0) {
            (f32::from(viewport_height) / LOG_VIEWER_ROW_HEIGHT).ceil() as usize
                + PAGED_LOG_RENDER_BUFFER_ROWS
        } else {
            PAGED_LOG_FALLBACK_VISIBLE_ROWS
        }
    }

    /// 将分页日志的逻辑滚动像素换算为首个可见真实行号和行内偏移。
    ///
    /// 业务意图：
    /// - 真实滚动位置使用 `f64` 保存，但渲染行只需要落在当前视口内的小像素坐标。
    /// - 返回的 `fractional_top` 始终小于单行高度，用于让首行在平滑滚动时只偏移很小的距离。
    pub(in crate::app) fn paged_log_visible_start(
        scroll_top_px: f64,
        line_count: usize,
    ) -> (usize, f32) {
        if line_count == 0 {
            return (0, 0.0);
        }

        let row_height = f64::from(px(LOG_VIEWER_ROW_HEIGHT));
        let max_first_line = line_count.saturating_sub(1) as f64;
        let safe_scroll_top = scroll_top_px.max(0.0);
        let first_line = (safe_scroll_top / row_height)
            .floor()
            .clamp(0.0, max_first_line) as usize;
        let fractional_top =
            (safe_scroll_top - first_line as f64 * row_height).clamp(0.0, row_height) as f32;

        (first_line, fractional_top)
    }

    /// 返回分页日志纵向最大逻辑滚动距离。
    ///
    /// 业务意图：
    /// - 内容总高度可能远超 `f32` 精度稳定区，因此这里全程用 `f64` 计算。
    /// - 视口高度尚未测量时使用一行高度兜底，避免除以零或产生负数。
    pub(in crate::app) fn paged_log_vertical_max_scroll_px(
        line_count: usize,
        viewport_height: Pixels,
    ) -> f64 {
        let row_height = f64::from(px(LOG_VIEWER_ROW_HEIGHT));
        let content_height = line_count as f64 * row_height;
        let viewport_height = f64::from(viewport_height).max(row_height);
        (content_height - viewport_height).max(0.0)
    }

    /// 计算分页日志跳转到目标行时应使用的纵向滚动位置。
    ///
    /// 业务意图：
    /// - 搜索结果和线程分析跳转都希望目标行出现在视口中间，而不是贴在顶部或底部。
    /// - 行号越界时按最后一行处理，避免旧搜索结果遇到外部文件变化时产生非法滚动位置。
    pub(in crate::app) fn paged_log_scroll_top_for_line(
        line_index: usize,
        line_count: usize,
        viewport_height: Pixels,
    ) -> f64 {
        if line_count == 0 {
            return 0.0;
        }

        let row_height = f64::from(px(LOG_VIEWER_ROW_HEIGHT));
        let viewport_height = f64::from(viewport_height).max(row_height);
        let safe_line_index = line_index.min(line_count.saturating_sub(1));
        let line_center = safe_line_index as f64 * row_height + row_height / 2.0;
        let target_top = line_center - viewport_height / 2.0;
        target_top.clamp(
            0.0,
            Self::paged_log_vertical_max_scroll_px(line_count, px(viewport_height as f32)),
        )
    }

    /// 估算分页日志横向内容宽度。
    ///
    /// 业务意图：
    /// - 分页模式不能再让 GPUI 测量完整最长行，因为那会重新创建超大虚拟列表。
    /// - 这里读取行索引记录的最长候选行并按等宽字体估算宽度，足够驱动横向滚动条范围；鼠标选区仍使用真实 shaping。
    pub(in crate::app) fn paged_log_estimated_content_width(
        document: &log_document::PagedLogDocument,
        line_number_width: f32,
        font_size: f32,
    ) -> f64 {
        let fallback_columns = document
            .line_index
            .get(document.longest_line_index)
            .map(|entry| entry.byte_len as usize)
            .unwrap_or(0);
        let display_columns = document
            .read_line(document.longest_line_index)
            .ok()
            .flatten()
            .map(|line| {
                Self::expanded_log_line_for_display(&line.text)
                    .text
                    .chars()
                    .count()
            })
            .unwrap_or(fallback_columns);
        let char_width = (font_size * PAGED_LOG_MONOSPACE_WIDTH_RATIO).max(1.0);
        f64::from(
            line_number_width
                + LOG_VIEWER_TEXT_LEFT_PADDING
                + display_columns as f32 * char_width
                + LOG_VIEWER_SCROLLBAR_WIDTH * 3.0,
        )
    }

    /// 返回分页日志横向最大逻辑滚动距离。
    pub(in crate::app) fn paged_log_horizontal_max_scroll_px(
        document: &log_document::PagedLogDocument,
        viewport_width: Pixels,
        line_number_width: f32,
        font_size: f32,
    ) -> f64 {
        let content_width =
            Self::paged_log_estimated_content_width(document, line_number_width, font_size);
        (content_width - f64::from(viewport_width).max(1.0)).max(0.0)
    }

    /// 计算分页日志纵向滚动条滑块。
    pub(in crate::app) fn paged_log_vertical_scrollbar_metrics(
        tab: &OpenLogTab,
        document: &log_document::PagedLogDocument,
    ) -> Option<LogScrollbarMetrics> {
        let viewport_height = tab.paged_viewport_handle.bounds().size.height;
        if viewport_height <= px(0.0) {
            return None;
        }

        let line_count = document.line_count();
        let max_scroll_px = Self::paged_log_vertical_max_scroll_px(line_count, viewport_height);
        if max_scroll_px <= 0.0 {
            return None;
        }

        let content_height = line_count as f64 * f64::from(px(LOG_VIEWER_ROW_HEIGHT));
        let track_start = px(LOG_VIEWER_SCROLLBAR_PADDING);
        let track_length = (viewport_height - track_start * 2.0).max(px(1.0));
        let min_thumb_length = px(LOG_VIEWER_SCROLLBAR_MIN_THUMB_HEIGHT).min(track_length);
        let thumb_length = (viewport_height * (f64::from(viewport_height) / content_height) as f32)
            .clamp(min_thumb_length, track_length);
        let movable_length = (track_length - thumb_length).max(px(0.0));
        let scroll_ratio = (tab.paged_scroll.top_px / max_scroll_px).clamp(0.0, 1.0) as f32;
        let thumb_start = track_start + movable_length * scroll_ratio;

        Some(LogScrollbarMetrics {
            thumb_start,
            thumb_length,
            track_start,
            track_length,
            max_scroll: px(max_scroll_px.min(f64::from(f32::MAX)) as f32),
            max_scroll_px,
        })
    }

    /// 计算分页日志横向滚动条滑块。
    pub(in crate::app) fn paged_log_horizontal_scrollbar_metrics(
        tab: &OpenLogTab,
        document: &log_document::PagedLogDocument,
        font_size: f32,
    ) -> Option<LogScrollbarMetrics> {
        let viewport_width = tab.paged_viewport_handle.bounds().size.width;
        if viewport_width <= px(0.0) {
            return None;
        }

        let line_number_width = Self::log_viewer_line_number_width(document.line_count());
        let max_scroll_px = Self::paged_log_horizontal_max_scroll_px(
            document,
            viewport_width,
            line_number_width,
            font_size,
        );
        if max_scroll_px <= 0.0 {
            return None;
        }

        let content_width = f64::from(viewport_width) + max_scroll_px;
        let track_start = px(line_number_width + LOG_VIEWER_SCROLLBAR_PADDING);
        let track_right_padding =
            px(LOG_VIEWER_SCROLLBAR_WIDTH + LOG_VIEWER_SCROLLBAR_PADDING * 2.0);
        let track_length = (viewport_width - track_start - track_right_padding).max(px(1.0));
        let min_thumb_length = px(LOG_VIEWER_SCROLLBAR_MIN_THUMB_HEIGHT).min(track_length);
        let thumb_length = (track_length * (f64::from(viewport_width) / content_width) as f32)
            .clamp(min_thumb_length, track_length);
        let movable_length = (track_length - thumb_length).max(px(0.0));
        let scroll_ratio = (tab.paged_scroll.left_px / max_scroll_px).clamp(0.0, 1.0) as f32;
        let thumb_start = track_start + movable_length * scroll_ratio;

        Some(LogScrollbarMetrics {
            thumb_start,
            thumb_length,
            track_start,
            track_length,
            max_scroll: px(max_scroll_px.min(f64::from(f32::MAX)) as f32),
            max_scroll_px,
        })
    }

    /// 在列表首帧尚未写入布局测量时，给明显较长的日志提供一个临时纵向滚动条提示。
    ///
    /// 业务意图：
    /// - `UniformListScrollHandle` 的内容高度需要等布局完成后才有值，首帧如果完全不显示滚动条会让长日志看起来不可滚动。
    /// - 这里仅对超过常见单屏行数的日志显示顶部最小滑块，下一次滚动或重绘会被真实测量值替换。
    pub(in crate::app) fn fallback_log_vertical_scrollbar_metrics(
        line_count: usize,
    ) -> Option<LogScrollbarMetrics> {
        if line_count <= 40 {
            return None;
        }

        Some(LogScrollbarMetrics {
            thumb_start: px(LOG_VIEWER_SCROLLBAR_PADDING),
            thumb_length: px(LOG_VIEWER_SCROLLBAR_MIN_THUMB_HEIGHT),
            track_start: px(LOG_VIEWER_SCROLLBAR_PADDING),
            track_length: px(LOG_VIEWER_SCROLLBAR_MIN_THUMB_HEIGHT),
            max_scroll: px(0.0),
            max_scroll_px: 0.0,
        })
    }

    /// 处理分页日志正文滚轮滚动。
    ///
    /// 业务意图：
    /// - 分页模式的纵向总高度不能交给 GPUI 滚动容器，因此滚轮事件直接更新应用侧 `PagedLogScrollState`。
    /// - Windows 的 Shift+滚轮和横向滚轮会产生 `delta.x`，这里同步支持横向滚动，保持和普通日志查看器一致。
    ///
    /// 边界条件：
    /// - 视口尚未完成测量时仍允许更新纵向位置；横向范围依赖视口宽度，未测量时会自然保持 0。
    pub(in crate::app) fn handle_paged_log_scroll_wheel(
        &mut self,
        tab_id: usize,
        event: &ScrollWheelEvent,
        context: &mut Context<Self>,
    ) {
        let font_size = self.settings.log_viewer_font_size;
        let pixel_delta = event.delta.pixel_delta(px(20.0));
        {
            let Some(tab) = self.log.open_tabs.iter_mut().find(|tab| tab.id == tab_id) else {
                return;
            };
            let LogTabState::Ready { document } = &tab.state else {
                return;
            };
            let LogTabDocument::Paged(document) = document.as_ref() else {
                return;
            };

            let viewport_bounds = tab.paged_viewport_handle.bounds();
            let line_number_width = Self::log_viewer_line_number_width(document.line_count());
            let max_vertical_scroll = Self::paged_log_vertical_max_scroll_px(
                document.line_count(),
                viewport_bounds.size.height,
            );
            let max_horizontal_scroll = Self::paged_log_horizontal_max_scroll_px(
                document,
                viewport_bounds.size.width,
                line_number_width,
                font_size,
            );

            tab.paged_scroll.top_px = (tab.paged_scroll.top_px - f64::from(pixel_delta.y))
                .clamp(0.0, max_vertical_scroll);
            tab.paged_scroll.left_px = (tab.paged_scroll.left_px - f64::from(pixel_delta.x))
                .clamp(0.0, max_horizontal_scroll);
        }
        self.log.log_viewer_context_menu = None;
        context.notify();
    }

    /// 开始拖动日志正文滚动条滑块。
    ///
    /// 业务意图：
    /// - 鼠标按下滑块时记录当前 tab、方向和按下点在滑块内的偏移，后续移动事件按这些信息换算滚动偏移。
    /// - 拖动滚动条属于正文操作，开始拖动时同步关闭 tab 右键菜单和编码下拉框，避免弹层遮挡拖动区域。
    ///
    /// 边界条件：
    /// - 如果当前 tab 已关闭、滚动条测量尚未完成或日志内容不足以滚动，则忽略本次按下。
    pub(in crate::app) fn start_log_scrollbar_drag(
        &mut self,
        tab_id: usize,
        axis: LogScrollbarAxis,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        self.note_keyboard_scroll_region(KeyboardScrollRegion::LogContent);
        let Some(tab) = self.log.open_tabs.iter().find(|tab| tab.id == tab_id) else {
            return;
        };
        let Some(metrics) = self.log_scrollbar_metrics_for_tab(tab, axis) else {
            return;
        };
        if metrics.max_scroll <= px(0.0) {
            return;
        }
        let Some(viewport_origin) = Self::log_scrollbar_viewport_axis_origin_for_tab(tab, axis)
        else {
            return;
        };

        let pointer_position = match axis {
            LogScrollbarAxis::Vertical => event.position.y,
            LogScrollbarAxis::Horizontal => event.position.x,
        };
        let absolute_thumb_start = viewport_origin + metrics.thumb_start;
        self.log.log_scrollbar_drag = Some(LogScrollbarDrag {
            tab_id,
            axis,
            cursor_offset: pointer_position - absolute_thumb_start,
        });
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
        self.stop_log_text_selection(context);
    }

    /// 根据鼠标移动更新日志正文滚动条拖动结果。
    ///
    /// 业务意图：
    /// - 自绘滚动条拖动必须反向写入 `uniform_list` 底层滚动偏移，才能让虚拟列表、滚轮滚动和滑块位置保持同源。
    /// - 只在鼠标左键仍处于按下状态时更新；如果系统报告左键已释放，则立即清理拖动状态。
    pub(in crate::app) fn update_log_scrollbar_drag(
        &mut self,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        let Some(drag) = self.log.log_scrollbar_drag else {
            return;
        };
        if !event.dragging() {
            self.log.log_scrollbar_drag = None;
            context.notify();
            return;
        }

        let (metrics, viewport_origin, is_paged) = {
            let Some(tab) = self.log.open_tabs.iter().find(|tab| tab.id == drag.tab_id) else {
                self.log.log_scrollbar_drag = None;
                context.notify();
                return;
            };
            let Some(metrics) = self.log_scrollbar_metrics_for_tab(tab, drag.axis) else {
                self.log.log_scrollbar_drag = None;
                context.notify();
                return;
            };
            let Some(viewport_origin) =
                Self::log_scrollbar_viewport_axis_origin_for_tab(tab, drag.axis)
            else {
                self.log.log_scrollbar_drag = None;
                context.notify();
                return;
            };
            (
                metrics,
                viewport_origin,
                matches!(
                    &tab.state,
                    LogTabState::Ready { document } if matches!(document.as_ref(), LogTabDocument::Paged(_))
                ),
            )
        };

        let pointer_position = match drag.axis {
            LogScrollbarAxis::Vertical => event.position.y,
            LogScrollbarAxis::Horizontal => event.position.x,
        };
        let movable_length = (metrics.track_length - metrics.thumb_length).max(px(0.0));
        if metrics.max_scroll <= px(0.0) || movable_length <= px(0.0) {
            return;
        }

        let requested_thumb_start = pointer_position - viewport_origin - drag.cursor_offset;
        let thumb_start =
            requested_thumb_start.clamp(metrics.track_start, metrics.track_start + movable_length);
        let scroll_ratio = f64::from((thumb_start - metrics.track_start) / movable_length);
        let scroll_offset_px = metrics.max_scroll_px * scroll_ratio;

        if is_paged {
            if let Some(tab) = self
                .log
                .open_tabs
                .iter_mut()
                .find(|tab| tab.id == drag.tab_id)
            {
                match drag.axis {
                    LogScrollbarAxis::Vertical => {
                        tab.paged_scroll.top_px =
                            scroll_offset_px.clamp(0.0, metrics.max_scroll_px);
                    }
                    LogScrollbarAxis::Horizontal => {
                        tab.paged_scroll.left_px =
                            scroll_offset_px.clamp(0.0, metrics.max_scroll_px);
                    }
                }
            }
        } else if let Some(tab) = self.log.open_tabs.iter().find(|tab| tab.id == drag.tab_id) {
            let scroll_offset = px(scroll_offset_px as f32);
            let base_scroll_handle = {
                // `UniformListScrollHandle` 包装了真正的 `ScrollHandle`；这里克隆句柄后释放借用，再写入偏移。
                // 这样可以避免在 RefCell 借用仍存活时触发内部可变借用，保持滚动同步逻辑清晰。
                tab.scroll_handle.0.borrow().base_handle.clone()
            };
            let current_offset = base_scroll_handle.offset();

            match drag.axis {
                LogScrollbarAxis::Vertical => {
                    base_scroll_handle.set_offset(point(current_offset.x, -scroll_offset));
                }
                LogScrollbarAxis::Horizontal => {
                    base_scroll_handle.set_offset(point(-scroll_offset, current_offset.y));
                }
            }
        }
        context.notify();
    }

    /// 结束日志正文滚动条拖动。
    ///
    /// 业务意图：
    /// - 鼠标释放后清空拖动状态，避免下一次普通鼠标移动继续改变日志滚动位置。
    pub(in crate::app) fn stop_log_scrollbar_drag(&mut self, context: &mut Context<Self>) {
        if self.log.log_scrollbar_drag.is_some() {
            self.log.log_scrollbar_drag = None;
            context.notify();
        }
    }
}
