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
            vec![0..0]
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
