// 设置页线程过滤输入元素。
//
// 业务意图：
// - 从设置窗口拆出 线程过滤多行输入元素，让设置页框架只负责页签和表单布局。
// - 迁移保持 GPUI 输入协议、IME 范围、绘制顺序和主视图状态写回逻辑完全不变。

use super::*;

/// 线程日志分析过滤输入区单行绘制状态。
pub(in crate::app) struct ThreadAnalysisFilterTextPaintLine {
    /// 当前行对应的原始文本 UTF-8 字节范围。
    byte_range: Range<usize>,
    /// 当前行绘制边界。
    bounds: Bounds<Pixels>,
    /// 当前行字形布局。
    line: ShapedLine,
}

/// 线程日志分析过滤输入区绘制状态。
pub(in crate::app) struct ThreadAnalysisFilterTextAreaPrepaint {
    /// 当前帧需要绘制的所有文本行。
    lines: Vec<ThreadAnalysisFilterTextPaintLine>,
    /// 当前选择范围对应的高亮矩形。
    selections: Vec<PaintQuad>,
    /// 当前光标矩形。
    cursor: Option<PaintQuad>,
}

/// 线程日志分析过滤多行输入元素。
///
/// 业务意图：
/// - GPUI 0.2.2 没有现成多行文本框；该元素复用搜索输入框的自定义元素方案，注册平台输入协议并手动绘制文本、选区和光标。
/// - 输入内容可能是线程名通配列表或完整 Java 堆栈，必须保留换行并使用等宽字体，方便用户核对过滤片段。
///
/// 边界条件：
/// - 当前不做自动换行，长线程名或长堆栈行横向超出时由输入区裁切；过滤匹配仍使用完整原文，不受显示裁切影响。
pub(in crate::app) struct ThreadAnalysisFilterTextAreaElement {
    /// 当前元素绑定的过滤输入类型。
    ///
    /// 业务意图：
    /// - 同一个自绘文本框既用于线程名规则，也用于堆栈片段；元素只保存类型，不直接持有文本，避免渲染阶段复制状态分支。
    pub(in crate::app) kind: ThreadAnalysisFilterInputKind,
    /// 主视图实体，用于读取和写回过滤输入状态。
    pub(in crate::app) view: Entity<MainView>,
    /// 过滤输入区焦点句柄。
    pub(in crate::app) focus_handle: gpui::FocusHandle,
    /// 当前是否允许平台输入法和键盘写入文本。
    ///
    /// 业务意图：
    /// - 只读态仍要绘制文本和选区，但不显示插入光标，避免用户误以为内容已经可编辑。
    pub(in crate::app) editable: bool,
    /// 输入为空时显示的占位文案。
    pub(in crate::app) placeholder: &'static str,
    /// 当前主题调色板。
    pub(in crate::app) palette: AppThemePalette,
}

impl IntoElement for ThreadAnalysisFilterTextAreaElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ThreadAnalysisFilterTextAreaElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<ThreadAnalysisFilterTextAreaPrepaint>;

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
        let line_count = self
            .view
            .read(context)
            .thread_analysis_filter_visual_line_count_for(self.kind);
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = px(
            (line_count as f32 * THREAD_ANALYSIS_FILTER_TEXT_LINE_HEIGHT)
                .max(THREAD_ANALYSIS_FILTER_TEXT_LINE_HEIGHT),
        )
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
            let (text, selection_range, marked_range) =
                view.thread_analysis_filter_text_snapshot(self.kind);
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
        let line_height = px(THREAD_ANALYSIS_FILTER_TEXT_LINE_HEIGHT);
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
            let line_top =
                bounds.top() + px(line_index as f32 * THREAD_ANALYSIS_FILTER_TEXT_LINE_HEIGHT);
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
                && self.editable
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

            lines.push(ThreadAnalysisFilterTextPaintLine {
                byte_range,
                bounds: line_bounds,
                line,
            });
        }

        Some(ThreadAnalysisFilterTextAreaPrepaint {
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
            layouts.push(ThreadAnalysisFilterLineLayout {
                byte_range: paint_line.byte_range,
                line: paint_line.line,
                bounds: paint_line.bounds,
            });
        }
        if let Some(cursor) = prepaint.cursor {
            window.paint_quad(cursor);
        }
        if self.focus_handle.is_focused(window) && self.editable {
            window.request_animation_frame();
        }
        self.view.update(context, |view, _context| {
            view.store_thread_analysis_filter_text_layouts(self.kind, layouts, bounds);
        });
    }
}
