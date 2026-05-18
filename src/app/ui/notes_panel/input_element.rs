// 笔记标题自绘输入元素。
//
// 业务意图：
// - 正文编辑已迁移到项目内富文本编辑器；标题和重命名弹窗仍使用项目内单行输入元素，控制改动面。
// - 本元素只负责标题绘制和平台输入协议接入，真实编辑行为由 `notes_panel/actions.rs` 维护。

use super::*;

/// 笔记标题单行输入元素。
///
/// 业务意图：
/// - 笔记标题和正文一起保存，但标题需要单行编辑和 IME 支持，因此使用独立元素接入平台文本输入协议。
pub(in crate::app) struct NoteTitleElement {
    /// 主视图实体，用于读取和写回标题草稿状态。
    pub(in crate::app) view: Entity<MainView>,
    /// 标题输入焦点句柄。
    pub(in crate::app) focus_handle: gpui::FocusHandle,
    /// 当前主题调色板。
    pub(in crate::app) palette: AppThemePalette,
    /// 空标题时展示的占位文案；编辑笔记和重命名弹窗需要不同提示。
    pub(in crate::app) placeholder: &'static str,
}

impl IntoElement for NoteTitleElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for NoteTitleElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<(
        ShapedLine,
        Vec<PaintQuad>,
        Option<PaintQuad>,
        Bounds<Pixels>,
    )>;

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
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = px(24.0).into();
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
        let (text, selection_range, marked_range, cursor_visible) = {
            let view = self.view.read(context);
            (
                view.notes.editor_title.clone(),
                MainView::clamp_search_text_range(
                    &view.notes.editor_title,
                    view.notes.title_selection_range.clone(),
                ),
                view.notes.title_marked_range.clone(),
                view.search_text_cursor_visible(),
            )
        };
        let focused = self.focus_handle.is_focused(window);
        let style = window.text_style();
        let is_placeholder = text.is_empty();
        let display_text = if is_placeholder {
            SharedString::from(self.placeholder)
        } else {
            SharedString::from(text.clone())
        };
        let base_run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: if is_placeholder {
                rgb(self.palette.muted_text).into()
            } else {
                style.color
            },
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked_range) = marked_range {
            let start = marked_range.start.min(display_text.len());
            let end = marked_range.end.min(display_text.len());
            vec![
                TextRun {
                    len: start,
                    ..base_run.clone()
                },
                TextRun {
                    len: end.saturating_sub(start),
                    underline: Some(UnderlineStyle {
                        color: Some(base_run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..base_run.clone()
                },
                TextRun {
                    len: display_text.len().saturating_sub(end),
                    ..base_run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![base_run]
        };
        let line = window
            .text_system()
            .shape_line(display_text, px(16.0), &runs, None);
        let mut selections = Vec::new();
        if focused && selection_range.start < selection_range.end && !is_placeholder {
            let mut selection_color = rgb(self.palette.accent);
            selection_color.a = 0.30;
            selections.push(fill(
                Bounds::from_corners(
                    point(
                        bounds.left() + line.x_for_index(selection_range.start),
                        bounds.top(),
                    ),
                    point(
                        bounds.left() + line.x_for_index(selection_range.end),
                        bounds.bottom(),
                    ),
                ),
                selection_color,
            ));
        }
        let cursor = if focused
            && selection_range.start == selection_range.end
            && cursor_visible
            && !is_placeholder
        {
            Some(fill(
                Bounds::new(
                    point(
                        bounds.left() + line.x_for_index(selection_range.end),
                        bounds.top(),
                    ),
                    size(px(1.5), bounds.bottom() - bounds.top()),
                ),
                rgb(self.palette.accent),
            ))
        } else {
            None
        };
        Some((line, selections, cursor, bounds))
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
        let Some((line, selections, cursor, line_bounds)) = prepaint.take() else {
            return;
        };
        for selection in selections {
            window.paint_quad(selection);
        }
        line.paint(
            line_bounds.origin,
            line_bounds.bottom() - line_bounds.top(),
            window,
            context,
        )
        .ok();
        if let Some(cursor) = cursor {
            window.paint_quad(cursor);
        }
        if self.focus_handle.is_focused(window) {
            window.request_animation_frame();
        }
        self.view.update(context, |view, _context| {
            view.notes.title_last_layout = Some(line);
            view.notes.title_last_bounds = Some(bounds);
        });
    }
}

/// 笔记 AI 多行输入元素。
///
/// 业务意图：
/// - 笔记 AI 侧边栏需要输入生成要求，且必须支持中文 IME、鼠标选择和复制粘贴。
/// - GPUI 当前项目内没有通用多行文本控件，因此这里沿用自绘输入方案，状态仍由 `NotesAiAssistantState` 统一保存。
pub(in crate::app) struct NotesAiInputElement {
    /// 主视图实体，用于读取和写回笔记 AI 输入状态。
    pub(in crate::app) view: Entity<MainView>,
    /// 输入框焦点句柄。
    pub(in crate::app) focus_handle: gpui::FocusHandle,
    /// 输入为空时显示的占位文案。
    pub(in crate::app) placeholder: &'static str,
    /// 当前主题调色板。
    pub(in crate::app) palette: AppThemePalette,
}

impl IntoElement for NotesAiInputElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for NotesAiInputElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<NotesAiInputPrepaint>;

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
        let line_count = self.view.read(context).notes_ai_input_visual_line_count();
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height =
            px((line_count as f32 * NOTES_AI_INPUT_LINE_HEIGHT).max(NOTES_AI_INPUT_LINE_HEIGHT))
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
            let (text, selection_range, marked_range) = view.notes_ai_input_text_snapshot();
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
        let line_height = px(NOTES_AI_INPUT_LINE_HEIGHT);
        let hard_line_ranges = MainView::thread_analysis_filter_line_ranges(&text);
        let measurement_run = TextRun {
            len: 0,
            font: style.font(),
            color: style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let display_ranges = if text.is_empty() {
            std::iter::once(0..0).collect::<Vec<_>>()
        } else {
            let wrap_width = (bounds.right() - bounds.left()).max(px(1.0));
            notes_ai_input_wrapped_ranges(
                &text,
                hard_line_ranges,
                wrap_width,
                font_size,
                &measurement_run,
                window,
            )
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
            let line_top = bounds.top() + px(line_index as f32 * NOTES_AI_INPUT_LINE_HEIGHT);
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

            lines.push(NotesAiInputLineLayout {
                byte_range,
                bounds: line_bounds,
                line,
            });
        }

        Some(NotesAiInputPrepaint {
            lines,
            selections,
            cursor,
            bounds,
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
        for layout in &prepaint.lines {
            layout
                .line
                .paint(
                    layout.bounds.origin,
                    layout.bounds.bottom() - layout.bounds.top(),
                    window,
                    context,
                )
                .ok();
        }
        if let Some(cursor) = prepaint.cursor {
            window.paint_quad(cursor);
        }
        if self.focus_handle.is_focused(window) {
            window.request_animation_frame();
        }
        self.view.update(context, |view, context| {
            if view.store_notes_ai_input_text_layouts(prepaint.lines, prepaint.bounds) {
                context.notify();
            }
        });
    }
}

/// 按输入框可用宽度生成笔记 AI 输入区的软换行范围。
///
/// 业务意图：
/// - 笔记 AI 输入框是自绘文本元素，不能依赖系统控件自动换行；这里在绘制前把每个真实文本行拆成多个视觉行。
/// - 返回范围仍然是原始 UTF-8 字节下标，保证鼠标点选、拖拽选区、复制和 IME 组合文本都能继续复用原有索引逻辑。
///
/// 边界条件：
/// - 空行必须保留为一个 0 长度范围，否则用户输入连续换行时光标会丢失可点击行。
/// - 超长英文单词或连续中文没有空格时，按 UTF-8 字符边界强制断行，禁止切到多字节字符内部。
fn notes_ai_input_wrapped_ranges(
    text: &str,
    hard_line_ranges: Vec<Range<usize>>,
    wrap_width: Pixels,
    font_size: Pixels,
    measurement_run: &TextRun,
    window: &mut Window,
) -> Vec<Range<usize>> {
    let mut visual_ranges = Vec::new();
    for hard_range in hard_line_ranges {
        if hard_range.is_empty() {
            visual_ranges.push(hard_range);
            continue;
        }
        let hard_line_text = &text[hard_range.clone()];
        let full_line = window.text_system().shape_line(
            SharedString::from(hard_line_text.to_string()),
            font_size,
            &[TextRun {
                len: hard_line_text.len(),
                ..measurement_run.clone()
            }],
            None,
        );
        visual_ranges.extend(notes_ai_input_wrap_single_line_range(
            hard_line_text,
            hard_range.start,
            &full_line,
            wrap_width,
        ));
    }
    visual_ranges
}

/// 将一个没有硬换行符的输入行拆成多个视觉行范围。
///
/// 实现原因：
/// - GPUI 的 `ShapedLine` 可以提供真实字形宽度和 UTF-8 字节下标命中结果；基于它折行可以兼容中文、英文、emoji 和混合文本。
/// - 优先复用富文本编辑器已有的空白优先断行策略，让 AI 输入框和正文编辑区在英文长句上的行为保持一致。
fn notes_ai_input_wrap_single_line_range(
    line_text: &str,
    global_start: usize,
    full_line: &ShapedLine,
    wrap_width: Pixels,
) -> Vec<Range<usize>> {
    let wrap_width = wrap_width.max(px(1.0));
    let mut ranges = Vec::new();
    let mut local_start = 0usize;
    while local_start < line_text.len() {
        let start_x = full_line.x_for_index(local_start);
        let remaining_width = full_line.width - start_x;
        let local_end = if remaining_width <= wrap_width {
            line_text.len()
        } else {
            notes_ai_input_wrap_end(line_text, local_start, full_line, start_x, wrap_width)
        };
        let local_end = local_end.min(line_text.len()).max(local_start);
        if local_end == local_start {
            let next = notes_ai_next_char_boundary_after(line_text, local_start);
            ranges.push(global_start + local_start..global_start + next);
            local_start = next;
        } else {
            ranges.push(global_start + local_start..global_start + local_end);
            local_start = local_end;
        }
    }
    ranges
}

/// 计算单个软换行片段的结束 UTF-8 字节下标。
fn notes_ai_input_wrap_end(
    line_text: &str,
    local_start: usize,
    full_line: &ShapedLine,
    start_x: Pixels,
    wrap_width: Pixels,
) -> usize {
    let tentative = clamp_note_rich_text_boundary(
        line_text,
        full_line.closest_index_for_x(start_x + wrap_width),
    );
    let preferred = preferred_soft_wrap_end(line_text, local_start, tentative);
    let preferred_width = full_line.x_for_index(preferred) - start_x;
    let mut end = if preferred < tentative && preferred_width >= wrap_width * 0.88 {
        preferred
    } else {
        tentative
    };
    while end > local_start && full_line.x_for_index(end) - start_x > wrap_width {
        let previous = notes_ai_previous_char_boundary_before(line_text, end);
        if previous <= local_start {
            break;
        }
        end = previous;
    }
    if end <= local_start {
        notes_ai_next_char_boundary_after(line_text, local_start)
    } else {
        end
    }
}

/// 返回指定 UTF-8 边界后的下一个字符边界。
fn notes_ai_next_char_boundary_after(text: &str, start: usize) -> usize {
    let start = clamp_note_rich_text_boundary(text, start).min(text.len());
    text[start..]
        .chars()
        .next()
        .map(|character| start + character.len_utf8())
        .unwrap_or(text.len())
}

/// 返回指定 UTF-8 边界前的上一个字符边界。
fn notes_ai_previous_char_boundary_before(text: &str, end: usize) -> usize {
    let end = clamp_note_rich_text_boundary(text, end).min(text.len());
    text[..end]
        .char_indices()
        .next_back()
        .map(|(index, _)| index)
        .unwrap_or(0)
}

/// 笔记 AI 输入区绘制前计算结果。
///
/// 业务意图：
/// - GPUI 绘制阶段需要同时绘制文本、选区和光标；预先缓存这些结果可以避免 paint 阶段再次读取和排版 UI 状态。
pub(in crate::app) struct NotesAiInputPrepaint {
    /// 每行文本布局。
    lines: Vec<NotesAiInputLineLayout>,
    /// 选区背景块。
    selections: Vec<PaintQuad>,
    /// 光标绘制块。
    cursor: Option<PaintQuad>,
    /// 输入元素整体边界。
    bounds: Bounds<Pixels>,
}
