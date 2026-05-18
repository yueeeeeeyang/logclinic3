// 设置窗口插件页签渲染和管理动作。
//
// 业务意图：
// - 插件页负责管理 JSON 注册表中的开发目录插件和 ZIP 安装插件启用状态。
// - 该页只读取 `plugin.json` 元数据、安装包结构和入口命令，不执行插件业务命令，避免打开设置页触发第三方代码。
//
// 边界条件：
// - ZIP 导入在后台执行，防止解压大包或校验路径时阻塞 GPUI 线程。
// - 卸载 ZIP 插件会删除配置目录中的安装副本；开发目录插件只移除注册记录，不删除用户原始目录。

use super::*;

impl SettingsWindowView {
    /// 渲染插件设置页签。
    ///
    /// 业务意图：
    /// - 让用户在设置页集中查看插件加载结果、来源、权限和贡献点，并执行加载、禁用、重载、卸载等管理动作。
    pub(in crate::app) fn render_plugin_tab(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let plugins = {
            let main_view = self.main_view.read(context);
            main_view.plugins.definitions.clone()
        };

        div()
            .id("settings-plugin-tab")
            .flex()
            .flex_col()
            .size_full()
            .p_4()
            .gap_3()
            .overflow_y_scroll()
            .scrollbar_width(px(6.0))
            .bg(rgb(palette.background))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child(MainView::render_lucide_icon(
                                Some(Icon::FileArchive),
                                16.0,
                                16.0,
                                palette.muted_text,
                            ))
                            .child("插件管理"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(self.render_plugin_action_button(
                                "settings-plugin-load-dir",
                                "加载目录",
                                Icon::FolderOpen,
                                true,
                                false,
                                palette,
                                context,
                                |view, context| view.begin_load_plugin_directory(context),
                            ))
                            .child(self.render_plugin_action_button(
                                "settings-plugin-load-zip",
                                "加载 ZIP",
                                Icon::FileArchive,
                                true,
                                true,
                                palette,
                                context,
                                |view, context| view.begin_load_plugin_zip(context),
                            )),
                    ),
            )
            .when(plugins.is_empty(), |body| {
                body.child(
                    div()
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(rgb(palette.border))
                        .bg(rgb(palette.surface))
                        .p_4()
                        .text_sm()
                        .text_color(rgb(palette.muted_text))
                        .child("当前没有插件"),
                )
            })
            .children(
                plugins
                    .into_iter()
                    .map(|plugin| self.render_plugin_item(plugin, palette, context)),
            )
    }

    /// 渲染单个插件条目。
    fn render_plugin_item(
        &self,
        plugin: PluginDefinition,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let enabled = plugin.enabled;
        let can_uninstall = true;
        let can_open_path = plugin.root_path.is_some();
        let permissions = plugin
            .manifest
            .as_ref()
            .map(|manifest| manifest.permissions.join("、"))
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "无".to_string());
        let contributes = plugin
            .manifest
            .as_ref()
            .map(Self::plugin_contribution_summary)
            .unwrap_or_else(|| "无可用贡献点".to_string());
        let status_text = if let Some(error) = plugin.load_error.as_ref() {
            format!("加载失败：{error}")
        } else if plugin.enabled {
            "已启用".to_string()
        } else {
            "已禁用".to_string()
        };
        let plugin_id = plugin.id.clone();

        div()
            .id(SharedString::from(format!("settings-plugin-{}", plugin.id)))
            .flex()
            .flex_col()
            .gap_3()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .p_4()
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(MainView::render_lucide_icon(
                                        Some(Icon::FileArchive),
                                        16.0,
                                        16.0,
                                        palette.muted_text,
                                    ))
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(palette.text))
                                            .child(format!("{}  {}", plugin.name, plugin.version)),
                                    ),
                            )
                            .child(div().text_xs().text_color(rgb(palette.muted_text)).child(
                                format!(
                                    "{} · {} · {}",
                                    plugin.id,
                                    plugin.source.label(),
                                    status_text
                                ),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(self.render_plugin_action_button(
                                SharedString::from(format!("settings-plugin-toggle-{plugin_id}")),
                                if enabled { "禁用" } else { "启用" },
                                if enabled { Icon::X } else { Icon::Check },
                                true,
                                false,
                                palette,
                                context,
                                {
                                    let plugin_id = plugin.id.clone();
                                    move |view, context| {
                                        view.set_plugin_enabled_from_settings(
                                            plugin_id.clone(),
                                            !enabled,
                                            context,
                                        )
                                    }
                                },
                            ))
                            .child(self.render_plugin_action_button(
                                SharedString::from(format!("settings-plugin-reload-{plugin_id}")),
                                "重载",
                                Icon::RefreshCw,
                                true,
                                false,
                                palette,
                                context,
                                |view, context| view.reload_plugins_from_settings(context),
                            ))
                            .child(self.render_plugin_action_button(
                                SharedString::from(format!("settings-plugin-open-{plugin_id}")),
                                "目录",
                                Icon::FolderOpen,
                                can_open_path,
                                false,
                                palette,
                                context,
                                {
                                    let plugin_id = plugin.id.clone();
                                    move |view, context| {
                                        view.open_plugin_directory_from_settings(
                                            plugin_id.clone(),
                                            context,
                                        )
                                    }
                                },
                            ))
                            .child(self.render_plugin_action_button(
                                SharedString::from(format!(
                                    "settings-plugin-uninstall-{plugin_id}"
                                )),
                                "卸载",
                                Icon::Trash2,
                                can_uninstall,
                                false,
                                palette,
                                context,
                                {
                                    let plugin_id = plugin.id.clone();
                                    move |view, context| {
                                        view.uninstall_plugin_from_settings(
                                            plugin_id.clone(),
                                            context,
                                        )
                                    }
                                },
                            )),
                    ),
            )
            .child(self.render_plugin_detail_row("路径", plugin.display_path(), palette))
            .child(self.render_plugin_detail_row("权限", permissions, palette))
            .child(self.render_plugin_detail_row("贡献点", contributes, palette))
    }

    /// 渲染插件条目中的说明行。
    fn render_plugin_detail_row(
        &self,
        label: &'static str,
        value: String,
        palette: AppThemePalette,
    ) -> gpui::Div {
        div()
            .flex()
            .items_start()
            .gap_2()
            .text_xs()
            .child(
                div()
                    .w(px(52.0))
                    .flex_none()
                    .text_color(rgb(palette.muted_text))
                    .child(label),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(rgb(palette.text))
                    .child(value),
            )
    }

    /// 渲染插件页操作按钮。
    fn render_plugin_action_button<F, I>(
        &self,
        id: I,
        label: &'static str,
        icon: Icon,
        enabled: bool,
        primary: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
        handler: F,
    ) -> gpui::Stateful<gpui::Div>
    where
        F: Fn(&mut SettingsWindowView, &mut Context<SettingsWindowView>) + 'static,
        I: Into<ElementId>,
    {
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .h(px(28.0))
            .px_2()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(if primary {
                palette.accent
            } else {
                palette.border
            }))
            .bg(rgb(if primary {
                palette.selected
            } else {
                palette.input
            }))
            .text_xs()
            .text_color(rgb(if enabled {
                if primary {
                    palette.accent
                } else {
                    palette.text
                }
            } else {
                palette.muted_text
            }))
            .opacity(if enabled { 1.0 } else { 0.5 })
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |button| button.bg(rgb(palette.hover)))
            })
            .child(MainView::render_lucide_icon(
                Some(icon),
                14.0,
                14.0,
                if primary {
                    palette.accent
                } else {
                    palette.muted_text
                },
            ))
            .child(label)
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    if enabled {
                        handler(view, context);
                    }
                    context.stop_propagation();
                }),
            )
    }

    /// 返回插件贡献点摘要。
    fn plugin_contribution_summary(manifest: &PluginManifest) -> String {
        let mut parts = Vec::new();
        if !manifest.contributes.navigation.is_empty() {
            parts.push(format!("导航页 {}", manifest.contributes.navigation.len()));
        }
        if !manifest.contributes.log_tree_context_menu.is_empty() {
            parts.push(format!(
                "日志树菜单 {}",
                manifest.contributes.log_tree_context_menu.len()
            ));
        }
        if !manifest.contributes.notes_tree_context_menu.is_empty() {
            parts.push(format!(
                "笔记树菜单 {}",
                manifest.contributes.notes_tree_context_menu.len()
            ));
        }
        if parts.is_empty() {
            "无".to_string()
        } else {
            parts.join("、")
        }
    }

    /// 打开系统选择器加载开发目录插件。
    pub(in crate::app) fn begin_load_plugin_directory(&self, context: &mut Context<Self>) {
        self.begin_load_plugin_path(
            PathPromptOptions {
                files: false,
                directories: true,
                multiple: false,
                prompt: Some("选择插件目录".into()),
            },
            PluginPathLoadKind::DevelopmentDirectory,
            context,
        );
    }

    /// 打开系统选择器加载 ZIP 插件。
    pub(in crate::app) fn begin_load_plugin_zip(&self, context: &mut Context<Self>) {
        self.begin_load_plugin_path(
            PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: Some("选择插件 ZIP".into()),
            },
            PluginPathLoadKind::ZipPackage,
            context,
        );
    }

    /// 打开系统路径选择器并在后台导入插件。
    fn begin_load_plugin_path(
        &self,
        options: PathPromptOptions,
        load_kind: PluginPathLoadKind,
        context: &mut Context<Self>,
    ) {
        let main_view = self.main_view.clone();
        let registry = self.main_view.read(context).plugins.registry.clone();
        context
            .spawn(async move |_settings_view, app| {
                let receiver = match app.update(|app| app.prompt_for_paths(options)) {
                    Ok(receiver) => receiver,
                    Err(error) => {
                        main_view
                            .update(app, |view, context| {
                                view.plugins.status_message =
                                    Some(format!("无法打开插件路径选择器：{error}"));
                                context.notify();
                            })
                            .ok();
                        return;
                    }
                };
                let selected_path = match receiver.await {
                    Ok(Ok(Some(paths))) => paths.into_iter().next(),
                    Ok(Ok(None)) => None,
                    Ok(Err(error)) => {
                        main_view
                            .update(app, |view, context| {
                                view.plugins.status_message =
                                    Some(format!("插件路径选择器返回错误：{error}"));
                                context.notify();
                            })
                            .ok();
                        return;
                    }
                    Err(error) => {
                        main_view
                            .update(app, |view, context| {
                                view.plugins.status_message =
                                    Some(format!("插件路径选择器被中断：{error}"));
                                context.notify();
                            })
                            .ok();
                        return;
                    }
                };
                let Some(selected_path) = selected_path else {
                    return;
                };
                let result = app
                    .background_executor()
                    .spawn(async move {
                        let mut registry = registry;
                        let message = match load_kind {
                            PluginPathLoadKind::DevelopmentDirectory => {
                                let entry = register_development_plugin_directory(
                                    &mut registry,
                                    &selected_path,
                                )?;
                                save_plugin_registry(&registry)?;
                                format!("已加载开发插件目录：{}", entry.id)
                            }
                            PluginPathLoadKind::ZipPackage => {
                                if selected_path
                                    .extension()
                                    .and_then(|extension| extension.to_str())
                                    .map_or(true, |extension| {
                                        !extension.eq_ignore_ascii_case("zip")
                                    })
                                {
                                    return Err("请选择 .zip 格式的插件安装包".to_string());
                                }
                                let entry = install_plugin_zip(&mut registry, &selected_path)?;
                                save_plugin_registry(&registry)?;
                                format!("已安装插件 ZIP：{}", entry.id)
                            }
                        };
                        Ok::<_, String>((registry, message))
                    })
                    .await;
                app.update(move |app| {
                    main_view.update(app, |view, context| {
                        match result {
                            Ok((registry, message)) => {
                                view.plugins.registry = registry;
                                view.plugins.reload_definitions();
                                view.plugins.status_message = Some(message);
                            }
                            Err(error) => {
                                view.plugins.status_message =
                                    Some(format!("加载插件失败：{error}"));
                            }
                        }
                        context.notify();
                    });
                })
                .ok();
            })
            .detach();
    }

    /// 启用或禁用插件。
    pub(in crate::app) fn set_plugin_enabled_from_settings(
        &mut self,
        plugin_id: String,
        enabled: bool,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            let Some(definition) = view
                .plugins
                .definitions
                .iter()
                .find(|definition| definition.id == plugin_id)
                .cloned()
            else {
                view.plugins.status_message = Some("插件不存在，无法修改启用状态".to_string());
                context.notify();
                return;
            };
            view.plugins.registry.upsert(PluginRegistryEntry {
                id: definition.id.clone(),
                source: definition.source,
                path: definition.root_path.clone(),
                enabled,
            });
            match save_plugin_registry(&view.plugins.registry) {
                Ok(()) => {
                    view.plugins.reload_definitions();
                    view.plugins.status_message = Some(format!(
                        "{}：{}",
                        if enabled {
                            "已启用插件"
                        } else {
                            "已禁用插件"
                        },
                        definition.name
                    ));
                }
                Err(error) => {
                    view.plugins.status_message = Some(format!("保存插件启用状态失败：{error}"));
                }
            }
            context.notify();
        });
        context.notify();
    }

    /// 重读插件注册表和所有 manifest。
    pub(in crate::app) fn reload_plugins_from_settings(&mut self, context: &mut Context<Self>) {
        self.main_view.update(context, |view, context| {
            view.plugins.registry = load_plugin_registry();
            view.plugins.reload_definitions();
            view.plugins.status_message = Some("已重新加载插件列表".to_string());
            context.notify();
        });
        context.notify();
    }

    /// 卸载插件。
    pub(in crate::app) fn uninstall_plugin_from_settings(
        &mut self,
        plugin_id: String,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            match uninstall_plugin(&mut view.plugins.registry, &plugin_id)
                .and_then(|_| save_plugin_registry(&view.plugins.registry))
            {
                Ok(()) => {
                    view.plugins.reload_definitions();
                    view.plugins.status_message = Some(format!("已卸载插件：{plugin_id}"));
                }
                Err(error) => {
                    view.plugins.status_message = Some(format!("卸载插件失败：{error}"));
                }
            }
            context.notify();
        });
        context.notify();
    }

    /// 使用系统文件管理器打开插件目录。
    pub(in crate::app) fn open_plugin_directory_from_settings(
        &mut self,
        plugin_id: String,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            let Some(path) = view
                .plugins
                .definitions
                .iter()
                .find(|definition| definition.id == plugin_id)
                .and_then(|definition| definition.root_path.clone())
            else {
                view.plugins.status_message = Some("该插件没有可打开的目录".to_string());
                context.notify();
                return;
            };
            match open_directory_in_platform_file_manager(&path) {
                Ok(()) => {
                    view.plugins.status_message =
                        Some(format!("已打开插件目录：{}", path.display()));
                }
                Err(error) => {
                    view.plugins.status_message = Some(format!("打开插件目录失败：{error}"));
                }
            }
            context.notify();
        });
        context.notify();
    }
}

/// 插件路径导入类型。
#[derive(Clone, Copy)]
enum PluginPathLoadKind {
    /// 开发目录引用。
    DevelopmentDirectory,
    /// ZIP 安装包。
    ZipPackage,
}
