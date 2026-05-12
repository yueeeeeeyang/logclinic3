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
