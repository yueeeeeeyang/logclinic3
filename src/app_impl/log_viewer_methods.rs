// 日志 tab、编码选择器、日志正文查看器和正文右键菜单方法。
//
// 业务意图：
// - 这些方法仍属于 `MainView` 的实现块，但物理上从 `app.rs` 拆出，降低根文件体积。
// - 分页日志在本文件内使用窗口化渲染，避免超大行号对应的绝对像素坐标触发 `f32` 精度问题。
//
// 边界条件：
// - 该文件作为 `app` 的子模块自行声明 `impl MainView`，跨模块调用通过 `pub(super)` 方法显式暴露。

use super::*;

impl MainView {
    pub(super) fn render_log_tab_body(
        &self,
        tab: &OpenLogTab,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let palette = self.palette();
        match &tab.state {
            LogTabState::Loading { message } => self.render_log_tab_loading_message(message),
            LogTabState::Failed { message } => self.render_log_tab_state_message(
                Icon::FileX,
                palette.error,
                "日志打开失败",
                message,
            ),
            LogTabState::Ready { document } => {
                self.render_log_document_viewer(tab, document, context)
            }
        }
    }

    /// 渲染日志打开中的动态提示。
    ///
    /// 业务意图：
    /// - 大日志读取和解码已经在后台执行，但静态图标会让用户误以为窗口卡死；这里使用 GPUI 循环动画持续旋转加载图标。
    /// - 该动画只在 `Loading` 状态渲染，加载完成或失败后会随状态切换自动移除，不需要额外保存动画状态。
    ///
    /// 边界条件：
    /// - 动画只影响图标层，不触碰后台读取任务；即使文件很大，UI 线程仍只负责轻量重绘。
    /// - 使用稳定 ID 让同一个 tab 的加载状态重绘时复用动画进度，避免文案更新导致旋转重新从 0 开始。
    pub(super) fn render_log_tab_loading_message(
        &self,
        message: &str,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = self.palette();
        div()
            .id("log-tab-loading-message")
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .flex_1()
            .size_full()
            .px_4()
            .bg(rgb(palette.background))
            .child(Self::render_loading_spinner(palette.accent))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_text))
                    .text_center()
                    .child(message.to_string()),
            )
    }

    /// 渲染循环脉冲的加载图标。
    ///
    /// 业务意图：
    /// - GPUI 0.2.2 只有 SVG 元素支持旋转变换；这里使用三个点的透明度脉冲，避免为加载态额外引入 SVG 资源。
    /// - 三个点错峰变化，用户在打开大文件时能持续看到“仍在处理”的动态反馈。
    pub(super) fn render_loading_spinner(icon_color: u32) -> impl IntoElement {
        div()
            .id("log-tab-loading-dots")
            .flex()
            .items_center()
            .gap_1()
            .h(px(28.0))
            .children((0..3).map(move |index| Self::render_loading_dot(index, icon_color)))
    }

    /// 渲染单个加载脉冲点。
    ///
    /// 业务意图：
    /// - 每个点使用相同动画周期但不同相位，形成从左到右流动的加载感。
    /// - 点元素尺寸固定，动画期间只改变透明度，避免布局抖动。
    pub(super) fn render_loading_dot(index: usize, icon_color: u32) -> impl IntoElement {
        div()
            .id(SharedString::from(format!("log-tab-loading-dot-{index}")))
            .w(px(8.0))
            .h(px(8.0))
            .rounded(px(4.0))
            .bg(rgb(icon_color))
            .with_animation(
                SharedString::from(format!("log-tab-loading-dot-animation-{index}")),
                Animation::new(Duration::from_millis(900)).repeat(),
                move |dot, delta| dot.opacity(Self::loading_dot_opacity(delta, index as f32 / 3.0)),
            )
    }

    /// 计算加载脉冲点在某一动画进度下的透明度。
    ///
    /// 业务意图：
    /// - 把动画数学逻辑拆成纯函数，避免渲染闭包里出现难以验证的魔法数字。
    /// - 返回值保持在可见范围内，即使窗口长时间停留在加载态也不会出现完全不可见的点。
    pub(super) fn loading_dot_opacity(delta: f32, phase_offset: f32) -> f32 {
        let phase = (delta + phase_offset).fract();
        if phase < 0.5 {
            0.35 + phase * 1.3
        } else {
            1.0 - (phase - 0.5) * 1.3
        }
    }

    /// 渲染 tab 状态提示。
    ///
    /// 业务意图：
    /// - 读取失败或自动编码失败时不能只显示空白，必须让用户知道下一步可以切换编码或重新加载。
    pub(super) fn render_log_tab_state_message(
        &self,
        icon: Icon,
        icon_color: u32,
        title: &str,
        message: &str,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = self.palette();
        div()
            .id("log-tab-state-message")
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .flex_1()
            .size_full()
            .px_4()
            .bg(rgb(palette.background))
            .child(Self::render_lucide_icon(Some(icon), 32.0, 28.0, icon_color))
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child(title.to_string()),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_text))
                    .text_center()
                    .child(message.to_string()),
            )
    }

    /// 渲染只读日志正文查看器。
    ///
    /// 业务意图：
    /// - 使用 GPUI `uniform_list` 只渲染可见日志行，避免大文件滚动时为全部行创建元素。
    /// - 行号和正文分栏显示，正文按日志级别做轻量高亮。
    pub(super) fn render_log_document_viewer(
        &self,
        tab: &OpenLogTab,
        document: &LogTabDocument,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if let LogTabDocument::Paged(document) = document {
            return self.render_paged_log_document_viewer(tab, document, context);
        }

        self.render_uniform_log_document_viewer(tab, document, context)
    }

    /// 渲染普通内存日志正文查看器。
    ///
    /// 业务意图：
    /// - 小文件行数有限，继续使用 GPUI `uniform_list` 可以复用成熟的虚拟列表、滚轮和横向测量能力。
    /// - 超大分页日志改走窗口化渲染，避免完整行数乘以固定行高后产生过大的 `f32` 坐标。
    fn render_uniform_log_document_viewer(
        &self,
        tab: &OpenLogTab,
        document: &LogTabDocument,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let tab_id = tab.id;
        let line_count = document.line_count();
        let scroll_handle = tab.scroll_handle.clone();
        let line_number_width = Self::log_viewer_line_number_width(line_count);
        let horizontal_measure_line_index = document.longest_line_index();
        let row_scroll_handle = scroll_handle.clone();
        let palette = self.palette();
        let syntax_theme = self.effective_theme().syntax_theme();

        let viewer = div()
            .id(SharedString::from(format!("log-viewer-{}", tab_id)))
            .flex()
            .flex_col()
            .flex_1()
            .overflow_hidden()
            .bg(rgb(palette.background));

        let viewer = if let Some(warning) = document.warning() {
            viewer.child(
                div()
                    .flex_none()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(if self.effective_theme() == EffectiveTheme::Dark {
                        0xffd33d
                    } else {
                        0x9a6700
                    }))
                    .bg(rgb(palette.search_highlight))
                    .border_b_1()
                    .border_color(rgb(palette.border))
                    .child(warning.to_string()),
            )
        } else {
            viewer
        };

        viewer.child(
            div()
                .id("log-viewer-body")
                .relative()
                .flex()
                .flex_1()
                .overflow_hidden()
                .bg(rgb(palette.background))
                .on_mouse_move(context.listener(
                    |view, _event: &MouseMoveEvent, _window, _context| {
                        view.note_keyboard_scroll_region(KeyboardScrollRegion::LogContent);
                    },
                ))
                .on_mouse_down(
                    MouseButton::Left,
                    context.listener(|view, _event: &MouseDownEvent, _window, _context| {
                        view.note_keyboard_scroll_region(KeyboardScrollRegion::LogContent);
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    context.listener(move |view, event: &MouseDownEvent, _window, context| {
                        view.note_keyboard_scroll_region(KeyboardScrollRegion::LogContent);
                        view.open_log_viewer_context_menu(
                            tab_id,
                            f32::from(event.position.x),
                            f32::from(event.position.y),
                            context,
                        );
                        context.stop_propagation();
                    }),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(0.0))
                        .top(px(0.0))
                        .h_full()
                        .w(px(line_number_width))
                        .bg(rgb(palette.panel))
                        .border_r_1()
                        .border_color(rgb(palette.border)),
                )
                .child(
                    uniform_list(
                        "log-document-virtual-list",
                        line_count,
                        context.processor(
                            move |view, range: std::ops::Range<usize>, _window, context| {
                                let lines = view
                                    .open_tabs
                                    .iter()
                                    .find(|tab| tab.id == tab_id)
                                    .and_then(|tab| match &tab.state {
                                        LogTabState::Ready { document } => Some((
                                            document,
                                            tab.highlighted_search_line,
                                            tab.text_selection.clone(),
                                        )),
                                        LogTabState::Loading { .. }
                                        | LogTabState::Failed { .. } => None,
                                    })
                                    .map(|(document, highlighted_search_line, text_selection)| {
                                        range
                                            .filter_map(|index| {
                                                let line = match document {
                                                    LogTabDocument::InMemory(document) => document
                                                        .lines
                                                        .get(index)
                                                        .cloned(),
                                                    LogTabDocument::Paged(document) => document
                                                        .read_line(index)
                                                        .ok()
                                                        .flatten()
                                                        .map(|line| {
                                                            let _ = (
                                                                line.line_number,
                                                                line.byte_offset,
                                                                line.had_replacements,
                                                            );
                                                            line.text
                                                        }),
                                                }?;
                                                let precomputed = match document {
                                                    LogTabDocument::InMemory(document) => document
                                                        .precomputed_highlights
                                                        .as_ref()
                                                        .filter(|_| {
                                                            syntax_theme == SyntaxTheme::Light
                                                        })
                                                        .and_then(|highlights| {
                                                            highlights.lines.get(index)
                                                        }),
                                                    LogTabDocument::Paged(_) => None,
                                                };
                                                let highlight_mode = match document {
                                                    LogTabDocument::InMemory(document) => {
                                                        document.highlight_mode
                                                    }
                                                    LogTabDocument::Paged(document) => {
                                                        document.highlight_mode
                                                    }
                                                };
                                                    let mut line_highlights = highlight_line(
                                                        highlight_mode,
                                                        &line,
                                                        precomputed,
                                                        syntax_theme,
                                                    );
                                                    if let Some(selection) = &text_selection
                                                        && let Some(range) =
                                                            Self::selected_byte_range_for_line(
                                                                selection, index, &line,
                                                            )
                                                    {
                                                        // `StyledText::with_highlights` 要求传入的高亮范围有序且不重叠。
                                                        // 日志语法高亮和选区高亮经常覆盖同一段时间戳、等级或线程名，因此必须先拆分合并，
                                                        // 让选区背景和原有文字颜色同时保留，避免选中文本时渲染错位。
                                                        line_highlights = gpui::combine_highlights(
                                                            line_highlights,
                                                            [(
                                                                range,
                                                                Self::log_text_selection_highlight_style(),
                                                            )],
                                                        )
                                                        .collect();
                                                    }
                                                    let expanded_line =
                                                        Self::expanded_log_line_for_display(&line);
                                                    let display_highlights =
                                                        Self::map_log_highlights_to_display(
                                                            line_highlights,
                                                            &expanded_line,
                                                        );
                                                    Some((
                                                        index,
                                                        line,
                                                        expanded_line.text,
                                                        display_highlights,
                                                        highlighted_search_line == Some(index),
                                                    ))
                                            })
                                            .collect::<Vec<_>>()
                                    })
                                    .unwrap_or_default();

                                lines
                                    .into_iter()
                                    .map(|(
                                        index,
                                        line,
                                        display_line,
                                        highlights,
                                        search_highlighted,
                                    )| {
                                        let horizontal_line_number_offset =
                                            -row_scroll_handle.0.borrow().base_handle.offset().x;
                                        Self::render_log_line(LogLineRenderData {
                                            tab_id,
                                            line_index: index,
                                            line,
                                            display_line,
                                            highlights,
                                            line_number_width,
                                            font_size: view.log_viewer_font_size,
                                            horizontal_line_number_offset,
                                            horizontal_content_offset: px(0.0),
                                            search_highlighted,
                                            suppress_hover: view.search_results_resize_drag.is_some()
                                                || view.log_scrollbar_drag.is_some(),
                                            palette: view.palette(),
                                        }, context)
                                    })
                                    .collect::<Vec<_>>()
                            },
                        ),
                    )
                    .with_width_from_item(Some(horizontal_measure_line_index))
                    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                    .size_full()
                    .track_scroll(scroll_handle.clone()),
                )
                .child(self.render_log_vertical_scrollbar(
                    tab_id,
                    &scroll_handle,
                    line_count,
                    context,
                ))
                .child(self.render_log_horizontal_scrollbar(
                    tab_id,
                    &scroll_handle,
                    line_number_width,
                    context,
                )),
        )
    }

    /// 渲染分页日志正文查看器。
    ///
    /// 业务意图：
    /// - 分页日志可能有数千万行，完整 `uniform_list` 会把行号乘以固定行高后交给 `f32` 像素坐标，滚到深处会出现行间距和重叠。
    /// - 这里只渲染当前视口附近的一小段真实行号，纵向滚动位置由 `PagedLogScrollState` 的 `f64` 逻辑坐标保存。
    /// - 行号、选区、高亮和右键菜单仍复用普通日志行渲染逻辑，保证两种模式的视觉行为一致。
    fn render_paged_log_document_viewer(
        &self,
        tab: &OpenLogTab,
        document: &paged_document::PagedLogDocument,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let tab_id = tab.id;
        let line_count = document.line_count();
        let line_number_width = Self::log_viewer_line_number_width(line_count);
        let palette = self.palette();
        let syntax_theme = self.effective_theme().syntax_theme();
        let viewport_handle = tab.paged_viewport_handle.clone();
        let viewport_height = viewport_handle.bounds().size.height;
        let visible_rows = Self::paged_log_visible_row_capacity(viewport_height);
        let max_vertical_scroll =
            Self::paged_log_vertical_max_scroll_px(line_count, viewport_height);
        let scroll_top = tab.paged_scroll.top_px.clamp(0.0, max_vertical_scroll);
        let (first_line_index, fractional_top) =
            Self::paged_log_visible_start(scroll_top, line_count);
        let horizontal_content_offset = px(-(tab.paged_scroll.left_px as f32));
        let text_selection = tab.text_selection.clone();
        let highlighted_search_line = tab.highlighted_search_line;

        let rows = (0..visible_rows)
            .filter_map(|row_offset| {
                let line_index = first_line_index.checked_add(row_offset)?;
                if line_index >= line_count {
                    return None;
                }
                let line = document.read_line(line_index).ok().flatten()?.text;
                let mut line_highlights =
                    highlight_line(document.highlight_mode, &line, None, syntax_theme);
                if let Some(selection) = &text_selection
                    && let Some(range) =
                        Self::selected_byte_range_for_line(selection, line_index, &line)
                {
                    // 分页模式仍需要和内存模式一样先合并语法高亮和选区高亮，避免重叠范围让 GPUI 文本绘制错位。
                    line_highlights = gpui::combine_highlights(
                        line_highlights,
                        [(range, Self::log_text_selection_highlight_style())],
                    )
                    .collect();
                }
                let expanded_line = Self::expanded_log_line_for_display(&line);
                let display_highlights =
                    Self::map_log_highlights_to_display(line_highlights, &expanded_line);
                let row_top = row_offset as f32 * LOG_VIEWER_ROW_HEIGHT - fractional_top;

                Some(
                    div()
                        .absolute()
                        .left(px(0.0))
                        .right(px(0.0))
                        .top(px(row_top))
                        .h(px(LOG_VIEWER_ROW_HEIGHT))
                        .child(Self::render_log_line(
                            LogLineRenderData {
                                tab_id,
                                line_index,
                                line,
                                display_line: expanded_line.text,
                                highlights: display_highlights,
                                line_number_width,
                                font_size: self.log_viewer_font_size,
                                horizontal_line_number_offset: px(0.0),
                                horizontal_content_offset,
                                search_highlighted: highlighted_search_line == Some(line_index),
                                suppress_hover: self.search_results_resize_drag.is_some()
                                    || self.log_scrollbar_drag.is_some(),
                                palette,
                            },
                            context,
                        )),
                )
            })
            .collect::<Vec<_>>();

        let viewer = div()
            .id(SharedString::from(format!("log-viewer-{}", tab_id)))
            .flex()
            .flex_col()
            .flex_1()
            .overflow_hidden()
            .bg(rgb(palette.background));

        let viewer = if let Some(warning) = document.warning.as_deref() {
            viewer.child(
                div()
                    .flex_none()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(if self.effective_theme() == EffectiveTheme::Dark {
                        0xffd33d
                    } else {
                        0x9a6700
                    }))
                    .bg(rgb(palette.search_highlight))
                    .border_b_1()
                    .border_color(rgb(palette.border))
                    .child(warning.to_string()),
            )
        } else {
            viewer
        };

        viewer.child(
            div()
                .id("log-viewer-body")
                .relative()
                .flex()
                .flex_1()
                .overflow_hidden()
                .bg(rgb(palette.background))
                .track_scroll(&viewport_handle)
                .on_scroll_wheel(context.listener(
                    move |view, event: &ScrollWheelEvent, _window, context| {
                        view.note_keyboard_scroll_region(KeyboardScrollRegion::LogContent);
                        view.handle_paged_log_scroll_wheel(tab_id, event, context);
                        context.stop_propagation();
                    },
                ))
                .on_mouse_move(context.listener(
                    |view, _event: &MouseMoveEvent, _window, _context| {
                        view.note_keyboard_scroll_region(KeyboardScrollRegion::LogContent);
                    },
                ))
                .on_mouse_down(
                    MouseButton::Left,
                    context.listener(|view, _event: &MouseDownEvent, _window, _context| {
                        view.note_keyboard_scroll_region(KeyboardScrollRegion::LogContent);
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    context.listener(move |view, event: &MouseDownEvent, _window, context| {
                        view.note_keyboard_scroll_region(KeyboardScrollRegion::LogContent);
                        view.open_log_viewer_context_menu(
                            tab_id,
                            f32::from(event.position.x),
                            f32::from(event.position.y),
                            context,
                        );
                        context.stop_propagation();
                    }),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(0.0))
                        .top(px(0.0))
                        .h_full()
                        .w(px(line_number_width))
                        .bg(rgb(palette.panel))
                        .border_r_1()
                        .border_color(rgb(palette.border)),
                )
                .children(rows)
                .child(self.render_log_vertical_scrollbar_for_tab(tab, context))
                .child(self.render_log_horizontal_scrollbar_for_tab(tab, context)),
        )
    }

    /// 根据总行数计算行号列宽度。
    ///
    /// 业务意图：
    /// - 小文件不需要占用上一版 68px 宽度；行数增长时再按数字位数适度增加。
    /// - 该宽度同时用于行号背景和每一行的行号单元格，保证未填满高度时背景也能对齐。
    pub(super) fn log_viewer_line_number_width(line_count: usize) -> f32 {
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
    pub(super) fn render_log_vertical_scrollbar(
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
    pub(super) fn render_log_horizontal_scrollbar(
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
    fn render_log_vertical_scrollbar_for_tab(
        &self,
        tab: &OpenLogTab,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = self.palette();
        let tab_id = tab.id;
        let Some(metrics) = self
            .log_scrollbar_metrics_for_tab(tab, LogScrollbarAxis::Vertical)
            .or_else(|| match &tab.state {
                LogTabState::Ready {
                    document: LogTabDocument::Paged(document),
                } => Self::fallback_log_vertical_scrollbar_metrics(document.line_count()),
                LogTabState::Ready {
                    document: LogTabDocument::InMemory(_),
                }
                | LogTabState::Loading { .. }
                | LogTabState::Failed { .. } => None,
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
    fn render_log_horizontal_scrollbar_for_tab(
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
    pub(super) fn log_vertical_scrollbar_metrics(
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
    pub(super) fn log_horizontal_scrollbar_metrics(
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
    pub(super) fn paged_log_visible_row_capacity(viewport_height: Pixels) -> usize {
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
    pub(super) fn paged_log_visible_start(scroll_top_px: f64, line_count: usize) -> (usize, f32) {
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
    pub(super) fn paged_log_vertical_max_scroll_px(
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
    pub(super) fn paged_log_scroll_top_for_line(
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
    fn paged_log_estimated_content_width(
        document: &paged_document::PagedLogDocument,
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
    fn paged_log_horizontal_max_scroll_px(
        document: &paged_document::PagedLogDocument,
        viewport_width: Pixels,
        line_number_width: f32,
        font_size: f32,
    ) -> f64 {
        let content_width =
            Self::paged_log_estimated_content_width(document, line_number_width, font_size);
        (content_width - f64::from(viewport_width).max(1.0)).max(0.0)
    }

    /// 计算分页日志纵向滚动条滑块。
    fn paged_log_vertical_scrollbar_metrics(
        tab: &OpenLogTab,
        document: &paged_document::PagedLogDocument,
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
    fn paged_log_horizontal_scrollbar_metrics(
        tab: &OpenLogTab,
        document: &paged_document::PagedLogDocument,
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
    pub(super) fn fallback_log_vertical_scrollbar_metrics(
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
    pub(super) fn handle_paged_log_scroll_wheel(
        &mut self,
        tab_id: usize,
        event: &ScrollWheelEvent,
        context: &mut Context<Self>,
    ) {
        let font_size = self.log_viewer_font_size;
        let pixel_delta = event.delta.pixel_delta(px(20.0));
        {
            let Some(tab) = self.open_tabs.iter_mut().find(|tab| tab.id == tab_id) else {
                return;
            };
            let LogTabState::Ready {
                document: LogTabDocument::Paged(document),
            } = &tab.state
            else {
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
        self.log_viewer_context_menu = None;
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
    pub(super) fn start_log_scrollbar_drag(
        &mut self,
        tab_id: usize,
        axis: LogScrollbarAxis,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        self.note_keyboard_scroll_region(KeyboardScrollRegion::LogContent);
        let Some(tab) = self.open_tabs.iter().find(|tab| tab.id == tab_id) else {
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
        self.log_scrollbar_drag = Some(LogScrollbarDrag {
            tab_id,
            axis,
            cursor_offset: pointer_position - absolute_thumb_start,
        });
        self.tab_context_menu = None;
        self.encoding_dropdown_menu = None;
        self.log_viewer_context_menu = None;
        self.stop_log_text_selection(context);
    }

    /// 根据鼠标移动更新日志正文滚动条拖动结果。
    ///
    /// 业务意图：
    /// - 自绘滚动条拖动必须反向写入 `uniform_list` 底层滚动偏移，才能让虚拟列表、滚轮滚动和滑块位置保持同源。
    /// - 只在鼠标左键仍处于按下状态时更新；如果系统报告左键已释放，则立即清理拖动状态。
    pub(super) fn update_log_scrollbar_drag(
        &mut self,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        let Some(drag) = self.log_scrollbar_drag else {
            return;
        };
        if !event.dragging() {
            self.log_scrollbar_drag = None;
            context.notify();
            return;
        }

        let (metrics, viewport_origin, is_paged) = {
            let Some(tab) = self.open_tabs.iter().find(|tab| tab.id == drag.tab_id) else {
                self.log_scrollbar_drag = None;
                context.notify();
                return;
            };
            let Some(metrics) = self.log_scrollbar_metrics_for_tab(tab, drag.axis) else {
                self.log_scrollbar_drag = None;
                context.notify();
                return;
            };
            let Some(viewport_origin) =
                Self::log_scrollbar_viewport_axis_origin_for_tab(tab, drag.axis)
            else {
                self.log_scrollbar_drag = None;
                context.notify();
                return;
            };
            (
                metrics,
                viewport_origin,
                matches!(
                    &tab.state,
                    LogTabState::Ready {
                        document: LogTabDocument::Paged(_)
                    }
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
            if let Some(tab) = self.open_tabs.iter_mut().find(|tab| tab.id == drag.tab_id) {
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
        } else if let Some(tab) = self.open_tabs.iter().find(|tab| tab.id == drag.tab_id) {
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
    pub(super) fn stop_log_scrollbar_drag(&mut self, context: &mut Context<Self>) {
        if self.log_scrollbar_drag.is_some() {
            self.log_scrollbar_drag = None;
            context.notify();
        }
    }

    /// 开始选择日志正文文本。
    ///
    /// 业务意图：
    /// - 当前日志查看器是虚拟列表自绘，鼠标按下时需要主动记录选择锚点，后续拖动才能跨行扩展选区。
    /// - 点击日志正文同时关闭浮层菜单，避免选区操作和 tab 菜单、编码菜单叠加造成误操作。
    /// - 点击正文后主动聚焦主窗口根节点，让 `Ctrl/Cmd+C` 走根节点键盘兜底入口，从而复制自绘选区。
    ///
    /// 边界条件：
    /// - 只响应当前仍存在且已解码的 tab；加载中或失败状态没有可选择的正文。
    /// - 单击会形成空选择，视觉上不高亮，但会清理上一次选择，符合常见文本查看器行为。
    /// - 如果正在拖动日志滚动条或搜索结果面板高度，则正文不应进入选区模式，避免控件拖动被误解为文本拖选。
    pub(super) fn start_log_text_selection(
        &mut self,
        tab_id: usize,
        line_index: usize,
        line: &str,
        event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.note_keyboard_scroll_region(KeyboardScrollRegion::LogContent);
        if self.search_results_resize_drag.is_some() || self.log_scrollbar_drag.is_some() {
            return;
        }

        let Some(position) =
            self.log_text_position_from_pointer(tab_id, line_index, line, event.position.x, window)
        else {
            return;
        };
        let Some(tab) = self.open_tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };
        if !matches!(&tab.state, LogTabState::Ready { .. }) {
            return;
        }

        window.focus(&self.root_focus_handle);
        let text_selection = match event.click_count {
            0 | 1 => LogTextSelection {
                anchor: position,
                focus: position,
            },
            2 => Self::word_selection_for_position(line_index, line, position).unwrap_or(
                LogTextSelection {
                    anchor: position,
                    focus: position,
                },
            ),
            _ => Self::line_selection_for_line(line_index, line),
        };

        tab.text_selection = Some(text_selection);
        tab.selection_drag_anchor = (event.click_count <= 1).then_some(position);
        self.tab_context_menu = None;
        self.encoding_dropdown_menu = None;
        self.log_viewer_context_menu = None;
        context.notify();
    }

    /// 构造整行选择范围。
    ///
    /// 业务意图：
    /// - 三连击是日志查看器中快速复制当前行的常见操作，需要直接选中当前可见行的全部真实字符。
    /// - 这里只选择行内文本，不主动附加换行符；跨行换行仍由 `selected_log_text` 在多行选择时统一处理。
    pub(super) fn line_selection_for_line(line_index: usize, line: &str) -> LogTextSelection {
        LogTextSelection {
            anchor: LogTextPosition {
                line_index,
                column: 0,
            },
            focus: LogTextPosition {
                line_index,
                column: line.chars().count(),
            },
        }
    }

    /// 根据点击位置构造当前日志 token 的选择范围。
    ///
    /// 业务意图：
    /// - 双击应选中“当前单词”，用户期望的是两个符号之间的连续文字，而不是整段包名、路径或线程名。
    /// - 因此 `.`、`-`、`_`、`:`、`/`、括号、引号等所有符号都作为边界，只保留连续字母/数字内容。
    ///
    /// 边界条件：
    /// - 如果用户双击在 token 右边界附近，命中列可能落在 token 后一列，此时优先回退到前一个字符。
    /// - 如果双击在纯空白或结构分隔符上，则返回 `None`，调用方退化为单点选择。
    pub(super) fn word_selection_for_position(
        line_index: usize,
        line: &str,
        position: LogTextPosition,
    ) -> Option<LogTextSelection> {
        let chars = line.chars().collect::<Vec<_>>();
        if chars.is_empty() {
            return None;
        }

        let original_column = position.column;
        let mut token_column = original_column.min(chars.len().saturating_sub(1));
        if !Self::is_log_word_char(chars[token_column])
            && token_column > 0
            && (original_column >= chars.len()
                || chars[token_column].is_whitespace()
                || Self::is_log_word_right_boundary_char(chars[token_column]))
            && Self::is_log_word_char(chars[token_column - 1])
        {
            token_column -= 1;
        }
        if !Self::is_log_word_char(chars[token_column]) {
            return None;
        }

        let mut start_column = token_column;
        while start_column > 0 && Self::is_log_word_char(chars[start_column - 1]) {
            start_column -= 1;
        }

        let mut end_column = token_column + 1;
        while end_column < chars.len() && Self::is_log_word_char(chars[end_column]) {
            end_column += 1;
        }

        Some(LogTextSelection {
            anchor: LogTextPosition {
                line_index,
                column: start_column,
            },
            focus: LogTextPosition {
                line_index,
                column: end_column,
            },
        })
    }

    /// 判断字符是否属于日志双击选择 token。
    ///
    /// 业务意图：
    /// - 双击选词按“两个符号之间的文字”处理，所有标点、路径分隔符、类名分隔符和键值分隔符都不能进入选区。
    /// - `is_alphanumeric` 覆盖英文、数字和中文等 Unicode 字母数字，避免中文日志中的普通词被拆坏。
    pub(super) fn is_log_word_char(character: char) -> bool {
        character.is_alphanumeric()
    }

    /// 判断当前符号是否允许按右边界回退到前一个单词。
    ///
    /// 业务意图：
    /// - GPUI 文本命中测试在用户双击单词右边缘时，可能返回后一个符号的插入列；Java 堆栈里的 `.` 最常见。
    /// - 这类符号仍然是单词边界，不进入选区，但允许回退到左侧单词，避免双击 `AESCipher.` 边缘时什么都不选。
    ///
    /// 边界条件：
    /// - `=` 等键值分隔符不做回退，用户双击字段分隔符时不应误选左侧 key。
    pub(super) fn is_log_word_right_boundary_char(character: char) -> bool {
        matches!(character, '.' | ')' | ']' | '}' | '>' | '"' | '\'')
    }

    /// 根据鼠标拖动更新日志正文选区。
    ///
    /// 业务意图：
    /// - 拖动过程中锚点保持不变，只更新焦点位置，从而支持任意方向选择。
    /// - 该函数只更新可见行上的拖动结果；虚拟列表外自动滚动选择后续需要单独定义交互规则。
    /// - 结果面板调高时，鼠标可能经过日志行，此时必须忽略底层正文选择，避免事件穿透造成误选。
    /// - 拖动日志正文滚动条时也必须忽略正文选择，避免滚动条拖动过程中出现选区和 hover 闪动。
    pub(super) fn update_log_text_selection(
        &mut self,
        tab_id: usize,
        line_index: usize,
        line: &str,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if self.search_results_resize_drag.is_some() || self.log_scrollbar_drag.is_some() {
            self.stop_log_text_selection(context);
            return;
        }

        if !event.dragging() {
            self.stop_log_text_selection(context);
            return;
        }

        let Some(anchor) = self
            .open_tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .and_then(|tab| tab.selection_drag_anchor)
        else {
            return;
        };
        let Some(position) =
            self.log_text_position_from_pointer(tab_id, line_index, line, event.position.x, window)
        else {
            return;
        };
        let Some(tab) = self.open_tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };

        tab.text_selection = Some(LogTextSelection {
            anchor,
            focus: position,
        });
        context.notify();
    }

    /// 结束日志正文选区拖动。
    ///
    /// 业务意图：
    /// - 鼠标释放后保留最终选区用于复制和搜索预填，但清理拖动锚点，避免下一次鼠标移动继续扩展旧选区。
    pub(super) fn stop_log_text_selection(&mut self, context: &mut Context<Self>) {
        let mut changed = false;
        for tab in &mut self.open_tabs {
            if tab.selection_drag_anchor.take().is_some() {
                changed = true;
            }
        }
        if changed {
            context.notify();
        }
    }

    /// 将鼠标横坐标换算为日志正文字符位置。
    ///
    /// 业务意图：
    /// - GPUI 鼠标事件提供窗口坐标，而日志正文在虚拟列表内会随横向滚动平移；命中测试必须把行号列、正文内边距和滚动偏移统一扣除。
    /// - 输出仍是字符列，复制和高亮时再按真实字符串转换为 UTF-8 字节范围。
    /// - 鼠标列计算必须复用 GPUI 文本系统的真实 shaping 结果，不能再用固定字符宽度估算；
    ///   否则行首空格、缩进、字体实际 advance 或平台字体渲染差异都会让视觉选区和真实文本列错位。
    ///
    /// 边界条件：
    /// - 指针落在正文起点左侧时归到第 0 列，落在行尾右侧时归到最后一列，避免越界。
    /// - `LineLayout::closest_index_for_x` 返回 UTF-8 字节下标，必须再转换为字符列，保证中文和 emoji 不会被切断。
    /// - shape 结果由 GPUI 按文本内容和字体缓存；拖动选择同一行时不会每帧都完整重建字形布局。
    pub(super) fn log_text_position_from_pointer(
        &self,
        tab_id: usize,
        line_index: usize,
        line: &str,
        pointer_x: Pixels,
        window: &mut Window,
    ) -> Option<LogTextPosition> {
        let tab = self.open_tabs.iter().find(|tab| tab.id == tab_id)?;
        let LogTabState::Ready { document } = &tab.state else {
            return None;
        };

        let line_number_width = Self::log_viewer_line_number_width(document.line_count());
        let (bounds, horizontal_offset) = match document {
            LogTabDocument::Paged(_) => (
                tab.paged_viewport_handle.bounds(),
                px(-(tab.paged_scroll.left_px as f32)),
            ),
            LogTabDocument::InMemory(_) => {
                let scroll_state = tab.scroll_handle.0.borrow();
                (
                    scroll_state.base_handle.bounds(),
                    scroll_state.base_handle.offset().x,
                )
            }
        };
        if bounds.size.width <= px(0.0) {
            return Some(LogTextPosition {
                line_index,
                column: 0,
            });
        }

        let text_origin_x = bounds.left()
            + horizontal_offset
            + px(line_number_width + LOG_VIEWER_TEXT_LEFT_PADDING);
        let text_relative_x = pointer_x - text_origin_x;
        if line.is_empty() || text_relative_x <= px(0.0) {
            return Some(LogTextPosition {
                line_index,
                column: 0,
            });
        }

        let mut text_style = window.text_style();
        text_style.font_family = LOG_VIEWER_FONT_FAMILY.into();
        text_style.font_size = px(self.log_viewer_font_size).into();
        let expanded_line = Self::expanded_log_line_for_display(line);
        let run = TextRun {
            len: expanded_line.text.len(),
            font: text_style.font(),
            color: text_style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let shaped_line = window.text_system().shape_line(
            SharedString::from(expanded_line.text.clone()),
            font_size,
            &[run],
            None,
        );
        let display_byte_index = shaped_line.closest_index_for_x(text_relative_x);
        let original_byte_index =
            Self::original_byte_index_for_display_byte(&expanded_line, display_byte_index);
        let column = Self::char_column_for_byte_index(line, original_byte_index);

        Some(LogTextPosition { line_index, column })
    }

    /// 构造日志正文选区高亮样式。
    ///
    /// 业务意图：
    /// - 语法高亮按文字颜色表达，选区属于交互反馈，需要使用浅色背景以接近系统文本选择体验。
    /// - 字体颜色保持默认，避免复制选区时影响日志级别、时间戳等已有高亮的可读性。
    pub(super) fn log_text_selection_highlight_style() -> gpui::HighlightStyle {
        gpui::HighlightStyle {
            background_color: Some(rgb(0xcfe8ff).into()),
            ..Default::default()
        }
    }

    /// 按 tab 和方向取得当前滚动条的真实测量数据。
    ///
    /// 业务意图：
    /// - 渲染、按下和拖动都复用同一套测量函数；横向滚动条需要根据当前日志行数计算行号列宽。
    /// - 只有处于已解码状态的 tab 才可能产生横向滚动条，因为加载和失败状态没有正文列表。
    pub(super) fn log_scrollbar_metrics_for_tab(
        &self,
        tab: &OpenLogTab,
        axis: LogScrollbarAxis,
    ) -> Option<LogScrollbarMetrics> {
        match &tab.state {
            LogTabState::Ready {
                document: LogTabDocument::Paged(document),
            } => match axis {
                LogScrollbarAxis::Vertical => {
                    Self::paged_log_vertical_scrollbar_metrics(tab, document)
                }
                LogScrollbarAxis::Horizontal => Self::paged_log_horizontal_scrollbar_metrics(
                    tab,
                    document,
                    self.log_viewer_font_size,
                ),
            },
            LogTabState::Ready {
                document: LogTabDocument::InMemory(document),
            } => match axis {
                LogScrollbarAxis::Vertical => {
                    Self::log_vertical_scrollbar_metrics(&tab.scroll_handle)
                }
                LogScrollbarAxis::Horizontal => Self::log_horizontal_scrollbar_metrics(
                    &tab.scroll_handle,
                    Self::log_viewer_line_number_width(document.line_count()),
                ),
            },
            LogTabState::Loading { .. } | LogTabState::Failed { .. } => None,
        }
    }

    /// 取得日志正文滚动条视口在当前轴向上的窗口坐标起点。
    ///
    /// 业务意图：
    /// - 普通日志使用 `uniform_list` 的底层 `ScrollHandle` bounds。
    /// - 分页日志使用独立视口测量句柄，避免为了坐标换算重新依赖完整虚拟列表。
    fn log_scrollbar_viewport_axis_origin_for_tab(
        tab: &OpenLogTab,
        axis: LogScrollbarAxis,
    ) -> Option<Pixels> {
        let bounds = match &tab.state {
            LogTabState::Ready {
                document: LogTabDocument::Paged(_),
            } => tab.paged_viewport_handle.bounds(),
            LogTabState::Ready {
                document: LogTabDocument::InMemory(_),
            }
            | LogTabState::Loading { .. }
            | LogTabState::Failed { .. } => tab.scroll_handle.0.borrow().base_handle.bounds(),
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
    pub(super) fn uniform_list_viewport_axis_origin(
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
    /// - 搜索结果跳转的目标行允许使用整行背景提示，这是定位反馈，不属于语法高亮规则。
    pub(super) fn render_log_line(
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
                    .text_color(rgb(palette.muted_text))
                    .bg(rgb(if search_highlighted {
                        palette.search_highlight
                    } else {
                        palette.panel
                    }))
                    .border_r_1()
                    .border_color(rgb(palette.border))
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

    /// 打开日志正文右键菜单。
    ///
    /// 业务意图：
    /// - 菜单位置使用右键点击位置，并转换成右侧日志面板内部坐标，保证单日志模式和左右分栏模式都能正确定位。
    /// - 打开正文菜单时关闭其它右侧弹层，避免多个自绘菜单重叠导致命令作用对象不清晰。
    pub(super) fn open_log_viewer_context_menu(
        &mut self,
        tab_id: usize,
        window_x: f32,
        window_y: f32,
        context: &mut Context<Self>,
    ) {
        if !self.open_tabs.iter().any(|tab| tab.id == tab_id) {
            return;
        }
        let panel_x = (window_x - self.right_panel_left_offset()).max(0.0);
        let panel_y = (window_y - TOOLBAR_HEIGHT).max(0.0);
        self.log_viewer_context_menu = Some(LogViewerContextMenu {
            tab_id,
            x: panel_x,
            y: panel_y,
        });
        self.tab_context_menu = None;
        self.search_results_context_menu = None;
        self.encoding_dropdown_menu = None;
        context.notify();
    }

    /// 渲染日志正文右键菜单。
    ///
    /// 业务意图：
    /// - 正文查看器是自绘只读列表，复制和另存为需要应用自己提供菜单，不能依赖平台文本控件菜单。
    pub(super) fn render_log_viewer_context_menu(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(menu) = &self.log_viewer_context_menu else {
            return div().id("log-viewer-context-menu-empty").hidden();
        };
        let tab_id = menu.tab_id;
        let palette = self.palette();
        let copy_enabled = self.selected_log_text_for_tab(tab_id).is_some();

        div()
            .id("log-viewer-context-menu")
            .absolute()
            .left(px(menu.x))
            .top(px(menu.y))
            .w(px(LOG_VIEWER_CONTEXT_MENU_WIDTH))
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.menu))
            .shadow_lg()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .child(self.render_log_viewer_context_menu_item(
                tab_id,
                LogViewerContextMenuAction::Copy,
                "复制",
                copy_enabled,
                palette,
                context,
            ))
            .child(self.render_log_viewer_context_menu_item(
                tab_id,
                LogViewerContextMenuAction::SaveAs,
                "另存为...",
                true,
                palette,
                context,
            ))
    }

    /// 渲染日志正文右键菜单单项。
    ///
    /// 业务意图：
    /// - “复制”在没有选区时仍展示但禁用，符合用户要求的“有选中内容时，可以点击”。
    pub(super) fn render_log_viewer_context_menu_item(
        &self,
        tab_id: usize,
        action: LogViewerContextMenuAction,
        label: &'static str,
        enabled: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!(
                "log-viewer-menu-{}-{}",
                tab_id, label
            )))
            .flex()
            .items_center()
            .h(px(LOG_VIEWER_CONTEXT_MENU_ITEM_HEIGHT))
            .px_3()
            .text_sm()
            .text_color(rgb(if enabled {
                palette.text
            } else {
                palette.muted_text
            }))
            .cursor_pointer()
            .when(enabled, move |item| {
                item.hover(move |item| item.bg(rgb(palette.hover)))
            })
            .when(!enabled, |item| item.opacity(0.55))
            .child(label)
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    if enabled {
                        view.handle_log_viewer_context_menu_action(tab_id, action, context);
                    }
                    context.stop_propagation();
                }),
            )
    }

    /// 执行日志正文右键菜单命令。
    ///
    /// 业务意图：
    /// - 复制和另存为都绑定右键时的 tab，避免菜单打开后因其它事件切换 active tab 导致作用对象变化。
    pub(super) fn handle_log_viewer_context_menu_action(
        &mut self,
        tab_id: usize,
        action: LogViewerContextMenuAction,
        context: &mut Context<Self>,
    ) {
        self.log_viewer_context_menu = None;
        self.tab_context_menu = None;
        self.encoding_dropdown_menu = None;
        self.search_results_context_menu = None;
        match action {
            LogViewerContextMenuAction::Copy => {
                let _ = self.copy_selected_log_text_for_tab(tab_id, context);
            }
            LogViewerContextMenuAction::SaveAs => {
                if let Some(source) = self.log_viewer_save_source_for_tab(tab_id) {
                    self.save_log_sources_as(vec![source], context);
                }
            }
        }
        context.notify();
    }

    /// 返回日志正文右键菜单绑定 tab 的另存为来源。
    ///
    /// 业务意图：
    /// - 正文右键菜单命令应始终作用于打开菜单时所在的 tab，不能依赖当前激活 tab，避免用户切换 tab 后保存错文件。
    pub(super) fn log_viewer_save_source_for_tab(&self, tab_id: usize) -> Option<LogFileSource> {
        Self::log_viewer_save_source_for_tab_from_tabs(&self.open_tabs, tab_id)
    }

    /// 从打开 tab 集合中查找正文右键菜单绑定的另存为来源。
    ///
    /// 业务意图：
    /// - 拆成纯函数便于测试，避免右键菜单另存为入口因为 tab 查找错误而无声失败。
    pub(super) fn log_viewer_save_source_for_tab_from_tabs(
        tabs: &[OpenLogTab],
        tab_id: usize,
    ) -> Option<LogFileSource> {
        tabs.iter()
            .find(|tab| tab.id == tab_id)
            .map(|tab| tab.source.clone())
    }
}
