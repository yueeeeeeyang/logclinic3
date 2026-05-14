// 左侧日志目录树搜索输入元素。
//
// 业务意图：
// - 将文件树搜索框的 GPUI 自绘输入、平台输入协议注册和光标/选区绘制从目录树视图中拆出。
// - 该文件只处理输入元素本身；搜索过滤、命中导航和打开日志文件仍由 `tree_view.rs` 中的目录树交互逻辑负责。
//
// 边界条件：
// - 输入框只搜索左侧目录树文件名，不读取日志正文、不启动全文搜索任务、不持久化关键字。
// - 输入保持单行文本，换行清理由 `EntityInputHandler` 统一完成，保证中文 IME 与现有搜索框行为一致。

use super::*;

/// 左侧目录树搜索输入框的预绘制结果。
///
/// 业务意图：
/// - 文件树搜索框复用 GPUI 自绘输入路径，需要在预绘制阶段缓存文本排版、选区和光标矩形。
/// - 该输入框只搜索左侧目录树文件名，不读取日志正文，也不参与已有全文搜索窗口任务。
pub(in crate::app) struct LogTreeSearchInputPrepaint {
    /// 当前帧的单行字形布局，用于绘制文本并回写给 `MainView` 供鼠标命中测试。
    line: ShapedLine,
    /// 当前选择范围对应的高亮矩形；无选择时为空。
    selection: Option<PaintQuad>,
    /// 当前光标矩形；有非空选择时为空。
    cursor: Option<PaintQuad>,
    /// 当前帧文本水平滚动偏移，单位为 GPUI 逻辑像素。
    horizontal_scroll_px: f32,
}

/// 左侧目录树搜索输入元素。
///
/// 业务意图：
/// - GPUI 当前版本没有可直接复用的单行输入控件，因此沿用项目内搜索窗口的自绘输入实现。
/// - 元素只负责文本绘制和平台输入注册，过滤、命中和打开文件行为仍由 `MainView` 的目录树状态协调。
///
/// 边界条件：
/// - 输入框保持单行文本；平台提交的换行会在 `EntityInputHandler` 中被清理。
/// - 搜索只按文件名普通子串匹配，不支持正则，也不会扫描日志正文内容。
pub(in crate::app) struct LogTreeSearchInputElement {
    /// 主视图实体，用于读取和写回搜索输入状态。
    pub(in crate::app) view: Entity<MainView>,
    /// 搜索框焦点句柄。
    pub(in crate::app) focus_handle: gpui::FocusHandle,
    /// 输入为空时显示的占位文案。
    pub(in crate::app) placeholder: &'static str,
    /// 当前主题调色板，用于绘制文本、占位、选区和光标。
    pub(in crate::app) palette: AppThemePalette,
}

impl IntoElement for LogTreeSearchInputElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for LogTreeSearchInputElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<LogTreeSearchInputPrepaint>;

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
        let (text, selection_range, marked_range, current_scroll_px, cursor_visible_by_activity) = {
            let view = self.view.read(context);
            let snapshot = view.log_tree_search_text_snapshot();
            (
                snapshot.text,
                snapshot.selection_range,
                snapshot.marked_range,
                snapshot.horizontal_scroll_px,
                view.search_text_cursor_visible(),
            )
        };
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
        let cursor_visible = focused && !has_selection && cursor_visible_by_activity;
        let cursor = cursor_visible.then(|| {
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

        Some(LogTreeSearchInputPrepaint {
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
            view.store_log_tree_search_layout(prepaint.line, bounds, prepaint.horizontal_scroll_px);
        });
    }
}
