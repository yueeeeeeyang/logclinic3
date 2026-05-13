// 设置独立窗口实现。
//
// 业务意图：
// - 该文件集中维护“通用/日志/模型”页签、主题选择、日志显示字号、日志分析过滤、快搜关键字和模型配置设置，避免设置 UI 继续堆在应用根文件里。
// - 当前作为 `app/ui` 的视图模块，通过显式 `pub(in crate::app)` 接口更新 `MainView` 的会话状态和持久化配置。
//
// 边界条件：
// - 设置窗口承载会写入应用配置目录的偏好，输入类设置必须显式处理只读、编辑和保存状态，避免误触改变跨会话配置。

use super::*;

pub(in crate::app) struct SettingsWindowView {
    /// 主窗口视图实体。
    ///
    /// 业务意图：
    /// - 设置窗口需要读写主窗口中的设置状态；状态放在主视图中可以在关闭再打开设置窗口后保留当前会话选择。
    pub(in crate::app) main_view: Entity<MainView>,
    /// 主窗口状态变更订阅。
    ///
    /// 业务意图：
    /// - 如果设置状态被其它入口更新，独立设置窗口需要跟随重绘；订阅句柄必须保存在视图中防止释放。
    _main_view_subscription: gpui::Subscription,
}

/// 设置内容区渲染所需的主视图快照。
///
/// 业务意图：
/// - 设置窗口渲染需要从主视图读取多个偏好和焦点句柄；集中成快照后，内容渲染函数只接收一个稳定输入。
/// - 快照只存在于单次渲染，不写入配置，也不改变设置保存时机。
struct SettingsContentSnapshot {
    /// 当前设置页签。
    active_tab: SettingsTab,
    /// 主题偏好。
    theme: ThemePreference,
    /// 日志正文字号。
    log_viewer_font_size: f32,
    /// 系统右键菜单集成状态。
    shell_integration_state: ShellIntegrationUiState,
    /// 快搜关键字是否处于编辑态。
    quick_search_keywords_is_editing: bool,
    /// 快搜关键字输入焦点。
    quick_search_keywords_focus: gpui::FocusHandle,
    /// 线程过滤规则是否处于编辑态。
    thread_analysis_filter_is_editing: bool,
    /// 线程过滤规则输入焦点。
    thread_analysis_filter_focus: gpui::FocusHandle,
    /// 当前主题色板。
    palette: AppThemePalette,
}

/// 模型配置工具按钮的渲染参数。
///
/// 业务意图：
/// - 模型页多个按钮共享同一视觉样式，参数对象避免布尔和图标位置参数误传。
pub(in crate::app) struct ModelIconButtonRequest {
    /// 元素稳定 ID。
    pub(in crate::app) id: &'static str,
    /// 按钮文字。
    pub(in crate::app) label: &'static str,
    /// 按钮图标。
    pub(in crate::app) icon: Icon,
    /// 是否可点击。
    pub(in crate::app) enabled: bool,
    /// 是否使用主按钮样式。
    pub(in crate::app) primary: bool,
    /// 当前主题色板。
    pub(in crate::app) palette: AppThemePalette,
}

impl SettingsWindowView {
    /// 创建设置窗口根视图。
    pub(in crate::app) fn new(main_view: Entity<MainView>, context: &mut Context<Self>) -> Self {
        let observed_main_view = main_view.clone();
        let main_view_subscription = context.observe(&observed_main_view, |_, _, context| {
            context.notify();
        });

        let view = Self {
            main_view,
            _main_view_subscription: main_view_subscription,
        };
        view.refresh_shell_integration_status(context);
        view
    }

    /// 切换设置窗口页签。
    ///
    /// 业务意图：
    /// - 页签状态保存在 `MainView`，让窗口关闭后再次打开仍停留在当前会话最后访问的页签。
    pub(in crate::app) fn select_tab(&mut self, tab: SettingsTab, context: &mut Context<Self>) {
        self.main_view.update(context, |view, context| {
            view.settings.settings_active_tab = tab;
            context.notify();
        });
        context.notify();
    }

    /// 切换主题选项。
    ///
    /// 业务意图：
    /// - 用户在设置窗口中选择主题后应立即影响所有已打开窗口，并写入配置供下次启动恢复。
    pub(in crate::app) fn select_theme(
        &mut self,
        theme: ThemePreference,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.settings.theme_preference = theme;
            save_theme_preference(theme);
            context.notify();
        });
        context.notify();
    }

    /// 调整日志显示字号。
    ///
    /// 业务意图：
    /// - 用户在通用设置中点击加减按钮后，应立即刷新日志正文和搜索结果预览，并写入配置供下次启动恢复。
    ///
    /// 边界条件：
    /// - 调整结果超出允许范围时不写入，避免设置按钮或损坏状态把日志字号推到不可读或挤破行高的值。
    pub(in crate::app) fn adjust_log_viewer_font_size(
        &mut self,
        delta: f32,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            let target = view.settings.log_viewer_font_size + delta;
            if let Some(font_size) = normalize_log_viewer_font_size(target) {
                view.settings.log_viewer_font_size = font_size;
                save_log_viewer_font_size_preference(font_size);
                context.notify();
            }
        });
        context.notify();
    }

    /// 后台刷新系统右键菜单集成状态。
    ///
    /// 业务意图：
    /// - 注册表和 LaunchServices 查询都可能访问系统服务，设置窗口打开时不能阻塞 UI 绘制。
    /// - 查询结果写回主视图设置状态，独立设置窗口通过订阅自动重绘。
    pub(in crate::app) fn refresh_shell_integration_status(&self, context: &mut Context<Self>) {
        let main_view = self.main_view.clone();
        self.main_view.update(context, |view, context| {
            view.settings.shell_integration_state = ShellIntegrationUiState::Checking;
            context.notify();
        });
        context
            .spawn(async move |_settings_view, app| {
                let status = app
                    .background_executor()
                    .spawn(async { query_shell_integration_status() })
                    .await;
                app.update(move |app| {
                    main_view.update(app, |view, context| {
                        view.settings.shell_integration_state =
                            ShellIntegrationUiState::Ready(status);
                        context.notify();
                    });
                })
                .ok();
            })
            .detach();
    }

    /// 后台注册系统右键菜单入口。
    ///
    /// 业务意图：
    /// - 用户点击注册后应立即看到执行中反馈；平台注册动作完成后再刷新为真实状态。
    pub(in crate::app) fn register_shell_integration_from_settings(
        &self,
        context: &mut Context<Self>,
    ) {
        let main_view = self.main_view.clone();
        self.main_view.update(context, |view, context| {
            view.settings.shell_integration_state = ShellIntegrationUiState::Registering;
            context.notify();
        });
        context
            .spawn(async move |_settings_view, app| {
                let result = app
                    .background_executor()
                    .spawn(async { register_shell_integration() })
                    .await;
                app.update(move |app| {
                    main_view.update(app, |view, context| {
                        view.settings.shell_integration_state = match result {
                            Ok(status) => ShellIntegrationUiState::Ready(status),
                            Err(message) => ShellIntegrationUiState::Failed(message),
                        };
                        context.notify();
                    });
                })
                .ok();
            })
            .detach();
    }

    /// 后台卸载系统右键菜单入口。
    ///
    /// 业务意图：
    /// - 卸载同样访问平台服务，必须和注册一样进入后台，避免设置窗口按钮卡住。
    pub(in crate::app) fn unregister_shell_integration_from_settings(
        &self,
        context: &mut Context<Self>,
    ) {
        let main_view = self.main_view.clone();
        self.main_view.update(context, |view, context| {
            view.settings.shell_integration_state = ShellIntegrationUiState::Unregistering;
            context.notify();
        });
        context
            .spawn(async move |_settings_view, app| {
                let result = app
                    .background_executor()
                    .spawn(async { unregister_shell_integration() })
                    .await;
                app.update(move |app| {
                    main_view.update(app, |view, context| {
                        view.settings.shell_integration_state = match result {
                            Ok(status) => ShellIntegrationUiState::Ready(status),
                            Err(message) => ShellIntegrationUiState::Failed(message),
                        };
                        context.notify();
                    });
                })
                .ok();
            })
            .detach();
    }

    /// 处理线程日志分析过滤输入区键盘编辑。
    pub(in crate::app) fn handle_thread_analysis_filter_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.handle_thread_analysis_filter_key_down(event, context);
        });
        context.notify();
    }

    /// 处理线程日志分析过滤输入区鼠标按下。
    pub(in crate::app) fn handle_thread_analysis_filter_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let focus_handle = self.main_view.update(context, |view, context| {
            view.start_thread_analysis_filter_mouse_selection(event, context);
            view.settings.thread_analysis_filter_focus.clone()
        });
        window.focus(&focus_handle);
        context.notify();
    }

    /// 拖动扩展线程日志分析过滤输入区的选择范围。
    pub(in crate::app) fn handle_thread_analysis_filter_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.update_thread_analysis_filter_mouse_selection(event.position, context);
        });
        context.notify();
    }

    /// 结束线程日志分析过滤输入区鼠标选择。
    pub(in crate::app) fn handle_thread_analysis_filter_mouse_up(
        &mut self,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.finish_thread_analysis_filter_mouse_selection(context);
        });
        context.notify();
    }

    /// 处理线程日志分析过滤的编辑/保存按钮。
    ///
    /// 返回值：
    /// - 进入编辑态时返回输入区焦点句柄，调用方负责把焦点交给多行输入区。
    /// - 保存时返回 `None`，避免保存按钮点击后再次抢回只读输入区焦点。
    pub(in crate::app) fn toggle_thread_analysis_filter_editing(
        &mut self,
        context: &mut Context<Self>,
    ) -> Option<gpui::FocusHandle> {
        let focus_handle = self.main_view.update(context, |view, context| {
            if view.settings.thread_analysis_filter_is_editing {
                view.save_thread_analysis_filter_edit(context);
                None
            } else {
                view.begin_thread_analysis_filter_edit(context);
                Some(view.settings.thread_analysis_filter_focus.clone())
            }
        });
        context.notify();
        focus_handle
    }

    /// 处理快搜关键字输入区键盘编辑。
    pub(in crate::app) fn handle_quick_search_keywords_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.handle_quick_search_keywords_key_down(event, context);
        });
        context.notify();
    }

    /// 处理快搜关键字输入区鼠标按下。
    pub(in crate::app) fn handle_quick_search_keywords_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let focus_handle = self.main_view.update(context, |view, context| {
            view.start_quick_search_keywords_mouse_selection(event, context);
            view.settings.quick_search_keywords_focus.clone()
        });
        window.focus(&focus_handle);
        context.notify();
    }

    /// 拖动扩展快搜关键字输入区选择范围。
    pub(in crate::app) fn handle_quick_search_keywords_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.update_quick_search_keywords_mouse_selection(event.position, context);
        });
        context.notify();
    }

    /// 结束快搜关键字输入区鼠标选择。
    pub(in crate::app) fn handle_quick_search_keywords_mouse_up(
        &mut self,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.finish_quick_search_keywords_mouse_selection(context);
        });
        context.notify();
    }

    /// 处理快搜关键字配置的编辑/保存按钮。
    pub(in crate::app) fn toggle_quick_search_keywords_editing(
        &mut self,
        context: &mut Context<Self>,
    ) -> Option<gpui::FocusHandle> {
        let focus_handle = self.main_view.update(context, |view, context| {
            if view.settings.quick_search_keywords_is_editing {
                view.save_quick_search_keywords_edit(context);
                None
            } else {
                view.begin_quick_search_keywords_edit(context);
                Some(view.settings.quick_search_keywords_focus.clone())
            }
        });
        context.notify();
        focus_handle
    }

    /// 处理模型配置输入框键盘编辑。
    pub(in crate::app) fn handle_model_config_input_key_down(
        &mut self,
        kind: ModelConfigInputKind,
        event: &KeyDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.handle_model_config_input_key_down(kind, event, context);
        });
        context.notify();
    }

    /// 处理模型配置输入框鼠标按下并聚焦对应字段。
    pub(in crate::app) fn handle_model_config_input_mouse_down(
        &mut self,
        kind: ModelConfigInputKind,
        event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let focus_handle = self.main_view.update(context, |view, context| {
            view.start_model_config_input_mouse_selection(kind, event, context);
            view.model_config_input_focus(kind)
        });
        window.focus(&focus_handle);
        context.notify();
    }

    /// 拖动扩展模型配置输入框选择范围。
    pub(in crate::app) fn handle_model_config_input_mouse_move(
        &mut self,
        kind: ModelConfigInputKind,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.update_model_config_input_mouse_selection(kind, event.position, context);
        });
        context.notify();
    }

    /// 结束模型配置输入框鼠标选择。
    pub(in crate::app) fn handle_model_config_input_mouse_up(
        &mut self,
        kind: ModelConfigInputKind,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.finish_model_config_input_mouse_selection(kind, context);
        });
        context.notify();
    }

    /// 渲染左侧页签栏。
    fn render_tab_sidebar(
        &self,
        active_tab: SettingsTab,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .w(px(SETTINGS_TAB_SIDEBAR_WIDTH))
            .h_full()
            .p_2()
            .border_r_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .children(
                SettingsTab::all()
                    .iter()
                    .copied()
                    .map(|tab| self.render_tab_button(tab, active_tab, palette, context)),
            )
    }

    /// 渲染单个设置页签按钮。
    fn render_tab_button(
        &self,
        tab: SettingsTab,
        active_tab: SettingsTab,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let selected = tab == active_tab;
        div()
            .id(SharedString::from(format!("settings-tab-{}", tab.label())))
            .flex()
            .items_center()
            .gap_2()
            .h(px(34.0))
            .px_2()
            .mb_1()
            .rounded(px(6.0))
            .text_sm()
            .font_weight(if selected {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::NORMAL
            })
            .text_color(rgb(if selected {
                palette.accent
            } else {
                palette.muted_text
            }))
            .bg(rgb(if selected {
                palette.selected
            } else {
                palette.panel
            }))
            .cursor_pointer()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .child(MainView::render_lucide_icon(
                Some(tab.icon()),
                16.0,
                15.0,
                if selected {
                    palette.accent
                } else {
                    palette.muted_text
                },
            ))
            .child(tab.label())
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.select_tab(tab, context);
                }),
            )
    }

    /// 渲染设置内容区域。
    ///
    /// 业务意图：
    /// - 设置页内容来自主视图快照和当前窗口上下文，保持显式参数能避免渲染阶段重新借用主视图状态。
    fn render_content(
        &self,
        snapshot: SettingsContentSnapshot,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let SettingsContentSnapshot {
            active_tab,
            theme,
            log_viewer_font_size,
            shell_integration_state,
            quick_search_keywords_is_editing,
            quick_search_keywords_focus,
            thread_analysis_filter_is_editing,
            thread_analysis_filter_focus,
            palette,
        } = snapshot;
        match active_tab {
            SettingsTab::General => self.render_general_tab(
                theme,
                log_viewer_font_size,
                &shell_integration_state,
                palette,
                context,
            ),
            SettingsTab::Log => self.render_log_tab(
                quick_search_keywords_is_editing,
                quick_search_keywords_focus,
                thread_analysis_filter_is_editing,
                thread_analysis_filter_focus,
                palette,
                context,
            ),
            SettingsTab::Model => self.render_model_tab(palette, context),
            SettingsTab::About => super::about_view::render_about_settings_tab(palette),
        }
    }
}

impl Render for SettingsWindowView {
    /// 渲染独立设置窗口。
    ///
    /// 业务意图：
    /// - 设置窗口内容由左侧页签和右侧内容组成，根节点填满独立窗口，避免系统标题栏下方出现未绘制区域。
    fn render(&mut self, _window: &mut Window, context: &mut Context<Self>) -> impl IntoElement {
        let (
            active_tab,
            theme,
            log_viewer_font_size,
            shell_integration_state,
            quick_search_keywords_is_editing,
            quick_search_keywords_focus,
            thread_analysis_filter_is_editing,
            thread_analysis_filter_focus,
            palette,
        ) = {
            let main_view = self.main_view.read(context);
            (
                main_view.settings.settings_active_tab,
                main_view.settings.theme_preference,
                main_view.settings.log_viewer_font_size,
                main_view.settings.shell_integration_state.clone(),
                main_view.settings.quick_search_keywords_is_editing,
                main_view.settings.quick_search_keywords_focus.clone(),
                main_view.settings.thread_analysis_filter_is_editing,
                main_view.settings.thread_analysis_filter_focus.clone(),
                main_view.palette(),
            )
        };

        div()
            .id("settings-window")
            .flex()
            .size_full()
            .bg(rgb(palette.background))
            .child(self.render_tab_sidebar(active_tab, palette, context))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(self.render_content(
                        SettingsContentSnapshot {
                            active_tab,
                            theme,
                            log_viewer_font_size,
                            shell_integration_state,
                            quick_search_keywords_is_editing,
                            quick_search_keywords_focus,
                            thread_analysis_filter_is_editing,
                            thread_analysis_filter_focus,
                            palette,
                        },
                        context,
                    )),
            )
    }
}
