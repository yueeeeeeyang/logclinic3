// AI 对话自绘多行输入元素。
//
// 业务意图：
// - GPUI 当前没有项目所需的多行输入控件，因此 AI 对话输入区继续使用自绘文本元素支持中文 IME、选择、粘贴和光标绘制。
// - 本文件只迁移原有输入元素实现，不改变输入行为、占位文案、行高或候选窗口定位逻辑。

use super::*;

/// AI 对话多行输入元素。
///
/// 业务意图：
/// - GPUI 0.2.2 没有内建多行文本输入控件；AI 输入必须支持中文 IME、粘贴、选择和光标，因此复用项目内自绘文本区方案。
/// - 第一版不做自动换行，长提示词横向裁切但完整文本仍保存在状态中并发送给模型。
pub(in crate::app) struct AiChatInputElement {
    /// 主视图实体，用于读取和写回 AI 输入状态。
    pub(in crate::app) view: Entity<MainView>,
    /// 输入框焦点句柄。
    pub(in crate::app) focus_handle: gpui::FocusHandle,
    /// 输入为空时显示的占位文案。
    pub(in crate::app) placeholder: &'static str,
    /// 当前主题调色板。
    pub(in crate::app) palette: AppThemePalette,
}

impl IntoElement for AiChatInputElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for AiChatInputElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<AiChatInputPrepaint>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        context: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let line_count = self.view.read(context).ai_chat_input_visual_line_count();
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height =
            px((line_count as f32 * AI_CHAT_INPUT_LINE_HEIGHT).max(AI_CHAT_INPUT_LINE_HEIGHT))
                .into();
        (window.request_layout(style, [], context), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        context: &mut App,
    ) -> Self::PrepaintState {
        let (text, selection_range, marked_range, cursor_visible_by_activity) = {
            let view = self.view.read(context);
            let (text, selection_range, marked_range) = view.ai_chat_input_text_snapshot();
            (
                text,
                selection_range,
                marked_range,
                view.search_text_cursor_visible(),
            )
        };
        let focused = self.focus_handle.is_focused(window);
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = px(AI_CHAT_INPUT_LINE_HEIGHT);
        let line_ranges = MainView::thread_analysis_filter_line_ranges(&text);
        let display_ranges = if text.is_empty() {
            std::iter::once(0..0).collect::<Vec<_>>()
        } else {
            line_ranges
        };

        let mut lines = Vec::new();
        let mut selections = Vec::new();
        let mut cursor = None;
        let selection_range = MainView::clamp_search_text_range(&text, selection_range);
        let has_selection = focused && selection_range.start < selection_range.end;
        let cursor_index = selection_range.end;

        for (line_index, byte_range) in display_ranges.into_iter().enumerate() {
            let is_placeholder = text.is_empty();
            let display_text = if is_placeholder {
                SharedString::from(self.placeholder)
            } else {
                SharedString::from(text[byte_range.clone()].to_string())
            };
            let text_color = if is_placeholder {
                rgb(self.palette.muted_text).into()
            } else {
                style.color
            };
            let base_run = TextRun {
                len: display_text.len(),
                font: style.font(),
                color: text_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let runs = if !is_placeholder {
                if let Some(marked_range) = marked_range.clone() {
                    let local_marked_start = marked_range
                        .start
                        .saturating_sub(byte_range.start)
                        .min(byte_range.len());
                    let local_marked_end = marked_range
                        .end
                        .saturating_sub(byte_range.start)
                        .min(byte_range.len());
                    vec![
                        TextRun {
                            len: local_marked_start,
                            ..base_run.clone()
                        },
                        TextRun {
                            len: local_marked_end.saturating_sub(local_marked_start),
                            underline: Some(UnderlineStyle {
                                color: Some(base_run.color),
                                thickness: px(1.0),
                                wavy: false,
                            }),
                            ..base_run.clone()
                        },
                        TextRun {
                            len: display_text.len().saturating_sub(local_marked_end),
                            ..base_run
                        },
                    ]
                    .into_iter()
                    .filter(|run| run.len > 0)
                    .collect()
                } else {
                    vec![base_run]
                }
            } else {
                vec![base_run]
            };
            let line = window
                .text_system()
                .shape_line(display_text, font_size, &runs, None);
            let line_top = bounds.top() + px(line_index as f32 * AI_CHAT_INPUT_LINE_HEIGHT);
            let line_bounds = Bounds::new(
                point(bounds.left(), line_top),
                size(bounds.right() - bounds.left(), line_height),
            );

            if has_selection && !is_placeholder {
                let start = selection_range
                    .start
                    .max(byte_range.start)
                    .min(byte_range.end);
                let end = selection_range
                    .end
                    .max(byte_range.start)
                    .min(byte_range.end);
                if start < end {
                    let mut selection_color = rgb(self.palette.accent);
                    selection_color.a = 0.32;
                    selections.push(fill(
                        Bounds::from_corners(
                            point(
                                line_bounds.left() + line.x_for_index(start - byte_range.start),
                                line_bounds.top(),
                            ),
                            point(
                                line_bounds.left() + line.x_for_index(end - byte_range.start),
                                line_bounds.bottom(),
                            ),
                        ),
                        selection_color,
                    ));
                }
            }

            if focused
                && !has_selection
                && cursor.is_none()
                && cursor_visible_by_activity
                && cursor_index >= byte_range.start
                && cursor_index <= byte_range.end
            {
                cursor = Some(fill(
                    Bounds::new(
                        point(
                            line_bounds.left() + line.x_for_index(cursor_index - byte_range.start),
                            line_bounds.top(),
                        ),
                        size(px(1.5), line_bounds.bottom() - line_bounds.top()),
                    ),
                    rgb(self.palette.accent),
                ));
            }

            lines.push(AiChatInputPaintLine {
                byte_range,
                bounds: line_bounds,
                line,
            });
        }

        Some(AiChatInputPrepaint {
            lines,
            selections,
            cursor,
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        context: &mut App,
    ) {
        window.handle_input(
            &self.focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            context,
        );
        let Some(prepaint) = prepaint.take() else {
            return;
        };
        for selection in prepaint.selections {
            window.paint_quad(selection);
        }

        let mut layouts = Vec::new();
        for paint_line in prepaint.lines {
            paint_line
                .line
                .paint(
                    paint_line.bounds.origin,
                    paint_line.bounds.bottom() - paint_line.bounds.top(),
                    window,
                    context,
                )
                .ok();
            layouts.push(AiChatInputLineLayout {
                byte_range: paint_line.byte_range,
                line: paint_line.line,
                bounds: paint_line.bounds,
            });
        }
        if let Some(cursor) = prepaint.cursor {
            window.paint_quad(cursor);
        }
        if self.focus_handle.is_focused(window) {
            window.request_animation_frame();
        }
        self.view.update(context, |view, _context| {
            view.store_ai_chat_input_text_layouts(layouts, bounds);
        });
    }
}

/// AI 对话消息正文可选择文本元素。
///
/// 业务意图：
/// - AI 气泡正文使用 Markdown 和虚拟列表渲染，普通 `StyledText` 只能展示，不能提供系统选区。
/// - 该元素在保留自动换行、Markdown 内联高亮和代码高亮的同时，自绘选区背景并回写布局快照，供鼠标拖选和复制快捷键使用。
///
/// 边界条件：
/// - 文本下标全部使用 UTF-8 字节范围；绘制和复制前都必须夹到字符边界，避免中文、emoji 或代码注释被截断成非法字符串。
pub(in crate::app) struct AiChatSelectableTextElement {
    /// 主视图实体，用于读取当前选区并保存最近绘制布局。
    pub(in crate::app) view: Entity<MainView>,
    /// 当前文本段标识。
    pub(in crate::app) segment: AiChatMessageTextSegmentKey,
    /// 当前文本段内容。
    pub(in crate::app) text: String,
    /// Markdown 或代码高亮范围。
    pub(in crate::app) highlights: Vec<(Range<usize>, HighlightStyle)>,
    /// 当前主题调色板。
    pub(in crate::app) palette: AppThemePalette,
}

impl IntoElement for AiChatSelectableTextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for AiChatSelectableTextElement {
    type RequestLayoutState = ();
    type PrepaintState = AiChatSelectableTextPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        _context: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let text = self.text.clone();
        let highlights = self.highlights.clone();
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        let layout_id =
            window.request_measured_layout(style, move |known, available, window, context| {
                let wrap_width = known.width.or(match available.width {
                    gpui::AvailableSpace::Definite(width) => Some(width),
                    _ => None,
                });
                let (lines, line_height) =
                    shape_ai_chat_selectable_text(&text, &highlights, wrap_width, window, context);
                ai_chat_selectable_text_size(&lines, line_height)
            });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        context: &mut App,
    ) -> Self::PrepaintState {
        let (lines, line_height) = shape_ai_chat_selectable_text(
            &self.text,
            &self.highlights,
            Some(bounds.size.width),
            window,
            context,
        );
        let selection_range = self
            .view
            .read(context)
            .ai_chat_message_text_selection_for_segment(&self.segment);
        let selections = selection_range
            .map(|range| {
                ai_chat_selectable_text_selection_quads(
                    &lines,
                    line_height,
                    bounds,
                    MainView::clamp_search_text_range(&self.text, range),
                    self.palette,
                )
            })
            .unwrap_or_default();
        AiChatSelectableTextPrepaint {
            lines,
            line_height,
            selections,
            bounds,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        context: &mut App,
    ) {
        for selection in prepaint.selections.drain(..) {
            window.paint_quad(selection);
        }

        let mut line_origin = prepaint.bounds.origin;
        for line in &prepaint.lines {
            line.paint(
                line_origin,
                prepaint.line_height,
                Default::default(),
                Some(prepaint.bounds),
                window,
                context,
            )
            .ok();
            line_origin.y += line.size(prepaint.line_height).height;
        }

        self.view.update(context, |view, _context| {
            view.store_ai_chat_message_text_layout(
                self.segment.clone(),
                AiChatMessageTextLayoutSnapshot {
                    text: self.text.clone(),
                    lines: prepaint.lines.clone(),
                    line_height: prepaint.line_height,
                    bounds: prepaint.bounds,
                },
            );
        });
    }
}

/// AI 消息可选文本元素的绘制前状态。
pub(in crate::app) struct AiChatSelectableTextPrepaint {
    /// 已根据当前可用宽度自动换行的文本行。
    lines: Vec<gpui::WrappedLine>,
    /// 当前文本行高。
    line_height: Pixels,
    /// 选区背景矩形。
    selections: Vec<PaintQuad>,
    /// 当前文本元素边界。
    bounds: Bounds<Pixels>,
}

/// 根据当前窗口文本样式和 Markdown 高亮排版可选文本。
fn shape_ai_chat_selectable_text(
    text: &str,
    highlights: &[(Range<usize>, HighlightStyle)],
    wrap_width: Option<Pixels>,
    window: &mut Window,
    _context: &mut App,
) -> (Vec<gpui::WrappedLine>, Pixels) {
    let text_style = window.text_style();
    let font_size = text_style.font_size.to_pixels(window.rem_size());
    let line_height = text_style
        .line_height
        .to_pixels(font_size.into(), window.rem_size());
    let runs = ai_chat_selectable_text_runs(text, &text_style, highlights);
    let lines = window
        .text_system()
        .shape_text(
            SharedString::from(text.to_string()),
            font_size,
            &runs,
            wrap_width,
            None,
        )
        .unwrap_or_default()
        .into_iter()
        .collect::<Vec<_>>();
    (lines, line_height)
}

/// 计算自动换行文本的布局尺寸。
fn ai_chat_selectable_text_size(
    lines: &[gpui::WrappedLine],
    line_height: Pixels,
) -> gpui::Size<Pixels> {
    let mut text_size: gpui::Size<Pixels> = gpui::Size::default();
    for line in lines {
        let line_size = line.size(line_height);
        text_size.height += line_size.height;
        text_size.width = text_size.width.max(line_size.width).ceil();
    }
    text_size
}

/// 将 Markdown 高亮转换成 GPUI 文本 run。
///
/// 边界条件：
/// - 高亮范围来自 Markdown 展平或代码高亮，正常情况下已经有序且不重叠；这里仍会夹到文本长度，防止流式半截内容导致调试断言或绘制失败。
fn ai_chat_selectable_text_runs(
    text: &str,
    default_style: &gpui::TextStyle,
    highlights: &[(Range<usize>, HighlightStyle)],
) -> Vec<TextRun> {
    let mut runs = Vec::new();
    let mut cursor = 0usize;
    for (range, highlight) in highlights {
        let range = MainView::clamp_search_text_range(text, range.clone());
        if range.start > cursor {
            runs.push(default_style.clone().to_run(range.start - cursor));
        }
        if range.start < range.end {
            runs.push(
                default_style
                    .clone()
                    .highlight(highlight.clone())
                    .to_run(range.end - range.start),
            );
        }
        cursor = cursor.max(range.end);
    }
    if cursor < text.len() {
        runs.push(default_style.to_run(text.len() - cursor));
    }
    if runs.is_empty() {
        runs.push(default_style.to_run(0));
    }
    runs
}

/// 根据当前选区生成跨软换行的选区背景。
fn ai_chat_selectable_text_selection_quads(
    lines: &[gpui::WrappedLine],
    line_height: Pixels,
    bounds: Bounds<Pixels>,
    range: Range<usize>,
    palette: AppThemePalette,
) -> Vec<PaintQuad> {
    if range.start >= range.end {
        return Vec::new();
    }
    let mut selection_color = rgb(palette.accent);
    selection_color.a = 0.28;
    let mut quads = Vec::new();
    let mut line_origin = bounds.origin;
    let mut line_start = 0usize;
    for line in lines {
        let visual_ranges = ai_chat_wrapped_line_visual_ranges(line);
        for visual_range in visual_ranges {
            let global_start = line_start + visual_range.start;
            let global_end = line_start + visual_range.end;
            let selection_start = range.start.max(global_start).min(global_end);
            let selection_end = range.end.min(global_end).max(selection_start);
            if selection_start >= selection_end {
                continue;
            }
            let local_start = selection_start.saturating_sub(line_start);
            let local_end = selection_end.saturating_sub(line_start);
            if let (Some(start_position), Some(end_position)) = (
                line.position_for_index(local_start, line_height),
                line.position_for_index(local_end, line_height),
            ) {
                let top = line_origin.y + start_position.y;
                quads.push(fill(
                    Bounds::from_corners(
                        point(line_origin.x + start_position.x, top),
                        point(line_origin.x + end_position.x, top + line_height),
                    ),
                    selection_color,
                ));
            }
        }
        line_origin.y += line.size(line_height).height;
        line_start += line.len() + 1;
    }
    quads
}

/// 返回一个硬换行内按软换行拆分后的局部范围。
pub(in crate::app) fn ai_chat_wrapped_line_visual_ranges(
    line: &gpui::WrappedLine,
) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0usize;
    for boundary in line.wrap_boundaries() {
        let end = line.runs()[boundary.run_ix].glyphs[boundary.glyph_ix].index;
        ranges.push(start..end);
        start = end;
    }
    ranges.push(start..line.len());
    ranges
}

/// 根据最近绘制的消息文本布局把窗口坐标换算成 UTF-8 字节下标。
pub(in crate::app) fn ai_chat_selectable_text_index_for_position(
    snapshot: &AiChatMessageTextLayoutSnapshot,
    position: Point<Pixels>,
) -> usize {
    if position.y < snapshot.bounds.top() {
        return 0;
    }
    let mut line_origin = snapshot.bounds.origin;
    let mut line_start = 0usize;
    for line in &snapshot.lines {
        let line_bottom = line_origin.y + line.size(snapshot.line_height).height;
        if position.y > line_bottom {
            line_origin.y = line_bottom;
            line_start += line.len() + 1;
            continue;
        }
        let position_within_line = position - line_origin;
        let local_index =
            match line.closest_index_for_position(position_within_line, snapshot.line_height) {
                Ok(index) | Err(index) => index,
            };
        return MainView::clamp_search_text_range(
            &snapshot.text,
            line_start + local_index..line_start + local_index,
        )
        .start;
    }
    snapshot.text.len()
}
