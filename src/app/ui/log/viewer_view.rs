// 日志 tab、编码选择器、日志正文查看器和正文右键菜单方法。
//
// 业务意图：
// - 这些方法仍属于 `MainView` 的实现块，但物理上从 `app.rs` 拆出，降低根文件体积。
// - 分页日志在本文件内使用窗口化渲染，避免超大行号对应的绝对像素坐标触发 `f32` 精度问题。
//
// 边界条件：
// - 该文件作为 `app` 的子模块自行声明 `impl MainView`，跨模块调用通过 `pub(in crate::app)` 方法显式暴露。

use super::*;

/// 日志正文搜索关键字片段的暖橙色背景。
///
/// 业务意图：
/// - 搜索定位统一同时展示整行黄色背景和关键字片段背景；整行黄色负责定位行，关键字暖橙色负责定位列。
/// - 使用固定色值而不是主题搜索色，避免关键字和整行同色后在长日志中难以区分命中位置，同时避免高饱和橘红色长时间阅读刺眼。
pub(in crate::app) const LOG_SEARCH_KEYWORD_HIGHLIGHT: u32 = 0xf2a65a;

impl MainView {
    pub(in crate::app) fn render_log_tab_body(
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
    pub(in crate::app) fn render_log_tab_loading_message(
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
    pub(in crate::app) fn render_loading_spinner(icon_color: u32) -> impl IntoElement {
        div()
            .id("log-tab-loading-dots")
            .flex()
            .items_center()
            .gap_1()
            .h(px(28.0))
            .children((0..3).map(move |index| Self::render_loading_dot(index, icon_color)))
    }

    /// 返回指定日志行上的搜索命中片段高亮。
    ///
    /// 业务意图：
    /// - 搜索结果跳转和“上一个/下一个”会在行容器上绘制整行定位背景；
    ///   这里额外返回暖橙色关键字片段背景，让用户同时知道命中行和命中列。
    /// - 命中范围来自搜索时的原始日志行，因此这里在渲染前夹紧到当前行的 UTF-8 边界，避免文件重载或编码切换后旧范围越界。
    pub(in crate::app) fn log_search_match_highlight_for_line(
        highlight: Option<&LogSearchMatchHighlight>,
        line_index: usize,
        line: &str,
        _palette: AppThemePalette,
    ) -> Option<(Range<usize>, gpui::HighlightStyle)> {
        let highlight = highlight?;
        if highlight.line_index != line_index {
            return None;
        }
        let range = Self::clamp_search_text_range(line, highlight.match_range.clone());
        if range.start >= range.end {
            return None;
        }
        Some((
            range,
            gpui::HighlightStyle {
                background_color: Some(rgb(LOG_SEARCH_KEYWORD_HIGHLIGHT).into()),
                font_weight: Some(FontWeight::SEMIBOLD),
                ..Default::default()
            },
        ))
    }

    /// 合并日志行高亮并保证鼠标选区背景优先显示。
    ///
    /// 业务意图：
    /// - 日志正文同一段文本可能同时拥有语法高亮、搜索关键字高亮和鼠标选区高亮。
    /// - 用户正在拖选文本时，选区是最强交互反馈；即使命中关键字落在选区内部，也应显示为被选中，而不是继续显示搜索暖橙色背景。
    ///
    /// 实现原因：
    /// - `gpui::combine_highlights` 会用集合合并重叠样式，重叠背景的最终来源不适合作为稳定的业务优先级。
    /// - 这里先把选区覆盖范围内已有高亮的背景清空，只保留文字颜色、粗体等非背景样式，再叠加选区背景。
    ///
    /// 边界条件：
    /// - 选区外的搜索关键字背景保持不变。
    /// - 选区内的搜索关键字仍保留粗体等文本样式，只是不再覆盖选区背景。
    pub(in crate::app) fn combine_log_highlights_with_selection(
        highlights: Vec<(Range<usize>, gpui::HighlightStyle)>,
        selection_range: Range<usize>,
    ) -> Vec<(Range<usize>, gpui::HighlightStyle)> {
        if selection_range.start >= selection_range.end {
            return highlights;
        }

        let mut adjusted_highlights = Vec::with_capacity(highlights.len().saturating_add(2));
        for (range, style) in highlights {
            if range.end <= selection_range.start || range.start >= selection_range.end {
                adjusted_highlights.push((range, style));
                continue;
            }

            if range.start < selection_range.start {
                let before_end = selection_range.start.min(range.end);
                if range.start < before_end {
                    adjusted_highlights.push((range.start..before_end, style));
                }
            }

            let overlap_start = range.start.max(selection_range.start);
            let overlap_end = range.end.min(selection_range.end);
            if overlap_start < overlap_end {
                let mut foreground_style = style;
                foreground_style.background_color = None;
                adjusted_highlights.push((overlap_start..overlap_end, foreground_style));
            }

            if selection_range.end < range.end {
                let after_start = selection_range.end.max(range.start);
                if after_start < range.end {
                    adjusted_highlights.push((after_start..range.end, style));
                }
            }
        }

        gpui::combine_highlights(
            adjusted_highlights,
            [(selection_range, Self::log_text_selection_highlight_style())],
        )
        .collect()
    }

    /// 渲染单个加载脉冲点。
    ///
    /// 业务意图：
    /// - 每个点使用相同动画周期但不同相位，形成从左到右流动的加载感。
    /// - 点元素尺寸固定，动画期间只改变透明度，避免布局抖动。
    pub(in crate::app) fn render_loading_dot(index: usize, icon_color: u32) -> impl IntoElement {
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
    pub(in crate::app) fn loading_dot_opacity(delta: f32, phase_offset: f32) -> f32 {
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
    pub(in crate::app) fn render_log_tab_state_message(
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
    pub(in crate::app) fn render_log_document_viewer(
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
                .child(
                    div()
                        .id("log-viewer-content")
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .h_full()
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
                                        let palette = view.palette();
                                        let log_snapshot = view
                                            .log
                                            .open_tabs
                                            .iter()
                                            .find(|tab| tab.id == tab_id)
                                            .and_then(|tab| match &tab.state {
                                                LogTabState::Ready { document } => Some((
                                                    document,
                                                    tab.highlighted_search_line,
                                                    tab.highlighted_search_match.clone(),
                                                    tab.text_selection.clone(),
                                                    tab.marked_lines.clone(),
                                                )),
                                                LogTabState::Loading { .. }
                                                | LogTabState::Failed { .. } => None,
                                            });
                                        let lines = if let Some((
                                            document,
                                            highlighted_search_line,
                                            highlighted_search_match,
                                            text_selection,
                                            marked_lines,
                                        )) = log_snapshot
                                        {
                                            range
                                                .filter_map(|index| {
                                                    let line = match document.as_ref() {
                                                        LogTabDocument::InMemory(document) => {
                                                            document.lines.get(index).cloned()
                                                        }
                                                        // 分页日志必须在入口提前走专用窗口化渲染。
                                                        // 这里作为防御性兜底直接跳过，避免未来调用路径变化后在普通 render processor 中同步读文件。
                                                        LogTabDocument::Paged(_) => None,
                                                    }?;
                                                    let precomputed = match document.as_ref() {
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
                                                    let highlight_mode = match document.as_ref() {
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
                                                    if let Some((range, style)) =
                                                        Self::log_search_match_highlight_for_line(
                                                            highlighted_search_match.as_ref(),
                                                            index,
                                                            &line,
                                                            palette,
                                                        )
                                                    {
                                                        // 搜索命中片段是定位反馈，只给关键字本身增加背景；后续选区高亮仍可覆盖它。
                                                        line_highlights = gpui::combine_highlights(
                                                            line_highlights,
                                                            [(range, style)],
                                                        )
                                                        .collect();
                                                    }
                                                    if let Some(selection) = &text_selection
                                                        && let Some(range) =
                                                            Self::selected_byte_range_for_line(
                                                                selection, index, &line,
                                                            )
                                                    {
                                                        // 搜索关键字背景和鼠标选区背景重叠时，选区必须优先显示；
                                                        // 否则用户拖选到关键字时会误以为关键字没有被选中。
                                                        line_highlights =
                                                            Self::combine_log_highlights_with_selection(
                                                                line_highlights,
                                                                range,
                                                            );
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
                                                        marked_lines.contains(&index),
                                                    ))
                                                })
                                                .collect::<Vec<_>>()
                                        } else {
                                            Vec::new()
                                        };

                                        lines
                                            .into_iter()
                                            .map(
                                                |(
                                                    index,
                                                    line,
                                                    display_line,
                                                    highlights,
                                                    search_highlighted,
                                                    marked,
                                                )| {
                                                    let horizontal_line_number_offset =
                                                        -row_scroll_handle
                                                            .0
                                                            .borrow()
                                                            .base_handle
                                                            .offset()
                                                            .x;
                                                    Self::render_log_line(
                                                        LogLineRenderData {
                                                            tab_id,
                                                            line_index: index,
                                                            line,
                                                            display_line,
                                                            highlights,
                                                            line_number_width,
                                                            font_size: view
                                                                .settings
                                                                .log_viewer_font_size,
                                                            horizontal_line_number_offset,
                                                            horizontal_content_offset: px(0.0),
                                                            search_highlighted,
                                                            marked,
                                                            suppress_hover: view
                                                                .search
                                                                .search_results_resize_drag
                                                                .is_some()
                                                                || view
                                                                    .log
                                                                    .log_scrollbar_drag
                                                                    .is_some()
                                                                || view
                                                                    .log
                                                                    .log_minimap_drag
                                                                    .is_some(),
                                                            palette: view.palette(),
                                                        },
                                                        context,
                                                    )
                                                },
                                            )
                                            .collect::<Vec<_>>()
                                    },
                                ),
                            )
                            .with_width_from_item(Some(horizontal_measure_line_index))
                            .with_horizontal_sizing_behavior(
                                ListHorizontalSizingBehavior::Unconstrained,
                            )
                            .size_full()
                            .track_scroll(scroll_handle.clone()),
                        )
                        .child(self.render_log_horizontal_scrollbar_for_tab(tab, context)),
                )
                .child(self.render_log_minimap(tab, document, context))
                .child(self.render_log_vertical_scrollbar_for_tab(tab, context)),
        )
    }

    /// 渲染分页日志正文查看器。
    ///
    /// 业务意图：
    /// - 分页日志可能有数千万行，完整 `uniform_list` 会把行号乘以固定行高后交给 `f32` 像素坐标，滚到深处会出现行间距和重叠。
    /// - 这里只渲染当前视口附近的一小段真实行号，纵向滚动位置由 `PagedLogScrollState` 的 `f64` 逻辑坐标保存。
    /// - 行号、选区、高亮和右键菜单仍复用普通日志行渲染逻辑，保证两种模式的视觉行为一致。
    /// - 文件读取和解码必须在后台任务中完成，render 只读取 tab 上已经准备好的可见行缓存，避免滚动过程中阻塞 UI 线程。
    fn render_paged_log_document_viewer(
        &self,
        tab: &OpenLogTab,
        document: &log_document::PagedLogDocument,
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
        let highlighted_search_match = tab.highlighted_search_match.clone();
        let marked_lines = tab.marked_lines.clone();
        let visible_request = PagedLogVisibleLinesRequest {
            start_line: first_line_index,
            max_lines: visible_rows,
        };
        let (visible_lines, should_request_visible_lines) = {
            let mut visible_state = tab.paged_visible_lines.borrow_mut();
            if visible_state.ready_request == Some(visible_request) {
                (visible_state.ready_lines.clone(), false)
            } else {
                let should_request = visible_request.max_lines > 0
                    && visible_state.pending_request != Some(visible_request);
                if should_request {
                    visible_state.pending_request = Some(visible_request);
                    visible_state.error_message = None;
                }
                (Vec::new(), should_request)
            }
        };
        if should_request_visible_lines {
            self.spawn_paged_visible_lines_read(tab_id, document.clone(), visible_request, context);
        }

        let rows = visible_lines
            .into_iter()
            .filter_map(|paged_line| {
                let line_index = paged_line.line_number;
                if line_index >= line_count {
                    return None;
                }
                // 分页行保留原始偏移和替换字符标记，供后续错误提示或定位扩展使用；当前渲染只需要行号和文本。
                let _ = (paged_line.byte_offset, paged_line.had_replacements);
                let line = paged_line.text;
                let mut line_highlights =
                    highlight_line(document.highlight_mode, &line, None, syntax_theme);
                if let Some((range, style)) = Self::log_search_match_highlight_for_line(
                    highlighted_search_match.as_ref(),
                    line_index,
                    &line,
                    palette,
                ) {
                    // 分页模式和内存模式保持一致：行容器负责整行定位背景，文本高亮只覆盖命中关键字片段。
                    line_highlights =
                        gpui::combine_highlights(line_highlights, [(range, style)]).collect();
                }
                if let Some(selection) = &text_selection
                    && let Some(range) =
                        Self::selected_byte_range_for_line(selection, line_index, &line)
                {
                    // 分页模式和内存模式保持相同的背景优先级：鼠标选区覆盖搜索关键字背景。
                    line_highlights =
                        Self::combine_log_highlights_with_selection(line_highlights, range);
                }
                let expanded_line = Self::expanded_log_line_for_display(&line);
                let display_highlights =
                    Self::map_log_highlights_to_display(line_highlights, &expanded_line);
                let row_offset = line_index.saturating_sub(first_line_index);
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
                                font_size: self.settings.log_viewer_font_size,
                                horizontal_line_number_offset: px(0.0),
                                horizontal_content_offset,
                                search_highlighted: highlighted_search_line == Some(line_index),
                                marked: marked_lines.contains(&line_index),
                                suppress_hover: self.search.search_results_resize_drag.is_some()
                                    || self.log.log_scrollbar_drag.is_some()
                                    || self.log.log_minimap_drag.is_some(),
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
                .child(
                    div()
                        .id("log-viewer-content")
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .h_full()
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
                            context.listener(
                                move |view, event: &MouseDownEvent, _window, context| {
                                    view.note_keyboard_scroll_region(
                                        KeyboardScrollRegion::LogContent,
                                    );
                                    view.open_log_viewer_context_menu(
                                        tab_id,
                                        f32::from(event.position.x),
                                        f32::from(event.position.y),
                                        context,
                                    );
                                    context.stop_propagation();
                                },
                            ),
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
                        .child(self.render_log_horizontal_scrollbar_for_tab(tab, context)),
                )
                .child(self.render_log_minimap(
                    tab,
                    &LogTabDocument::Paged(document.clone()),
                    context,
                ))
                .child(self.render_log_vertical_scrollbar_for_tab(tab, context)),
        )
    }

    /// 在后台读取分页日志当前视口所需的可见行。
    ///
    /// 业务意图：
    /// - GPUI render 可能在滚轮、拖动滚动条和窗口 resize 时高频触发，不能在其中同步执行本地文件 I/O。
    /// - 后台任务先调用 `read_visible_lines` 做连续区间读取；若区间读取失败，再在后台逐行兜底，保留旧实现“局部可读仍展示”的容错特性。
    ///
    /// 边界条件：
    /// - 请求返回时 tab 可能已经关闭、重新加载或滚动到其它位置，因此合并前必须再次校验 tab ID 和 `pending_request`。
    /// - 后台任务只克隆分页文档句柄，不持有 `RefCell` 或 GPUI 元素，避免跨线程访问 UI 状态。
    fn spawn_paged_visible_lines_read(
        &self,
        tab_id: usize,
        document: log_document::PagedLogDocument,
        request: PagedLogVisibleLinesRequest,
        context: &mut Context<Self>,
    ) {
        context
            .spawn(async move |view, app| {
                let result = app
                    .background_executor()
                    .spawn(
                        async move { Self::read_paged_visible_lines_for_render(document, request) },
                    )
                    .await;

                view.update(app, |view, context| {
                    view.apply_paged_visible_lines_result(tab_id, request, result);
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 执行分页日志可见行读取任务。
    ///
    /// 业务意图：
    /// - 将文件读取封装成纯后台函数，方便 render 侧只负责调度和消费结果。
    /// - 主路径批量读取连续可见区间，兜底路径逐行读取只在异常情况下触发，并且同样运行在后台线程。
    ///
    /// 边界条件：
    /// - 视口容量为 0 或起始行越界时返回空集合，覆盖窗口高度极小、文件为空和滚动状态被裁剪的情况。
    /// - 如果批量读取失败且逐行兜底也没有读到任何行，返回原始错误，便于后续 UI 提示定位真实文件问题。
    fn read_paged_visible_lines_for_render(
        document: log_document::PagedLogDocument,
        request: PagedLogVisibleLinesRequest,
    ) -> Result<Vec<log_document::PagedLine>, LogContentError> {
        match document.read_visible_lines(request.start_line, request.max_lines) {
            Ok(lines) => Ok(lines),
            Err(error) => {
                let line_count = document.line_count();
                let mut fallback_lines = Vec::new();
                for row_offset in 0..request.max_lines {
                    let Some(line_index) = request.start_line.checked_add(row_offset) else {
                        break;
                    };
                    if line_index >= line_count {
                        break;
                    }
                    if let Some(line) = document.read_line(line_index)? {
                        fallback_lines.push(line);
                    }
                }
                if fallback_lines.is_empty() {
                    Err(error)
                } else {
                    Ok(fallback_lines)
                }
            }
        }
    }

    /// 合并分页日志可见行后台读取结果。
    ///
    /// 业务意图：
    /// - 后台读取完成后只更新发起该请求的 tab，render 下一帧再消费 `ready_lines`。
    /// - 用 `pending_request` 抵消乱序返回：用户快速拖动滚动条时，旧区域结果不能覆盖新区域的可见行。
    ///
    /// 边界条件：
    /// - tab 已关闭、tab 已切换为内存文档或请求已过期时直接忽略。
    /// - 读取失败只清空当前可见行缓存并记录错误，不把整个 tab 切到失败状态，避免瞬时 I/O 错误破坏已打开文档。
    fn apply_paged_visible_lines_result(
        &mut self,
        tab_id: usize,
        request: PagedLogVisibleLinesRequest,
        result: Result<Vec<log_document::PagedLine>, LogContentError>,
    ) {
        let Some(tab) = self.log.open_tabs.iter().find(|tab| tab.id == tab_id) else {
            return;
        };
        if !matches!(
            &tab.state,
            LogTabState::Ready { document }
                if matches!(document.as_ref(), LogTabDocument::Paged(_))
        ) {
            return;
        }

        let mut visible_state = tab.paged_visible_lines.borrow_mut();
        if visible_state.pending_request != Some(request) {
            return;
        }
        visible_state.pending_request = None;
        match result {
            Ok(lines) => {
                visible_state.ready_request = Some(request);
                visible_state.ready_lines = lines;
                visible_state.error_message = None;
            }
            Err(error) => {
                visible_state.ready_request = None;
                visible_state.ready_lines.clear();
                visible_state.error_message = Some(error.to_string());
            }
        }
    }
}
