// 主功能工作区渲染协调。
//
// 业务意图：
// - 本文件由原主窗口工作区实现机械拆分而来，只承载 GPUI 壳层内的 主功能页选择、日志分析页面、HPROF 页面和保存覆盖确认弹窗渲染。
// - 拆分过程保持所有状态字段、事件时序和用户可见行为不变，便于后续按视图职责继续收束。

use super::*;

impl MainView {
    /// 渲染当前大功能页内容。
    ///
    /// 业务意图：
    /// - 左侧主导航只负责切换功能，右侧区域按当前功能渲染完整工作区。
    /// - 日志分析保留现有目录树和日志正文；HPROF 解析嵌入可复用分析实体；AI 对话渲染完整聊天工作区。
    pub(in crate::app) fn render_main_feature_page(
        &mut self,
        context: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match self.navigation.active_main_feature {
            MainFeature::LogAnalysis => self.render_log_analysis_page(context).into_any_element(),
            MainFeature::Notes => self.render_notes_page(context).into_any_element(),
            MainFeature::Connections => self.render_connections_page(context).into_any_element(),
            MainFeature::HprofAnalysis => {
                self.render_hprof_analysis_page(context).into_any_element()
            }
            MainFeature::AiChat => self.render_ai_chat_page(context).into_any_element(),
        }
    }

    /// 渲染日志分析大功能页。
    ///
    /// 业务意图：
    /// - 根级顶部工具栏已移除，日志页内部保留“加载日志”和“搜索”操作栏，下面继续使用原有左右分栏日志工作区。
    pub(in crate::app) fn render_log_analysis_page(
        &self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("log-analysis-page")
            .flex()
            .flex_col()
            .size_full()
            .child(self.render_log_action_bar(context))
            .child(self.render_content(context))
    }

    /// 渲染 HPROF 解析大功能页。
    ///
    /// 业务意图：
    /// - HPROF 入口从独立窗口迁入主窗口，页顶部提供文件选择按钮，下面直接嵌入解析视图的空态、进度或结果。
    pub(in crate::app) fn render_hprof_analysis_page(
        &mut self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let hprof_view = self.hprof_analysis_view(context);
        div()
            .id("hprof-analysis-page")
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(self.palette().background))
            .child(self.render_hprof_action_bar(context))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(hprof_view),
            )
    }

    /// 渲染 HPROF 页顶部操作栏。
    pub(in crate::app) fn render_hprof_action_bar(
        &self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let palette = self.palette();
        div()
            .id("hprof-action-bar")
            .flex()
            .items_center()
            .h(px(TOOLBAR_HEIGHT))
            .pl(px(LOG_TREE_ROW_HORIZONTAL_PADDING))
            .pr_4()
            .bg(rgb(palette.panel))
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(self.render_hprof_select_file_button(context))
    }

    /// 渲染 HPROF 文件选择按钮。
    ///
    /// 业务意图：
    /// - HPROF 页内选择文件后复用当前内嵌分析视图启动后台解析，不再打开新的分析窗口。
    pub(in crate::app) fn render_hprof_select_file_button(
        &self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let palette = self.palette();
        div()
            .id("hprof-select-file-button")
            .flex()
            .items_center()
            .gap_1()
            .flex_none()
            .pl(px(0.0))
            .pr(px(TOOLBAR_BUTTON_HORIZONTAL_PADDING))
            .py(px(TOOLBAR_BUTTON_VERTICAL_PADDING))
            .text_sm()
            .text_color(rgb(palette.text))
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(move |button| button.text_color(rgb(palette.accent)))
            .active(|button| button.opacity(0.82))
            .child(Self::render_lucide_icon(
                Some(Icon::ChartNoAxesCombined),
                TOOLBAR_BUTTON_ICON_WIDTH,
                TOOLBAR_BUTTON_ICON_SIZE,
                palette.muted_text,
            ))
            .child("选择 HPROF 文件")
            .on_click(context.listener(Self::open_hprof_from_toolbar))
    }

    /// 渲染左右分栏内容区域。
    ///
    /// 业务意图：
    /// - 未加载日志时主内容区占满剩余空间并显示友好提示，不出现空的左侧目录区域。
    /// - 加载完成后切换为左右两栏，左侧展示目录树，右侧展示日志 tab 工作区或点击文件提示。
    ///
    /// 边界条件：
    /// - 当前只有加载完成状态显示可拖动分割线；未加载、加载中或失败状态不显示分割线。
    /// - 没有打开任何日志 tab 时，右侧只显示下一步提示，不创建空白占位 tab。
    /// - 当前分割宽度仅在内存中生效，不跨启动保存。
    pub(in crate::app) fn render_content(&self, context: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette();
        if !matches!(self.log.load_state, LogTreeLoadState::Loaded(_)) {
            return div()
                .id("log-content-empty-state")
                .flex()
                .flex_1()
                .size_full()
                .bg(rgb(palette.background))
                .child(self.render_primary_content_message());
        }

        if self.should_hide_log_tree_panel() {
            return div()
                .id("log-content-single-log-view")
                .flex()
                .flex_1()
                .size_full()
                .bg(rgb(palette.background))
                // 单文件模式隐藏了左侧目录树和分割线，但右侧日志正文仍使用自绘滚动条。
                // 拖动滑块时后续鼠标移动可能落在正文空白处，必须和分栏模式一样由内容容器统一续传和清理拖动状态。
                .on_mouse_move(context.listener(Self::handle_content_mouse_move))
                .on_mouse_up(
                    MouseButton::Left,
                    context.listener(Self::handle_content_mouse_up),
                )
                .on_mouse_up_out(
                    MouseButton::Left,
                    context.listener(Self::handle_content_mouse_up),
                )
                .child(
                    div()
                        .id("right-log-panel-single-log")
                        .relative()
                        .h_full()
                        .flex_1()
                        .bg(rgb(palette.background))
                        .overflow_hidden()
                        .child(self.render_right_log_panel(context)),
                );
        }

        div()
            .id("log-content-split-view")
            .flex()
            .flex_1()
            .size_full()
            .bg(rgb(palette.background))
            .on_mouse_move(context.listener(Self::handle_content_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                context.listener(Self::handle_content_mouse_up),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                context.listener(Self::handle_content_mouse_up),
            )
            .child(
                div()
                    .id("left-log-panel")
                    .h_full()
                    .w(px(self.left_panel_width))
                    .flex_none()
                    .overflow_hidden()
                    .child(self.render_log_tree_panel(context)),
            )
            .child(self.render_splitter(context))
            .child(
                div()
                    .id("right-log-panel")
                    .relative()
                    .h_full()
                    .flex_1()
                    .bg(rgb(palette.background))
                    .overflow_hidden()
                    .child(self.render_right_log_panel(context))
                    .child(self.render_splitter_hit_overlay(context)),
            )
    }

    /// 判断当前加载结果是否应隐藏左侧目录树。
    ///
    /// 业务意图：
    /// - 当用户加载的内容最终只有一个可打开日志时，左侧树没有选择价值，应把完整空间留给日志浏览。
    /// - 多文件目录或压缩包仍显示左侧树，便于用户选择、右键保存和线程分析。
    pub(in crate::app) fn should_hide_log_tree_panel(&self) -> bool {
        matches!(
            &self.log.load_state,
            LogTreeLoadState::Loaded(tree_state) if tree_state.single_log_source().is_some()
        )
    }

    /// 返回右侧日志工作区相对主窗口左侧的横向偏移。
    ///
    /// 业务意图：
    /// - 右键菜单和编码下拉菜单由窗口坐标转换到右侧面板局部坐标，必须和当前布局是否隐藏左侧树保持一致。
    /// - 主窗口最左侧现在固定 56px 大导航，因此所有日志页内部弹层都必须先扣除导航宽度。
    /// - 单日志模式没有左侧树和分割线，只扣除导航宽度；多文件模式继续额外扣除左侧树宽度和分割线宽度。
    pub(in crate::app) fn right_panel_left_offset(&self) -> f32 {
        Self::right_panel_left_offset_for_layout(
            self.should_hide_log_tree_panel(),
            self.left_panel_width,
        )
    }

    /// 按布局状态计算右侧日志工作区相对主窗口左侧的横向偏移。
    ///
    /// 业务意图：
    /// - 将坐标规则拆成纯函数，便于测试固定导航竖条加入后各类日志弹层不会整体向左错位。
    pub(in crate::app) fn right_panel_left_offset_for_layout(
        hide_log_tree_panel: bool,
        left_panel_width: f32,
    ) -> f32 {
        if hide_log_tree_panel {
            MAIN_NAV_WIDTH
        } else {
            MAIN_NAV_WIDTH + left_panel_width + SPLITTER_VISIBLE_WIDTH
        }
    }

    /// 计算左侧目录树右键菜单在目录树面板内的横坐标。
    ///
    /// 业务意图：
    /// - 鼠标事件使用主窗口坐标，而菜单渲染在目录树面板内部；固定大导航宽度必须在进入面板坐标前扣除。
    pub(in crate::app) fn log_tree_context_menu_x(window_x: f32, left_panel_width: f32) -> f32 {
        (window_x - MAIN_NAV_WIDTH)
            .max(0.0)
            .clamp(0.0, left_panel_width - LOG_TREE_CONTEXT_MENU_WIDTH)
    }

    /// 渲染左右两栏之间的可拖动分割线。
    ///
    /// 业务意图：
    /// - 分割线提供明确的拖动命中区域，让用户可以动态调整左右区域宽度。
    /// - 可见部分保持 1px 细线，透明命中区由右侧内容覆盖层提供，避免右侧出现白色拖动间隔。
    ///
    /// 边界条件：
    /// - 当前只支持水平拖动，不能双击重置，也不能通过键盘调整。
    /// - 该元素只承担视觉线条和左侧边界拖动；右侧扩展命中区由 `render_splitter_hit_overlay` 提供。
    pub(in crate::app) fn render_splitter(&self, context: &mut Context<Self>) -> impl IntoElement {
        // 拖动中使用略深的线条颜色，提供清晰反馈；非拖动状态保持轻量，避免抢占内容注意力。
        let line_color = if self.is_resizing_splitter {
            0x94a3b8
        } else {
            self.palette().border
        };

        div()
            .id("content-splitter")
            .flex()
            .items_center()
            .justify_center()
            .h_full()
            .w(px(SPLITTER_VISIBLE_WIDTH))
            .flex_none()
            .cursor_col_resize()
            .bg(rgb(line_color))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(Self::start_resizing_splitter),
            )
    }

    /// 渲染另存为同名文件确认弹窗。
    ///
    /// 业务意图：
    /// - 目标目录已有同名文件时，必须先让用户在“跳过”和“覆盖”之间明确选择，避免后台任务静默覆盖用户文件。
    ///
    /// 边界条件：
    /// - 弹窗作为主窗口内模态层绘制，遮挡底层日志区域并拦截鼠标事件，防止用户在确认前继续触发其它操作。
    pub(in crate::app) fn render_save_overwrite_confirm_dialog(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(dialog) = &self.log.save_overwrite_confirm_dialog else {
            return div().id("save-overwrite-confirm-empty").hidden();
        };
        let palette = self.palette();
        let mut backdrop = rgb(0x000000);
        backdrop.a = 0.34;

        div()
            .id("save-overwrite-confirm")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .flex()
            .items_center()
            .justify_center()
            .bg(backdrop)
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .child(
                div()
                    .w(px(420.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.surface))
                    .shadow_lg()
                    .p_4()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child("目标目录已有同名文件"),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_sm()
                            .line_height(px(20.0))
                            .text_color(rgb(palette.muted_text))
                            .child(format!(
                                "发现 {} 个目标文件已存在。请选择跳过这些文件，或覆盖目标目录中的同名文件。",
                                dialog.conflict_count
                            )),
                    )
                    .child(
                        div()
                            .mt_2()
                            .px_2()
                            .py_1()
                            .rounded(px(5.0))
                            .bg(rgb(palette.input))
                            .text_xs()
                            .text_color(rgb(palette.muted_text))
                            .child(format!("示例：{}", dialog.first_conflict_path.display())),
                    )
                    .child(
                        div()
                            .mt_4()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(self.render_save_overwrite_button(
                                "跳过",
                                SaveConflictPolicy::SkipExisting,
                                false,
                                palette,
                                context,
                            ))
                            .child(self.render_save_overwrite_button(
                                "覆盖",
                                SaveConflictPolicy::OverwriteExisting,
                                true,
                                palette,
                                context,
                            )),
                    ),
            )
    }

    /// 渲染另存为冲突确认按钮。
    ///
    /// 业务意图：
    /// - “跳过”是保守操作，“覆盖”是破坏性操作；通过不同视觉权重帮助用户理解风险。
    pub(in crate::app) fn render_save_overwrite_button(
        &self,
        label: &'static str,
        conflict_policy: SaveConflictPolicy,
        primary: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!("save-overwrite-{label}")))
            .flex()
            .items_center()
            .justify_center()
            .h(px(30.0))
            .px_4()
            .rounded(px(5.0))
            .border_1()
            .border_color(rgb(if primary {
                palette.accent
            } else {
                palette.border
            }))
            .bg(rgb(if primary {
                palette.accent
            } else {
                palette.panel
            }))
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(if primary {
                palette.on_accent
            } else {
                palette.text
            }))
            .cursor_pointer()
            .hover(move |button| {
                button.bg(rgb(if primary {
                    palette.accent_hover
                } else {
                    palette.hover
                }))
            })
            .child(label)
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.handle_save_overwrite_choice(conflict_policy, context);
                }),
            )
    }
}
