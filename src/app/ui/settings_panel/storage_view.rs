// 设置窗口存储页签渲染和扫描动作。
//
// 业务意图：
// - 存储页集中展示 LogClinic 在本机使用的应用配置目录、笔记目录、SQLite 数据库、插件安装目录和大日志临时缓存。
// - 该页只提供安全管理能力：刷新、打开位置、复制路径和清理过期缓存，不删除用户配置、数据库或插件目录。
//
// 边界条件：
// - 目录大小统计可能访问大量文件或遇到权限错误，必须在后台执行，不能阻塞 GPUI 主线程。
// - 路径不存在是正常状态，例如用户尚未打开 AI 对话或未安装插件；这类条目显示“未创建”，不主动创建文件或目录。

use std::collections::BTreeMap;

use crate::{
    ai_chat::ai_chat_database_path,
    archive::{cleanup_stale_large_log_cache, large_log_cache_root, large_log_session_dir},
    notes::notes_root_dir,
};

use super::*;

/// 存储位置静态描述。
///
/// 业务意图：
/// - 路径扫描结果每次都来自文件系统，但条目的名称、用途和分组是稳定产品语义，集中定义可避免 UI 和测试顺序分歧。
#[derive(Clone, Copy)]
pub(in crate::app) struct StorageLocationDescriptor {
    /// 稳定条目 ID。
    pub(in crate::app) id: &'static str,
    /// 展示分组。
    pub(in crate::app) group: &'static str,
    /// 展示名称。
    pub(in crate::app) name: &'static str,
    /// 用途说明。
    pub(in crate::app) purpose: &'static str,
    /// 条目类型。
    pub(in crate::app) kind: StorageLocationKind,
    /// 当前平台路径解析函数。
    pub(in crate::app) path: fn() -> Option<PathBuf>,
}

/// 存储页固定分组顺序。
const STORAGE_GROUPS: &[&str] = &["配置存储", "笔记存储", "数据库存储", "插件存储", "临时缓存"];

/// 返回大日志缓存根目录路径。
fn large_log_cache_root_path() -> Option<PathBuf> {
    Some(large_log_cache_root())
}

/// 返回当前进程大日志缓存 session 目录路径。
fn large_log_session_dir_path() -> Option<PathBuf> {
    Some(large_log_session_dir())
}

/// 存储页固定条目描述。
///
/// 边界条件：
/// - 路径函数只拼接路径，不创建目录；扫描函数负责把不存在展示为“未创建”。
pub(in crate::app) const STORAGE_LOCATION_DESCRIPTORS: &[StorageLocationDescriptor] = &[
    StorageLocationDescriptor {
        id: "config-root",
        group: "配置存储",
        name: "应用配置目录",
        purpose: "LogClinic 用户配置和本地数据的根目录",
        kind: StorageLocationKind::Directory,
        path: app_config_dir,
    },
    StorageLocationDescriptor {
        id: "notes-dir",
        group: "笔记存储",
        name: "笔记目录",
        purpose: "保存真实笔记目录和 Markdown 正文文件",
        kind: StorageLocationKind::Directory,
        path: notes_root_dir,
    },
    StorageLocationDescriptor {
        id: "ai-chat-db",
        group: "数据库存储",
        name: "AI 对话数据库",
        purpose: "保存 AI 对话会话和消息历史",
        kind: StorageLocationKind::Database,
        path: ai_chat_database_path,
    },
    StorageLocationDescriptor {
        id: "plugins-dir",
        group: "插件存储",
        name: "ZIP 插件安装目录",
        purpose: "保存从 ZIP 安装的插件副本",
        kind: StorageLocationKind::Directory,
        path: plugins_install_dir,
    },
    StorageLocationDescriptor {
        id: "large-log-cache-root",
        group: "临时缓存",
        name: "大日志缓存根目录",
        purpose: "保存压缩包内超大日志分页读取所需的临时物化文件",
        kind: StorageLocationKind::Cache,
        path: large_log_cache_root_path,
    },
    StorageLocationDescriptor {
        id: "large-log-cache-session",
        group: "临时缓存",
        name: "当前进程缓存目录",
        purpose: "当前 LogClinic 进程正在使用的大日志临时缓存目录",
        kind: StorageLocationKind::Cache,
        path: large_log_session_dir_path,
    },
];

impl SettingsWindowView {
    /// 确保存储页首次打开时会自动扫描。
    ///
    /// 业务意图：
    /// - 设置窗口可能关闭时停留在存储页，再次打开应直接展示真实状态，而不是空白等待用户手动刷新。
    pub(in crate::app) fn ensure_storage_locations_loaded(&self, context: &mut Context<Self>) {
        let should_refresh = self.main_view.read(context).settings.settings_active_tab
            == SettingsTab::Storage
            && !self.main_view.read(context).settings.storage.loading
            && self
                .main_view
                .read(context)
                .settings
                .storage
                .last_refreshed_at
                .is_none();
        if should_refresh {
            self.refresh_storage_locations_from_settings(context);
        }
    }

    /// 渲染存储设置页签。
    pub(in crate::app) fn render_storage_tab(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let storage = self.main_view.read(context).settings.storage.clone();
        let config_dir_text = app_config_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "当前平台没有可用的应用配置目录".to_string());
        let total_size = storage
            .locations
            .iter()
            .filter_map(|location| location.size_bytes)
            .sum::<u64>();
        let existing_count = storage
            .locations
            .iter()
            .filter(|location| location.exists)
            .count();
        let last_refreshed = storage
            .last_refreshed_at
            .map(format_storage_relative_time)
            .unwrap_or_else(|| "尚未刷新".to_string());

        div()
            .id("settings-storage-tab")
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
                                Some(Icon::Database),
                                16.0,
                                16.0,
                                palette.muted_text,
                            ))
                            .child("存储管理"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(self.render_storage_action_button(
                                "settings-storage-clean-cache",
                                "清理过期缓存",
                                Icon::Trash2,
                                !storage.loading,
                                false,
                                palette,
                                context,
                                |view, context| {
                                    view.cleanup_stale_storage_cache_from_settings(context)
                                },
                            ))
                            .child(self.render_storage_action_button(
                                "settings-storage-refresh",
                                if storage.loading {
                                    "刷新中"
                                } else {
                                    "刷新"
                                },
                                Icon::RefreshCw,
                                !storage.loading,
                                true,
                                palette,
                                context,
                                |view, context| {
                                    view.refresh_storage_locations_from_settings(context)
                                },
                            )),
                    ),
            )
            .child(self.render_storage_summary_card(
                existing_count,
                storage.locations.len(),
                total_size,
                last_refreshed,
                config_dir_text,
                storage.status_message.clone(),
                palette,
            ))
            .when(storage.locations.is_empty(), |body| {
                body.child(
                    div()
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(rgb(palette.border))
                        .bg(rgb(palette.surface))
                        .p_4()
                        .text_sm()
                        .text_color(rgb(palette.muted_text))
                        .child(if storage.loading {
                            "正在扫描存储位置"
                        } else {
                            "点击刷新查看当前存储位置"
                        }),
                )
            })
            .children(group_storage_locations(&storage.locations).into_iter().map(
                |(group, locations)| self.render_storage_group(group, locations, palette, context),
            ))
    }

    /// 渲染存储总览卡片。
    fn render_storage_summary_card(
        &self,
        existing_count: usize,
        total_count: usize,
        total_size: u64,
        last_refreshed: String,
        config_dir_text: String,
        status_message: Option<String>,
        palette: AppThemePalette,
    ) -> gpui::Div {
        div()
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
                    .flex_wrap()
                    .gap_2()
                    .child(self.render_storage_metric(
                        "已创建",
                        format!("{existing_count}/{total_count}"),
                        palette,
                    ))
                    .child(self.render_storage_metric(
                        "总占用",
                        format_storage_size(total_size),
                        palette,
                    ))
                    .child(self.render_storage_metric("最近刷新", last_refreshed, palette)),
            )
            .child(self.render_storage_detail_line("配置目录", config_dir_text, palette))
            .when_some(status_message, |card, message| {
                card.child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette.muted_text))
                        .child(message),
                )
            })
    }

    /// 渲染总览指标。
    fn render_storage_metric(
        &self,
        label: &'static str,
        value: String,
        palette: AppThemePalette,
    ) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(28.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .px_2()
            .text_xs()
            .child(div().text_color(rgb(palette.muted_text)).child(label))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child(value),
            )
    }

    /// 渲染存储分组。
    fn render_storage_group(
        &self,
        group: &'static str,
        locations: Vec<StorageLocationStatus>,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.muted_text))
                    .child(group),
            )
            .children(
                locations
                    .into_iter()
                    .map(|location| self.render_storage_location_item(location, palette, context)),
            )
    }

    /// 渲染单个存储位置。
    fn render_storage_location_item(
        &self,
        location: StorageLocationStatus,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let can_copy = location.path.is_some();
        let can_open = storage_location_file_manager_path(&location).is_some();
        let location_id = location.id;
        let path_text = location
            .path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "当前平台不可用".to_string());
        let size_text = location
            .size_bytes
            .map(format_storage_size)
            .unwrap_or_else(|| "-".to_string());
        let modified_text = location
            .modified
            .map(format_storage_relative_time)
            .unwrap_or_else(|| "-".to_string());

        div()
            .id(SharedString::from(format!(
                "settings-storage-{location_id}"
            )))
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
                                        Some(storage_location_icon(location.kind)),
                                        16.0,
                                        16.0,
                                        palette.muted_text,
                                    ))
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(palette.text))
                                            .child(location.name),
                                    ),
                            )
                            .child(div().text_xs().text_color(rgb(palette.muted_text)).child(
                                format!(
                                    "{} · {} · {}",
                                    location.kind.label(),
                                    location.state_label(),
                                    location.purpose
                                ),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(self.render_storage_action_button(
                                SharedString::from(format!("settings-storage-copy-{location_id}")),
                                "复制路径",
                                Icon::Copy,
                                can_copy,
                                false,
                                palette,
                                context,
                                move |view, context| {
                                    view.copy_storage_location_path(location_id, context)
                                },
                            ))
                            .child(self.render_storage_action_button(
                                SharedString::from(format!("settings-storage-open-{location_id}")),
                                "打开位置",
                                Icon::FolderOpen,
                                can_open,
                                false,
                                palette,
                                context,
                                move |view, context| {
                                    view.open_storage_location_from_settings(location_id, context)
                                },
                            )),
                    ),
            )
            .child(self.render_storage_detail_line("路径", path_text, palette))
            .child(self.render_storage_detail_line("大小", size_text, palette))
            .child(self.render_storage_detail_line("修改", modified_text, palette))
            .when_some(location.error, |item, error| {
                item.child(self.render_storage_detail_line("错误", error, palette))
            })
    }

    /// 渲染存储条目说明行。
    fn render_storage_detail_line(
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
                    .w(px(64.0))
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

    /// 渲染存储页按钮。
    fn render_storage_action_button<F, I>(
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

    /// 后台刷新存储位置状态。
    pub(in crate::app) fn refresh_storage_locations_from_settings(
        &self,
        context: &mut Context<Self>,
    ) {
        let main_view = self.main_view.clone();
        // 文件系统扫描可能递归统计目录大小，已有任务运行时直接复用当前进度提示，避免重复启动后台任务。
        if self.main_view.read(context).settings.storage.loading {
            return;
        }
        self.main_view.update(context, |view, context| {
            view.settings.storage.loading = true;
            view.settings.storage.status_message = Some("正在扫描存储位置".to_string());
            context.notify();
        });
        context
            .spawn(async move |_settings_view, app| {
                let locations = app
                    .background_executor()
                    .spawn(async { scan_storage_locations() })
                    .await;
                app.update(move |app| {
                    main_view.update(app, |view, context| {
                        view.settings.storage.loading = false;
                        view.settings.storage.last_refreshed_at = Some(SystemTime::now());
                        view.settings.storage.locations = locations;
                        view.settings.storage.status_message = Some("已刷新存储位置".to_string());
                        context.notify();
                    });
                })
                .ok();
            })
            .detach();
    }

    /// 复制存储位置路径。
    fn copy_storage_location_path(
        &mut self,
        location_id: &'static str,
        context: &mut Context<Self>,
    ) {
        let path = self
            .main_view
            .read(context)
            .settings
            .storage
            .locations
            .iter()
            .find(|location| location.id == location_id)
            .and_then(|location| location.path.clone());
        let Some(path) = path else {
            self.update_storage_status_message("该存储位置没有可复制的路径", context);
            return;
        };
        context.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
        self.update_storage_status_message(format!("已复制路径：{}", path.display()), context);
    }

    /// 使用系统文件管理器打开存储位置。
    fn open_storage_location_from_settings(
        &mut self,
        location_id: &'static str,
        context: &mut Context<Self>,
    ) {
        let open_path = self
            .main_view
            .read(context)
            .settings
            .storage
            .locations
            .iter()
            .find(|location| location.id == location_id)
            .and_then(storage_location_file_manager_path);
        let Some(open_path) = open_path else {
            self.update_storage_status_message("该存储位置没有可打开的目录", context);
            return;
        };
        match open_directory_in_platform_file_manager(&open_path) {
            Ok(()) => {
                self.update_storage_status_message(
                    format!("已打开位置：{}", open_path.display()),
                    context,
                );
            }
            Err(error) => {
                self.update_storage_status_message(format!("打开位置失败：{error}"), context);
            }
        }
    }

    /// 清理过期大日志临时缓存。
    ///
    /// 业务意图：
    /// - 只调用现有过期缓存清理逻辑，不能删除当前进程 session，也不能删除配置、数据库或插件目录。
    pub(in crate::app) fn cleanup_stale_storage_cache_from_settings(
        &self,
        context: &mut Context<Self>,
    ) {
        let main_view = self.main_view.clone();
        // 清理缓存后会立即重新扫描；如果已有扫描或清理在进行，避免并发修改同一组状态提示。
        if self.main_view.read(context).settings.storage.loading {
            return;
        }
        self.main_view.update(context, |view, context| {
            view.settings.storage.loading = true;
            view.settings.storage.status_message = Some("正在清理过期缓存".to_string());
            context.notify();
        });
        context
            .spawn(async move |_settings_view, app| {
                app.background_executor()
                    .spawn(async { cleanup_stale_large_log_cache() })
                    .await;
                let locations = app
                    .background_executor()
                    .spawn(async { scan_storage_locations() })
                    .await;
                app.update(move |app| {
                    main_view.update(app, |view, context| {
                        view.settings.storage.loading = false;
                        view.settings.storage.last_refreshed_at = Some(SystemTime::now());
                        view.settings.storage.locations = locations;
                        view.settings.storage.status_message =
                            Some("已清理过期大日志缓存".to_string());
                        context.notify();
                    });
                })
                .ok();
            })
            .detach();
    }

    /// 更新存储页状态提示。
    fn update_storage_status_message(
        &mut self,
        message: impl Into<String>,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.settings.storage.status_message = Some(message.into());
            context.notify();
        });
        context.notify();
    }
}

/// 扫描所有存储位置。
pub(in crate::app) fn scan_storage_locations() -> Vec<StorageLocationStatus> {
    STORAGE_LOCATION_DESCRIPTORS
        .iter()
        .map(scan_storage_location)
        .collect()
}

/// 扫描单个存储位置。
fn scan_storage_location(descriptor: &StorageLocationDescriptor) -> StorageLocationStatus {
    let path = (descriptor.path)();
    let Some(path_value) = path.clone() else {
        return StorageLocationStatus {
            id: descriptor.id,
            name: descriptor.name,
            group: descriptor.group,
            purpose: descriptor.purpose,
            kind: descriptor.kind,
            path,
            exists: false,
            size_bytes: None,
            modified: None,
            error: Some("当前平台没有可用路径".to_string()),
        };
    };

    match fs::metadata(&path_value) {
        Ok(metadata) => {
            let (size_bytes, error) = if metadata.is_dir() {
                directory_size_bytes(&path_value)
            } else {
                (Some(metadata.len()), None)
            };
            StorageLocationStatus {
                id: descriptor.id,
                name: descriptor.name,
                group: descriptor.group,
                purpose: descriptor.purpose,
                kind: descriptor.kind,
                path,
                exists: true,
                size_bytes,
                modified: metadata.modified().ok(),
                error,
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => StorageLocationStatus {
            id: descriptor.id,
            name: descriptor.name,
            group: descriptor.group,
            purpose: descriptor.purpose,
            kind: descriptor.kind,
            path,
            exists: false,
            size_bytes: None,
            modified: None,
            error: None,
        },
        Err(error) => StorageLocationStatus {
            id: descriptor.id,
            name: descriptor.name,
            group: descriptor.group,
            purpose: descriptor.purpose,
            kind: descriptor.kind,
            path,
            exists: false,
            size_bytes: None,
            modified: None,
            error: Some(format!("读取元数据失败：{error}")),
        },
    }
}

/// 递归统计目录大小。
///
/// 边界条件：
/// - 使用 `symlink_metadata`，不跟随符号链接，避免插件目录或临时缓存中的链接逃逸到任意系统位置。
/// - 遇到单个子项错误时保留首个中文错误并继续统计其它可读子项，避免一个坏文件让整页无法展示。
fn directory_size_bytes(path: &Path) -> (Option<u64>, Option<String>) {
    let Ok(metadata) = fs::metadata(path) else {
        return (None, Some("读取目录元数据失败".to_string()));
    };
    if !metadata.is_dir() {
        return (None, Some("目标不是目录，无法统计目录大小".to_string()));
    }

    let mut total = 0u64;
    let mut first_error = None;
    collect_directory_size(path, &mut total, &mut first_error);
    (Some(total), first_error)
}

/// 递归累加目录大小。
fn collect_directory_size(path: &Path, total: &mut u64, first_error: &mut Option<String>) {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => {
            record_storage_scan_error(first_error, format!("读取目录失败：{error}"));
            return;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                record_storage_scan_error(first_error, format!("读取目录项失败：{error}"));
                continue;
            }
        };
        let entry_path = entry.path();
        let metadata = match fs::symlink_metadata(&entry_path) {
            Ok(metadata) => metadata,
            Err(error) => {
                record_storage_scan_error(
                    first_error,
                    format!("读取 {} 失败：{error}", entry_path.display()),
                );
                continue;
            }
        };
        if metadata.is_file() {
            *total = total.saturating_add(metadata.len());
        } else if metadata.is_dir() {
            collect_directory_size(&entry_path, total, first_error);
        }
    }
}

/// 记录目录扫描中的首个错误。
fn record_storage_scan_error(first_error: &mut Option<String>, message: String) {
    if first_error.is_none() {
        *first_error = Some(message);
    }
}

/// 按固定顺序对存储条目分组。
fn group_storage_locations(
    locations: &[StorageLocationStatus],
) -> Vec<(&'static str, Vec<StorageLocationStatus>)> {
    let mut grouped: BTreeMap<&'static str, Vec<StorageLocationStatus>> = BTreeMap::new();
    for location in locations {
        grouped
            .entry(location.group)
            .or_default()
            .push(location.clone());
    }
    STORAGE_GROUPS
        .iter()
        .filter_map(|group| grouped.remove(group).map(|locations| (*group, locations)))
        .collect()
}

/// 返回存储条目的文件管理器打开目录。
fn storage_location_file_manager_path(location: &StorageLocationStatus) -> Option<PathBuf> {
    let path = location.path.as_ref()?;
    match location.kind {
        StorageLocationKind::Database => path.parent().map(Path::to_path_buf),
        StorageLocationKind::Directory | StorageLocationKind::Cache => {
            if location.exists {
                Some(path.clone())
            } else {
                path.parent().map(Path::to_path_buf)
            }
        }
    }
}

/// 返回存储条目图标。
fn storage_location_icon(kind: StorageLocationKind) -> Icon {
    match kind {
        StorageLocationKind::Database => Icon::Database,
        StorageLocationKind::Directory => Icon::FolderOpen,
        StorageLocationKind::Cache => Icon::HardDrive,
    }
}

/// 格式化存储大小。
fn format_storage_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let bytes_f64 = bytes as f64;
    if bytes_f64 >= GB {
        format!("{:.1} GB", bytes_f64 / GB)
    } else if bytes_f64 >= MB {
        format!("{:.1} MB", bytes_f64 / MB)
    } else if bytes_f64 >= KB {
        format!("{:.1} KB", bytes_f64 / KB)
    } else {
        format!("{bytes} B")
    }
}

/// 格式化相对时间。
fn format_storage_relative_time(time: SystemTime) -> String {
    let Ok(elapsed) = time.elapsed() else {
        return "刚刚".to_string();
    };
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        "刚刚".to_string()
    } else if seconds < 60 * 60 {
        format!("{} 分钟前", seconds / 60)
    } else if seconds < 24 * 60 * 60 {
        format!("{} 小时前", seconds / (60 * 60))
    } else {
        format!("{} 天前", seconds / (24 * 60 * 60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证存储描述符覆盖当前页面要求的核心条目。
    ///
    /// 业务意图：
    /// - 配置存储只展示应用配置目录，不再展开每个配置文件；数据库、插件和缓存入口仍必须保留。
    #[test]
    fn 存储描述符包含核心条目且配置只展示目录() {
        let ids = STORAGE_LOCATION_DESCRIPTORS
            .iter()
            .map(|descriptor| descriptor.id)
            .collect::<std::collections::HashSet<_>>();
        assert!(ids.contains("config-root"));
        assert!(ids.contains("notes-dir"));
        assert!(ids.contains("ai-chat-db"));
        assert!(ids.contains("plugins-dir"));
        assert!(ids.contains("large-log-cache-root"));
        assert!(ids.contains("large-log-cache-session"));
        assert!(!ids.contains("window-size"));
        assert!(!ids.contains("theme-preference"));
        assert!(!ids.contains("model-configs"));
        assert!(!ids.contains("plugin-registry"));

        let config_count = STORAGE_LOCATION_DESCRIPTORS
            .iter()
            .filter(|descriptor| descriptor.group == "配置存储")
            .count();
        assert_eq!(config_count, 1);

        let kinds = STORAGE_LOCATION_DESCRIPTORS
            .iter()
            .map(|descriptor| descriptor.kind)
            .collect::<std::collections::HashSet<_>>();
        assert!(kinds.contains(&StorageLocationKind::Database));
        assert!(kinds.contains(&StorageLocationKind::Directory));
        assert!(kinds.contains(&StorageLocationKind::Cache));
    }

    /// 验证缺失文件显示为未创建。
    ///
    /// 业务意图：
    /// - 很多存储目录或数据库只有用户操作后才会创建，缺失不能被当作错误展示。
    #[test]
    fn 缺失文件状态为未创建() {
        fn missing_path() -> Option<PathBuf> {
            Some(env::temp_dir().join(format!(
                "logclinic3-missing-storage-test-{}",
                std::process::id()
            )))
        }

        let descriptor = StorageLocationDescriptor {
            id: "missing",
            group: "配置存储",
            name: "缺失目录",
            purpose: "测试缺失路径",
            kind: StorageLocationKind::Directory,
            path: missing_path,
        };
        let status = scan_storage_location(&descriptor);
        assert!(!status.exists);
        assert_eq!(status.state_label(), "未创建");
        assert!(status.error.is_none());
    }

    /// 验证目录大小统计遇到非目录路径会返回单条错误。
    ///
    /// 业务意图：
    /// - 文件系统状态可能被用户手工改坏；存储页应把该条目标记为错误，而不是影响其它条目。
    #[test]
    fn 目录大小统计遇到不可读路径返回错误() {
        let file_path = env::temp_dir().join(format!(
            "logclinic3-storage-file-not-dir-{}",
            std::process::id()
        ));
        fs::write(&file_path, b"abc").expect("测试文件应能写入");
        let (size, error) = directory_size_bytes(&file_path);
        let _ = fs::remove_file(&file_path);
        assert!(size.is_none());
        assert!(error.is_some());
    }

    /// 验证安全管理不会暴露删除配置或数据库的描述符级动作。
    ///
    /// 业务意图：
    /// - 本版本只允许刷新、打开、复制路径和清理过期缓存；描述符本身不能携带删除或重置语义。
    #[test]
    fn 存储描述符不包含危险删除动作() {
        assert!(STORAGE_LOCATION_DESCRIPTORS.iter().all(|descriptor| {
            !descriptor.id.contains("delete") && !descriptor.id.contains("reset")
        }));
    }
}
