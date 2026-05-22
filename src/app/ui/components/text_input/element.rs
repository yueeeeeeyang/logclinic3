// 通用单行输入框 GPUI 元素。
//
// 业务意图：
// - 将输入框文本排版、placeholder、选区、光标、IME marked underline 和平台输入注册集中在一个组件中。
// - 调用方继续负责边框、背景、按钮和业务副作用，组件只绘制输入框内部文本层。

use super::*;

/// 通用输入框文本元素的预绘制结果。
pub(in crate::app) struct TextInputPrepaint {
    /// 输入框当前帧命中区域。
    ///
    /// 业务意图：
    /// - 通用输入框需要自己消费鼠标按下、拖拽和释放事件，不能再依赖搜索或连接页面外层容器各自处理。
    /// - Hitbox 由 GPUI 在 `prepaint` 阶段创建，`paint` 阶段用来判断事件是否命中最上层输入框。
    hitbox: gpui::Hitbox,
    /// 当前帧展示文本的单行字形布局。
    line: ShapedLine,
    /// 当前选择范围对应的高亮矩形。
    selection: Option<PaintQuad>,
    /// 当前光标矩形。
    cursor: Option<PaintQuad>,
    /// 当前水平滚动偏移。
    horizontal_scroll_px: f32,
}

/// 可复用的 GPUI 单行文本输入元素。
///
/// 边界条件：
/// - 该元素只绘制和注册平台输入，不直接保存业务文本；真实状态通过 `TextInputBinding` 回写到宿主视图。
/// - 外层容器需要负责 `track_focus`、键盘事件和视觉边框；鼠标定位、拖拽选区与释放清理由组件统一处理。
pub(in crate::app) struct TextInputElement<V: TextInputElementHost> {
    /// 宿主视图实体，用于读取输入快照并回写布局。
    pub(in crate::app) view: Entity<V>,
    /// 输入框绑定位置。
    pub(in crate::app) binding: TextInputBinding,
    /// 焦点句柄。
    pub(in crate::app) focus_handle: gpui::FocusHandle,
    /// 空文本时展示的占位文案。
    pub(in crate::app) placeholder: &'static str,
    /// 当前主题调色板。
    pub(in crate::app) palette: AppThemePalette,
}

/// 通用输入框宿主视图接口。
///
/// 业务意图：
/// - 搜索、连接弹窗和文件管理地址栏都需要同一套单行输入框元素，但它们分别属于不同 GPUI `Entity`。
/// - 组件只依赖该接口读取快照、回写布局和驱动鼠标选区，避免把文件管理窗口强行塞进 `MainView` 状态。
///
/// 边界条件：
/// - 宿主必须同时实现 `EntityInputHandler`，这样组件才能在绘制阶段向 GPUI 注册平台输入法处理器。
/// - 鼠标事件可能在弹窗或窗口关闭后的下一帧到达，宿主方法应在绑定不存在时返回 `false`。
pub(in crate::app) trait TextInputElementHost: EntityInputHandler {
    /// 读取当前输入框绘制快照。
    fn text_input_snapshot(&self, binding: TextInputBinding) -> Option<TextInputSnapshot>;
    /// 保存最近一次绘制布局，供鼠标命中和 IME 候选窗口定位使用。
    fn store_text_input_layout(
        &mut self,
        binding: TextInputBinding,
        line: ShapedLine,
        bounds: Bounds<Pixels>,
        horizontal_scroll_px: f32,
    );
    /// 处理鼠标按下，开始定位、选词、全选或拖拽选区。
    fn begin_text_input_mouse_interaction(
        &mut self,
        binding: TextInputBinding,
        event: &MouseDownEvent,
        was_focused: bool,
        context: &mut Context<Self>,
    ) -> bool;
    /// 鼠标拖拽时扩展选区。
    fn update_text_input_mouse_drag(
        &mut self,
        binding: TextInputBinding,
        position: Point<Pixels>,
        context: &mut Context<Self>,
    ) -> bool;
    /// 鼠标释放时结束拖拽状态。
    fn finish_text_input_mouse_drag(
        &mut self,
        binding: TextInputBinding,
        context: &mut Context<Self>,
    ) -> bool;
    /// 当前光标是否需要绘制。
    fn text_input_cursor_visible(&self) -> bool;
}

impl<V: TextInputElementHost> IntoElement for TextInputElement<V> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<V: TextInputElementHost> Element for TextInputElement<V> {
    type RequestLayoutState = ();
    type PrepaintState = Option<TextInputPrepaint>;

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
        // 文本元素必须在布局阶段拿到明确行高，否则 flex 压缩路径下可能出现可输入但不可见的输入框。
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
        let snapshot = self.view.read(context).text_input_snapshot(self.binding)?;
        let hitbox = window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal);
        let display_text = if snapshot.text.is_empty() {
            SharedString::from(self.placeholder)
        } else {
            SharedString::from(snapshot.display_mode.display_text(&snapshot.text))
        };
        let display_text_len = display_text.len();

        let style = window.text_style();
        let text_color = if snapshot.text.is_empty() {
            rgb(self.palette.muted_text).into()
        } else {
            style.color
        };
        let base_run = TextRun {
            len: display_text_len,
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if !snapshot.text.is_empty() {
            if let Some(marked_range) = snapshot.marked_range.clone() {
                let display_marked_range = snapshot
                    .display_mode
                    .display_range_for_text_range(&snapshot.text, marked_range);
                vec![
                    TextRun {
                        len: display_marked_range.start,
                        ..base_run.clone()
                    },
                    TextRun {
                        len: display_marked_range
                            .end
                            .saturating_sub(display_marked_range.start),
                        underline: Some(UnderlineStyle {
                            color: Some(base_run.color),
                            thickness: px(1.0),
                            wavy: false,
                        }),
                        ..base_run.clone()
                    },
                    TextRun {
                        len: display_text_len.saturating_sub(display_marked_range.end),
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
        let selection_range = text_input_clamp_range(&snapshot.text, snapshot.selection_range);
        let has_selection =
            focused && !snapshot.text.is_empty() && selection_range.start < selection_range.end;
        let cursor_index = snapshot
            .display_mode
            .display_index_for_text_index(&snapshot.text, selection_range.end);
        let content_width = if snapshot.text.is_empty() {
            px(0.0)
        } else {
            line.x_for_index(display_text_len)
        };
        let horizontal_scroll_px = text_input_horizontal_scroll_offset(
            snapshot.horizontal_scroll_px,
            line.x_for_index(cursor_index),
            content_width,
            bounds.size.width,
            focused,
        );
        let text_origin = point(bounds.left() - px(horizontal_scroll_px), bounds.top());
        let selection = has_selection.then(|| {
            let mut selection_color = rgb(self.palette.accent);
            selection_color.a = 0.32;
            let display_selection = snapshot
                .display_mode
                .display_range_for_text_range(&snapshot.text, selection_range);
            let left = f32::from(text_origin.x + line.x_for_index(display_selection.start))
                .clamp(f32::from(bounds.left()), f32::from(bounds.right()));
            let right = f32::from(text_origin.x + line.x_for_index(display_selection.end))
                .clamp(f32::from(bounds.left()), f32::from(bounds.right()));
            fill(
                Bounds::from_corners(
                    point(px(left.min(right)), bounds.top()),
                    point(px(left.max(right)), bounds.bottom()),
                ),
                selection_color,
            )
        });
        let cursor_visible =
            focused && !has_selection && self.view.read(context).text_input_cursor_visible();
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

        Some(TextInputPrepaint {
            hitbox,
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
        let hitbox = prepaint.hitbox.clone();
        if hitbox.is_hovered(window) {
            window.set_cursor_style(gpui::CursorStyle::IBeam, &hitbox);
        }
        self.register_mouse_handlers(hitbox, window);
        if let Some(selection) = prepaint.selection {
            window.paint_quad(selection);
        }
        // 长文本水平滚动后仍可能有字形位于输入框外侧；这里用 GPUI 内容裁剪限制文本绘制区域，
        // 避免 SMB 地址、长路径或长关键字溢出到标签和弹窗外部。
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
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
        });
        if let Some(cursor) = prepaint.cursor {
            window.paint_quad(cursor);
        }
        if self.focus_handle.is_focused(window) {
            window.request_animation_frame();
        }
        self.view.update(context, |view, _context| {
            view.store_text_input_layout(
                self.binding,
                prepaint.line,
                bounds,
                prepaint.horizontal_scroll_px,
            );
        });
    }
}

impl<V: TextInputElementHost> TextInputElement<V> {
    /// 注册输入框内部鼠标事件。
    ///
    /// 业务意图：
    /// - 单行输入框的单击定位、Shift+单击扩展、双击选词、三击全选和拖拽选区都是组件职责，不能散落到连接、搜索等具体功能中。
    /// - 释放事件使用捕获阶段统一清理拖拽锚点，解决弹窗里按下后释放到其它控件或空白区域时状态残留的问题。
    ///
    /// 边界条件：
    /// - 只处理左键；右键菜单、滚轮和其它鼠标按钮仍由外层页面或平台默认逻辑处理。
    /// - 事件监听只在当前帧有效，绑定对应的输入框消失后 `MainView` 的安全入口会返回 `false` 并忽略。
    fn register_mouse_handlers(&self, hitbox: gpui::Hitbox, window: &mut Window) {
        let mouse_down_hitbox = hitbox.clone();
        let mouse_down_view = self.view.clone();
        let mouse_down_binding = self.binding;
        let mouse_down_focus = self.focus_handle.clone();
        window.on_mouse_event(
            move |event: &MouseDownEvent, phase, window: &mut Window, context: &mut App| {
                if !phase.bubble()
                    || event.button != MouseButton::Left
                    || !mouse_down_hitbox.is_hovered(window)
                {
                    return;
                }

                let was_focused = mouse_down_focus.is_focused(window);
                let handled = mouse_down_view.update(context, |view, context| {
                    view.begin_text_input_mouse_interaction(
                        mouse_down_binding,
                        event,
                        was_focused,
                        context,
                    )
                });
                if handled {
                    window.focus(&mouse_down_focus);
                    window.prevent_default();
                    context.stop_propagation();
                }
            },
        );

        let mouse_move_view = self.view.clone();
        let mouse_move_binding = self.binding;
        window.on_mouse_event(
            move |event: &MouseMoveEvent, phase, window: &mut Window, context: &mut App| {
                if !phase.capture() || event.pressed_button != Some(MouseButton::Left) {
                    return;
                }
                let handled = mouse_move_view.update(context, |view, context| {
                    view.update_text_input_mouse_drag(mouse_move_binding, event.position, context)
                });
                if handled {
                    window.prevent_default();
                    context.stop_propagation();
                }
            },
        );

        let mouse_up_hitbox = hitbox;
        let mouse_up_view = self.view.clone();
        let mouse_up_binding = self.binding;
        window.on_mouse_event(
            move |event: &MouseUpEvent, phase, _window: &mut Window, context: &mut App| {
                if event.button != MouseButton::Left {
                    return;
                }
                if phase.capture() {
                    let handled = mouse_up_view.update(context, |view, context| {
                        view.finish_text_input_mouse_drag(mouse_up_binding, context)
                    });
                    if handled {
                        context.stop_propagation();
                    }
                } else if phase.bubble() && mouse_up_hitbox.is_hovered(_window) {
                    context.stop_propagation();
                }
            },
        );
    }
}
