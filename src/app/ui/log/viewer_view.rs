// 日志 tab、编码选择器、日志正文查看器和正文右键菜单方法。
//
// 业务意图：
// - 这些方法仍属于 `MainView` 的实现块，但物理上从 `app.rs` 拆出，降低根文件体积。
// - 分页日志在本文件内使用窗口化渲染，避免超大行号对应的绝对像素坐标触发 `f32` 精度问题。
//
// 边界条件：
// - 该文件作为 `app` 的子模块自行声明 `impl MainView`，跨模块调用通过 `pub(in crate::app)` 方法显式暴露。

use super::*;

/// 日志正文搜索关键字片段在亮色主题下的暖橙色背景。
///
/// 业务意图：
/// - 搜索定位统一同时展示整行黄色背景和关键字片段背景；整行黄色负责定位行，关键字暖橙色负责定位列。
/// - 使用固定色值而不是主题搜索色，避免关键字和整行同色后在长日志中难以区分命中位置，同时避免高饱和橘红色长时间阅读刺眼。
pub(in crate::app) const LOG_SEARCH_KEYWORD_HIGHLIGHT: u32 = 0xf2a65a;

/// 日志正文搜索关键字片段在暗色主题下的深蓝背景。
///
/// 业务意图：
/// - 暗色日志正文常使用绿色、蓝色等语法前景色；如果继续使用亮色黄橙底，语法色和背景会互相干扰。
/// - 这里使用深蓝底色并配合浅色前景，让搜索关键字在暗色日志和整行搜索定位背景上都能清晰区分。
pub(in crate::app) const LOG_SEARCH_KEYWORD_HIGHLIGHT_DARK: u32 = 0x1d4ed8;

/// 暗色主题搜索关键字片段上的文本颜色。
///
/// 业务意图：
/// - 搜索命中片段优先表达“当前命中的文字内容”，因此暗色主题下显式覆盖语法高亮前景，避免绿色日志文本落在搜索背景上发糊。
pub(in crate::app) const LOG_SEARCH_KEYWORD_TEXT_DARK: u32 = 0xf8fafc;

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
        palette: AppThemePalette,
    ) -> Option<(Range<usize>, gpui::HighlightStyle)> {
        let highlight = highlight?;
        if highlight.line_index != line_index {
            return None;
        }
        let range = Self::clamp_search_text_range(line, highlight.match_range.clone());
        if range.start >= range.end {
            return None;
        }
        let is_dark = Self::log_palette_is_dark(palette);
        Some((
            range,
            gpui::HighlightStyle {
                color: is_dark.then(|| rgb(LOG_SEARCH_KEYWORD_TEXT_DARK).into()),
                background_color: Some(
                    rgb(if is_dark {
                        LOG_SEARCH_KEYWORD_HIGHLIGHT_DARK
                    } else {
                        LOG_SEARCH_KEYWORD_HIGHLIGHT
                    })
                    .into(),
                ),
                font_weight: Some(FontWeight::SEMIBOLD),
                ..Default::default()
            },
        ))
    }

    /// 合并日志行高亮和搜索关键字高亮。
    ///
    /// 业务意图：
    /// - 搜索关键字是当前定位的最重要反馈，暗色主题下需要让关键字文字颜色稳定可读。
    /// - GPUI 的高亮合并会把两个前景色做混合，而不是按后者覆盖；如果直接合并，绿色日志语法色会把搜索关键字的浅色前景混成低对比颜色。
    ///
    /// 边界条件：
    /// - 亮色主题搜索样式没有显式前景色，仍保留原有语法高亮文字颜色。
    /// - 暗色主题搜索样式带前景色时，只清理命中范围内已有高亮的前景色，命中范围外的日志语法高亮不受影响。
    pub(in crate::app) fn combine_log_highlights_with_search_match(
        highlights: Vec<(Range<usize>, gpui::HighlightStyle)>,
        search_range: Range<usize>,
        search_style: gpui::HighlightStyle,
    ) -> Vec<(Range<usize>, gpui::HighlightStyle)> {
        if search_range.start >= search_range.end {
            return highlights;
        }

        let mut adjusted_highlights = Vec::with_capacity(highlights.len().saturating_add(2));
        for (range, style) in highlights {
            if search_style.color.is_none()
                || range.end <= search_range.start
                || range.start >= search_range.end
            {
                adjusted_highlights.push((range, style));
                continue;
            }

            if range.start < search_range.start {
                let before_end = search_range.start.min(range.end);
                if range.start < before_end {
                    adjusted_highlights.push((range.start..before_end, style));
                }
            }

            let overlap_start = range.start.max(search_range.start);
            let overlap_end = range.end.min(search_range.end);
            if overlap_start < overlap_end {
                let mut readable_style = style;
                readable_style.color = None;
                adjusted_highlights.push((overlap_start..overlap_end, readable_style));
            }

            if search_range.end < range.end {
                let after_start = search_range.end.max(range.start);
                if after_start < range.end {
                    adjusted_highlights.push((after_start..range.end, style));
                }
            }
        }

        gpui::combine_highlights(adjusted_highlights, [(search_range, search_style)]).collect()
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
        palette: AppThemePalette,
    ) -> Vec<(Range<usize>, gpui::HighlightStyle)> {
        if selection_range.start >= selection_range.end {
            return highlights;
        }

        let selection_style = Self::log_text_selection_highlight_style(palette);
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
                if let Some(color) = selection_style.color {
                    // 暗色主题的选区会显式覆盖前景色，避免搜索关键字或语法高亮的绿色文字落在蓝色选区上仍然看不清。
                    foreground_style.color = Some(color);
                }
                adjusted_highlights.push((overlap_start..overlap_end, foreground_style));
            }

            if selection_range.end < range.end {
                let after_start = selection_range.end.max(range.start);
                if after_start < range.end {
                    adjusted_highlights.push((after_start..range.end, style));
                }
            }
        }

        gpui::combine_highlights(adjusted_highlights, [(selection_range, selection_style)])
            .collect()
    }

    /// 判断当前日志调色板是否属于暗色主题。
    ///
    /// 业务意图：
    /// - 日志高亮 helper 只接收调色板，不直接依赖窗口主题状态；通过集中比较主题背景色，保持渲染路径和测试路径都能得到一致结果。
    /// - 如果未来暗色主题调色板调整，只需要继续保证 `AppThemePalette::for_theme` 是唯一来源，日志高亮就会跟随主题分支。
    fn log_palette_is_dark(palette: AppThemePalette) -> bool {
        palette.background == AppThemePalette::for_theme(EffectiveTheme::Dark).background
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
                                                        line_highlights =
                                                            Self::combine_log_highlights_with_search_match(
                                                            line_highlights,
                                                            range,
                                                            style,
                                                        );
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
                                                                palette,
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
    /// - 按用户要求，分页可见行回到 render 路径同步读取；为降低回退后的成本，读取仍走连续区间批量读取和分页解码缓存。
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
        let visible_lines =
            Self::read_paged_visible_lines_for_render(document, first_line_index, visible_rows)
                .unwrap_or_default();

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
                    line_highlights = Self::combine_log_highlights_with_search_match(
                        line_highlights,
                        range,
                        style,
                    );
                }
                if let Some(selection) = &text_selection
                    && let Some(range) =
                        Self::selected_byte_range_for_line(selection, line_index, &line)
                {
                    // 分页模式和内存模式保持相同的背景优先级：鼠标选区覆盖搜索关键字背景。
                    line_highlights = Self::combine_log_highlights_with_selection(
                        line_highlights,
                        range,
                        palette,
                    );
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

    /// 读取分页日志当前视口所需的可见行。
    ///
    /// 业务意图：
    /// - 用户要求取消分页可见行异步加载，以避免滚动或拖动滚动条时出现新数据未返回导致的白屏闪烁。
    /// - 主路径仍调用 `read_visible_lines` 批量读取连续可见区间；该方法内部复用解码缓存，避免恢复到逐行 `seek + read` 的最差路径。
    ///
    /// 边界条件：
    /// - 视口容量为 0 或起始行越界时返回空集合，覆盖窗口高度极小、文件为空和滚动状态被裁剪的情况。
    /// - 如果批量读取失败且逐行兜底也没有读到任何行，返回原始错误，便于后续 UI 提示定位真实文件问题。
    fn read_paged_visible_lines_for_render(
        document: &log_document::PagedLogDocument,
        start_line: usize,
        max_lines: usize,
    ) -> Result<Vec<log_document::PagedLine>, LogContentError> {
        match document.read_visible_lines(start_line, max_lines) {
            Ok(lines) => Ok(lines),
            Err(error) => {
                let line_count = document.line_count();
                let mut fallback_lines = Vec::new();
                for row_offset in 0..max_lines {
                    let Some(line_index) = start_line.checked_add(row_offset) else {
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
}
