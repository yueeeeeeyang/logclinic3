// 笔记自绘多行编辑元素。
//
// 业务意图：
// - GPUI 当前没有项目所需的多行文本编辑控件，因此笔记编辑器复用项目内自绘文本区方案支持中文 IME、选择、粘贴和光标绘制。
// - 本元素只负责绘制和平台输入协议接入，真实编辑行为由 `notes_panel/actions.rs` 维护。

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

/// 笔记多行编辑元素。
pub(in crate::app) struct NoteEditorElement {
    /// 主视图实体，用于读取和写回笔记编辑状态。
    pub(in crate::app) view: Entity<MainView>,
    /// 输入框焦点句柄。
    pub(in crate::app) focus_handle: gpui::FocusHandle,
    /// 当前主题调色板。
    pub(in crate::app) palette: AppThemePalette,
}

impl IntoElement for NoteEditorElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for NoteEditorElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<NoteEditorPrepaint>;

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
        let line_count = self.view.read(context).note_editor_visual_line_count();
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height =
            px((line_count as f32 * NOTES_TEXT_LINE_HEIGHT).max(NOTES_TEXT_LINE_HEIGHT)).into();
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
            let (text, selection_range, marked_range) = view.note_editor_text_snapshot();
            (
                text,
                selection_range,
                marked_range,
                view.search_text_cursor_visible(),
            )
        };
        let focused = self.focus_handle.is_focused(window);
        let style = window.text_style();
        let font_size = px(NOTES_TEXT_FONT_SIZE);
        let line_height = px(NOTES_TEXT_LINE_HEIGHT);
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
                SharedString::from("输入笔记内容")
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
            let line_top = bounds.top() + px(line_index as f32 * NOTES_TEXT_LINE_HEIGHT);
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
                    selection_color.a = 0.30;
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

            lines.push(NoteEditorPaintLine {
                byte_range,
                bounds: line_bounds,
                line,
            });
        }

        Some(NoteEditorPrepaint {
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
            layouts.push(NoteEditorLineLayout {
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
            view.store_note_editor_text_layouts(layouts, bounds);
        });
    }
}
