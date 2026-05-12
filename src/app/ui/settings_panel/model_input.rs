// 设置页模型配置输入元素。
//
// 业务意图：
// - 从设置窗口拆出 模型配置单行输入元素，让设置页框架只负责页签和表单布局。
// - 迁移保持 GPUI 输入协议、IME 范围、绘制顺序和主视图状态写回逻辑完全不变。

use super::*;

/// 模型配置输入框的预绘制结果。
pub(in crate::app) struct ModelConfigInputPrepaint {
    /// 当前帧单行字形布局。
    line: ShapedLine,
    /// 当前选择范围对应的高亮矩形。
    selection: Option<PaintQuad>,
    /// 当前插入光标矩形。
    cursor: Option<PaintQuad>,
    /// 当前帧文本水平滚动偏移，单位为 GPUI 逻辑像素。
    horizontal_scroll_px: f32,
}

/// 模型配置单行输入元素。
///
/// 业务意图：
/// - 设置-模型页需要四个支持 IME 的自绘输入框，复用快搜单行输入的绘制和平台输入协议方案。
/// - 元素只负责绘制可见文本、选区和光标；真实字段值、掩码状态和保存逻辑仍由 `MainView` 统一管理。
pub(in crate::app) struct ModelConfigInputElement {
    /// 主视图实体，用于读取和写回模型配置输入状态。
    pub(in crate::app) view: Entity<MainView>,
    /// 当前字段类型。
    pub(in crate::app) kind: ModelConfigInputKind,
    /// 输入区焦点句柄。
    pub(in crate::app) focus_handle: gpui::FocusHandle,
    /// 输入为空时显示的占位文案。
    pub(in crate::app) placeholder: &'static str,
    /// 当前主题调色板。
    pub(in crate::app) palette: AppThemePalette,
}

impl IntoElement for ModelConfigInputElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ModelConfigInputElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<ModelConfigInputPrepaint>;

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
        style.size.height = window.line_height().into();
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
        let (
            text,
            display_text,
            selection_range,
            marked_range,
            current_scroll_px,
            cursor_visible_by_activity,
        ) = {
            let view = self.view.read(context);
            let snapshot = view.model_config_input_text_snapshot(self.kind);
            (
                snapshot.text,
                snapshot.display_text,
                snapshot.selection_range,
                snapshot.marked_range,
                snapshot.horizontal_scroll_px,
                view.search_text_cursor_visible(),
            )
        };
        let style = window.text_style();
        let rendered_text = if text.is_empty() {
            SharedString::from(self.placeholder)
        } else {
            SharedString::from(display_text)
        };
        let text_color = if text.is_empty() {
            rgb(self.palette.muted_text).into()
        } else {
            style.color
        };
        let base_run = TextRun {
            len: rendered_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if !text.is_empty() {
            if let Some(marked_range) = marked_range {
                vec![
                    TextRun {
                        len: marked_range.start,
                        ..base_run.clone()
                    },
                    TextRun {
                        len: marked_range.end.saturating_sub(marked_range.start),
                        underline: Some(UnderlineStyle {
                            color: Some(base_run.color),
                            thickness: px(1.0),
                            wavy: false,
                        }),
                        ..base_run.clone()
                    },
                    TextRun {
                        len: rendered_text.len().saturating_sub(marked_range.end),
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

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(rendered_text, font_size, &runs, None);
        let focused = self.focus_handle.is_focused(window);
        let selection_range = MainView::clamp_search_text_range(&text, selection_range);
        let has_selection =
            focused && !text.is_empty() && selection_range.start < selection_range.end;
        let cursor_index = selection_range.end;
        let content_width = if text.is_empty() {
            px(0.0)
        } else {
            line.x_for_index(text.len())
        };
        let horizontal_scroll_px = MainView::single_line_horizontal_scroll_offset(
            current_scroll_px,
            line.x_for_index(cursor_index),
            content_width,
            bounds.size.width,
            focused,
        );
        let text_origin = point(bounds.left() - px(horizontal_scroll_px), bounds.top());
        let selection = has_selection.then(|| {
            let mut selection_color = rgb(self.palette.accent);
            selection_color.a = 0.32;
            let left = f32::from(text_origin.x + line.x_for_index(selection_range.start))
                .clamp(f32::from(bounds.left()), f32::from(bounds.right()));
            let right = f32::from(text_origin.x + line.x_for_index(selection_range.end))
                .clamp(f32::from(bounds.left()), f32::from(bounds.right()));
            fill(
                Bounds::from_corners(
                    point(px(left.min(right)), bounds.top()),
                    point(px(left.max(right)), bounds.bottom()),
                ),
                selection_color,
            )
        });
        let cursor = (focused && !has_selection && cursor_visible_by_activity).then(|| {
            let cursor_right_limit = (f32::from(bounds.right()) - SINGLE_LINE_INPUT_CARET_WIDTH)
                .max(f32::from(bounds.left()));
            let cursor_x = f32::from(text_origin.x + line.x_for_index(cursor_index))
                .clamp(f32::from(bounds.left()), cursor_right_limit);
            fill(
                Bounds::new(
                    point(px(cursor_x), bounds.top()),
                    size(
                        px(SINGLE_LINE_INPUT_CARET_WIDTH),
                        bounds.bottom() - bounds.top(),
                    ),
                ),
                rgb(self.palette.accent),
            )
        });

        Some(ModelConfigInputPrepaint {
            line,
            selection,
            cursor,
            horizontal_scroll_px,
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
        if let Some(selection) = prepaint.selection {
            window.paint_quad(selection);
        }
        prepaint
            .line
            .paint(
                point(
                    bounds.left() - px(prepaint.horizontal_scroll_px),
                    bounds.top(),
                ),
                window.line_height(),
                window,
                context,
            )
            .ok();
        if let Some(cursor) = prepaint.cursor {
            window.paint_quad(cursor);
        }
        if self.focus_handle.is_focused(window) {
            window.request_animation_frame();
        }
        self.view.update(context, |view, _context| {
            view.store_model_config_input_layout(
                self.kind,
                prepaint.line,
                bounds,
                prepaint.horizontal_scroll_px,
            );
        });
    }
}
