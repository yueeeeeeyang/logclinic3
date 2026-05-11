// 搜索窗口自绘输入元素。
//
// 业务意图：
// - 从搜索窗口渲染中拆出关键字和目录目标输入元素，保持搜索 UI 框架、输入协议和 IME 状态职责分离。
// - 本轮只移动元素实现，不改变单行输入、双击选词、选择范围或光标闪烁行为。

use super::*;

/// 搜索输入框文本元素的预绘制结果。
///
/// 业务意图：
/// - GPUI 的官方输入示例会在 `prepaint` 阶段完成文本排版、选区矩形和光标矩形计算。
/// - 搜索关键字和目录目标输入框沿用这一路径，避免自绘文本再用估算字符宽度处理鼠标命中。
pub(in crate::app) struct SearchTextInputPrepaint {
    /// 当前帧的单行字形布局，用于绘制文本并回写给 `MainView` 供鼠标命中测试。
    line: ShapedLine,
    /// 当前选择范围对应的高亮矩形；无选择时为空。
    selection: Option<PaintQuad>,
    /// 当前光标矩形；有非空选择时为空。
    cursor: Option<PaintQuad>,
}

/// 搜索输入框的 GPUI 文本输入元素。
///
/// 业务意图：
/// - GPUI 0.2.2 没有公开导出的现成 `TextInput` 控件，但官方示例提供的做法是自定义 `Element`，
///   在绘制阶段调用 `Window::handle_input`，并使用 GPUI 文本系统 `shape_line` 管理光标和选区。
/// - 该元素把搜索关键字和目录目标输入框改为同一套 GPUI 文本输入实现，支持中文 IME、全选、双击选词和三连击全选。
///
/// 边界条件：
/// - 输入框仍只支持单行文本；平台提交的换行会在 `EntityInputHandler` 中清理。
/// - 该元素只负责文本绘制和平台输入注册，搜索业务状态仍保存在 `MainView.search_dialog` 中。
pub(in crate::app) struct SearchTextInputElement {
    /// 主视图实体，用于读取和写回搜索输入状态。
    pub(in crate::app) view: Entity<MainView>,
    /// 当前元素对应的输入槽位。
    pub(in crate::app) input_kind: SearchTextInputKind,
    /// 该输入框的焦点句柄。
    pub(in crate::app) focus_handle: gpui::FocusHandle,
    /// 输入为空时显示的占位文案。
    pub(in crate::app) placeholder: &'static str,
    /// 当前主题调色板，用于绘制文本、占位、选区和光标。
    pub(in crate::app) palette: AppThemePalette,
}

impl IntoElement for SearchTextInputElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SearchTextInputElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<SearchTextInputPrepaint>;

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
        // GPUI 文本元素需要在布局阶段拿到明确行高；如果使用相对高度，父级 flex 布局在某些窗口
        // 尺寸计算路径下会给出 0 高度，导致文字、光标和选区都完成状态更新但没有可见绘制区域。
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
        let (text, selection_range, marked_range, cursor_visible_by_activity) = {
            let view = self.view.read(context);
            view.search_text_snapshot(self.input_kind).map(
                |(text, selection_range, marked_range)| {
                    (
                        text,
                        selection_range,
                        marked_range,
                        view.search_text_cursor_visible(),
                    )
                },
            )
        }?;
        let style = window.text_style();
        let display_text = if text.is_empty() {
            SharedString::from(self.placeholder)
        } else {
            SharedString::from(text.clone())
        };
        let text_color = if text.is_empty() {
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
                        len: display_text.len().saturating_sub(marked_range.end),
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
            .shape_line(display_text, font_size, &runs, None);
        let focused = self.focus_handle.is_focused(window);
        let selection_range = MainView::clamp_search_text_range(&text, selection_range);
        let has_selection =
            focused && !text.is_empty() && selection_range.start < selection_range.end;
        let cursor_index = selection_range.end;
        let selection = has_selection.then(|| {
            let mut selection_color = rgb(self.palette.accent);
            selection_color.a = 0.32;
            fill(
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
            )
        });
        let cursor_visible = focused && !has_selection && cursor_visible_by_activity;
        let cursor = cursor_visible.then(|| {
            fill(
                Bounds::new(
                    point(bounds.left() + line.x_for_index(cursor_index), bounds.top()),
                    size(px(1.5), bounds.bottom() - bounds.top()),
                ),
                rgb(self.palette.accent),
            )
        });

        Some(SearchTextInputPrepaint {
            line,
            selection,
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
        if let Some(selection) = prepaint.selection {
            window.paint_quad(selection);
        }
        prepaint
            .line
            .paint(bounds.origin, window.line_height(), window, context)
            .ok();
        if let Some(cursor) = prepaint.cursor {
            window.paint_quad(cursor);
        }
        if self.focus_handle.is_focused(window) {
            // 光标闪烁不依赖业务状态变化；只要输入框仍聚焦，就请求下一帧重绘，由时间片决定当前帧是否显示光标。
            window.request_animation_frame();
        }
        self.view.update(context, |view, _context| {
            view.store_search_text_layout(self.input_kind, prepaint.line, bounds);
        });
    }
}
