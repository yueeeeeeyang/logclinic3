// 连接文件管理独立窗口。
//
// 业务意图：
// - 终端右键菜单打开的文件管理器不应占用连接页主面板，也不能阻塞终端输入和输出。
// - 窗口只维护路径栏、目录列表、选区和冲突弹窗状态；真实文件 IO 通过 `ConnectionFileBackendHandle`
//   在后台线程执行，UI 通过轮询事件刷新。
//
// 跨平台约束：
// - 上传和下载都通过 GPUI 系统文件选择器获取本地路径；Windows/macOS 的对话框差异由 GPUI 封装。
// - SSH 路径是远端字符串，本地路径是 `PathBuf` 展示字符串，二者只在后端边界转换。

use std::sync::mpsc;

use chrono::{Local, TimeZone};

use super::*;

/// 文件列表右键菜单状态。
#[derive(Clone)]
struct ConnectionFileEntryContextMenu {
    /// 菜单目标文件或目录。
    entry: ConnectionFileEntry,
    /// 菜单横坐标，窗口局部坐标。
    x: f32,
    /// 菜单纵坐标，窗口局部坐标。
    y: f32,
}

/// 文件列表右键菜单动作。
#[derive(Clone, Copy)]
enum ConnectionFileEntryContextMenuAction {
    /// 预览普通文件。
    Preview,
    /// 下载普通文件。
    Download,
    /// 上传本地文件到当前目录。
    Upload,
    /// 删除当前右键目标。
    Delete,
}

/// 文件管理窗口默认宽度。
const CONNECTION_FILE_MANAGER_WINDOW_WIDTH: f32 = 920.0;
/// 文件管理窗口默认高度。
const CONNECTION_FILE_MANAGER_WINDOW_HEIGHT: f32 = 620.0;
/// 文件管理窗口最小宽度。
const CONNECTION_FILE_MANAGER_WINDOW_MIN_WIDTH: f32 = 760.0;
/// 文件管理窗口最小高度。
const CONNECTION_FILE_MANAGER_WINDOW_MIN_HEIGHT: f32 = 460.0;
/// 文件管理工具栏高度。
const CONNECTION_FILE_MANAGER_TOOLBAR_HEIGHT: f32 = 48.0;
/// 文件管理表头高度。
const CONNECTION_FILE_MANAGER_TABLE_HEADER_HEIGHT: f32 = 32.0;
/// 文件管理行高。
const CONNECTION_FILE_MANAGER_ROW_HEIGHT: f32 = 32.0;
/// 文件管理后台事件轮询间隔。
const CONNECTION_FILE_MANAGER_POLL_MILLIS: u64 = 60;
/// 文件预览窗口默认宽度。
const CONNECTION_FILE_PREVIEW_WINDOW_WIDTH: f32 = 760.0;
/// 文件预览窗口默认高度。
const CONNECTION_FILE_PREVIEW_WINDOW_HEIGHT: f32 = 560.0;
/// 文件预览窗口最小宽度。
const CONNECTION_FILE_PREVIEW_WINDOW_MIN_WIDTH: f32 = 520.0;
/// 文件预览窗口最小高度。
const CONNECTION_FILE_PREVIEW_WINDOW_MIN_HEIGHT: f32 = 360.0;
/// 文件列表右键菜单宽度。
const CONNECTION_FILE_CONTEXT_MENU_WIDTH: f32 = 132.0;
/// 文件列表右键菜单项高度。
const CONNECTION_FILE_CONTEXT_MENU_ITEM_HEIGHT: f32 = 32.0;
/// 文件列表右键菜单纵向内边距。
const CONNECTION_FILE_CONTEXT_MENU_VERTICAL_PADDING: f32 = 8.0;

/// 连接文件管理窗口状态。
pub(in crate::app) struct ConnectionFileManagerWindowView {
    /// 主窗口实体，用于读取主题色并跟随应用明暗主题。
    main_view: Entity<MainView>,
    /// 文件后端命令/事件通道。
    backend: ConnectionFileBackendHandle,
    /// 当前后端是否是 SSH 远端；影响路径上级计算和上传/下载语义。
    remote: bool,
    /// 当前目录路径。
    current_path: String,
    /// 路径栏编辑文本。
    path_text: String,
    /// 路径栏焦点。
    path_focus: FocusHandle,
    /// 已加载的目录项。
    entries: Vec<ConnectionFileEntry>,
    /// 当前选中文件路径集合。
    selected_paths: HashSet<String>,
    /// 文件列表右键菜单。
    ///
    /// 业务意图：
    /// - 文件行的预览、下载、上传属于低频操作，统一收在右键菜单，避免每一行都堆按钮。
    /// - 菜单坐标按窗口局部坐标保存，渲染时绝对定位并夹紧，防止出现在窗口外。
    file_context_menu: Option<ConnectionFileEntryContextMenu>,
    /// 当前传输进度。
    ///
    /// 边界条件：
    /// - 只有上传或下载正在执行时才有值；完成、错误或冲突时清空，避免展示过期速度。
    transfer_progress: Option<ConnectionFileTransferProgress>,
    /// 用户可见状态消息。
    status_message: Option<String>,
    /// 用户可见错误消息。
    error_message: Option<String>,
    /// 同名冲突等待用户确认。
    pending_conflict: Option<ConnectionFileConflict>,
    /// 删除操作等待用户确认。
    ///
    /// 业务意图：
    /// - 删除是破坏性操作，右键菜单只进入确认状态；只有用户再次点击确认后才发送后端删除命令。
    /// - 第一版删除普通文件、符号链接和空目录，非空目录由后端 API 返回中文错误。
    pending_delete: Option<ConnectionFileDeleteRequest>,
    /// 是否已经安排后台事件轮询。
    poll_scheduled: bool,
}

impl ConnectionFileManagerWindowView {
    /// 创建文件管理窗口。
    pub(in crate::app) fn new(
        main_view: Entity<MainView>,
        _title: String,
        backend: ConnectionFileBackendHandle,
        initial_path: String,
        remote: bool,
        context: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            main_view,
            backend,
            remote,
            current_path: initial_path.clone(),
            path_text: initial_path,
            path_focus: context.focus_handle(),
            entries: Vec::new(),
            selected_paths: HashSet::new(),
            file_context_menu: None,
            transfer_progress: None,
            status_message: Some("正在读取目录...".to_string()),
            error_message: None,
            pending_conflict: None,
            pending_delete: None,
            poll_scheduled: false,
        };
        view.send_command(ConnectionFileCommand::List {
            path: view.current_path.clone(),
        });
        view.schedule_poll(context);
        view
    }

    /// 发送文件后端命令。
    fn send_command(&self, command: ConnectionFileCommand) {
        let _ = self.backend.command_sender.send(command);
    }

    /// 打开独立文件预览窗口。
    ///
    /// 业务意图：
    /// - 预览窗口与文件列表窗口分离，避免右侧固定预览栏挤压目录表格。
    /// - 后端预览结果已经完成大小、二进制和编码判断，因此窗口只负责展示，不再触发额外文件 IO。
    /// - 使用 `App::defer` 延迟创建窗口，避免在文件管理窗口状态更新过程中同步渲染新窗口。
    fn open_preview_window(&self, preview: ConnectionFilePreview, context: &mut Context<Self>) {
        let main_view = self.main_view.clone();
        let title = format!("{} - 文件预览", preview.name);
        context.defer(move |app| {
            let window_options = connection_file_preview_window_options(&title, app);
            let _ = app.open_window(window_options, move |_window, app| {
                app.new(|_context| ConnectionFilePreviewWindowView::new(main_view, preview))
            });
        });
    }

    /// 安排文件后端事件轮询。
    fn schedule_poll(&mut self, context: &mut Context<Self>) {
        if self.poll_scheduled {
            return;
        }
        self.poll_scheduled = true;
        context
            .spawn(async move |view, app| {
                loop {
                    app.background_executor()
                        .timer(Duration::from_millis(CONNECTION_FILE_MANAGER_POLL_MILLIS))
                        .await;
                    let keep_polling = view
                        .update(app, |view, context| {
                            view.drain_events(context);
                            view.poll_scheduled
                        })
                        .unwrap_or(false);
                    if !keep_polling {
                        break;
                    }
                }
            })
            .detach();
    }

    /// 轮询并应用后台文件事件。
    fn drain_events(&mut self, context: &mut Context<Self>) {
        let mut received = false;
        loop {
            let event = match self.backend.event_receiver.try_recv() {
                Ok(event) => {
                    received = true;
                    event
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    received = true;
                    self.poll_scheduled = false;
                    ConnectionFileEvent::Error("文件管理后端已断开".to_string())
                }
            };

            match event {
                ConnectionFileEvent::Listed { path, entries } => {
                    self.current_path = path.clone();
                    self.path_text = path;
                    self.entries = entries;
                    self.selected_paths.clear();
                    self.file_context_menu = None;
                    self.error_message = None;
                    self.status_message = Some("目录已刷新".to_string());
                }
                ConnectionFileEvent::Previewed(preview) => {
                    self.open_preview_window(preview, context);
                    self.error_message = None;
                    self.status_message = Some("已打开预览窗口".to_string());
                }
                ConnectionFileEvent::TransferProgress(progress) => {
                    self.transfer_progress = Some(progress);
                    self.error_message = None;
                }
                ConnectionFileEvent::OperationFinished(message) => {
                    self.transfer_progress = None;
                    self.status_message = Some(message);
                    self.error_message = None;
                    self.pending_conflict = None;
                    self.pending_delete = None;
                    self.file_context_menu = None;
                    self.send_command(ConnectionFileCommand::List {
                        path: self.current_path.clone(),
                    });
                }
                ConnectionFileEvent::Conflict(conflict) => {
                    self.transfer_progress = None;
                    self.pending_conflict = Some(conflict);
                    self.status_message = Some("存在同名文件，需要确认处理方式".to_string());
                }
                ConnectionFileEvent::Error(error) => {
                    self.transfer_progress = None;
                    self.pending_delete = None;
                    self.error_message = Some(error);
                    self.status_message = None;
                }
            }
        }
        if received {
            context.notify();
        }
    }

    /// 刷新当前目录。
    fn refresh(&mut self, context: &mut Context<Self>) {
        self.status_message = Some("正在读取目录...".to_string());
        self.error_message = None;
        self.send_command(ConnectionFileCommand::List {
            path: self.current_path.clone(),
        });
        self.schedule_poll(context);
        context.notify();
    }

    /// 切换到路径栏中的目录。
    fn open_path_from_input(&mut self, context: &mut Context<Self>) {
        let next = self.path_text.trim().to_string();
        if next.is_empty() {
            self.error_message = Some("路径不能为空".to_string());
            context.notify();
            return;
        }
        self.status_message = Some("正在读取目录...".to_string());
        self.error_message = None;
        self.send_command(ConnectionFileCommand::List { path: next });
        self.schedule_poll(context);
        context.notify();
    }

    /// 返回上级目录。
    fn open_parent_path(&mut self, context: &mut Context<Self>) {
        let Some(parent) = connection_file_parent_path(&self.current_path, self.remote) else {
            return;
        };
        self.path_text = parent.clone();
        self.status_message = Some("正在读取目录...".to_string());
        self.send_command(ConnectionFileCommand::List { path: parent });
        self.schedule_poll(context);
        context.notify();
    }

    /// 处理路径栏按键。
    fn handle_path_key_down(&mut self, event: &KeyDownEvent, context: &mut Context<Self>) {
        if MainView::is_paste_keystroke(&event.keystroke) {
            if let Some(text) = context.read_from_clipboard().and_then(|item| item.text()) {
                self.path_text
                    .push_str(&sanitize_file_manager_path_text(&text));
            }
            context.stop_propagation();
            context.notify();
            return;
        }
        match event.keystroke.key.as_str() {
            "enter" => {
                self.open_path_from_input(context);
                context.stop_propagation();
            }
            "escape" => {
                self.path_text = self.current_path.clone();
                context.stop_propagation();
                context.notify();
            }
            "backspace" => {
                self.path_text.pop();
                context.stop_propagation();
                context.notify();
            }
            _ => {
                if event.keystroke.modifiers.control
                    || event.keystroke.modifiers.platform
                    || event.keystroke.modifiers.alt
                {
                    return;
                }
                if let Some(text) = event.keystroke.key_char.as_ref() {
                    self.path_text
                        .push_str(&sanitize_file_manager_path_text(text));
                    context.stop_propagation();
                    context.notify();
                }
            }
        }
    }

    /// 选择或打开目录项。
    fn handle_entry_mouse_down(
        &mut self,
        entry: ConnectionFileEntry,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        if event.click_count >= 2 {
            if entry.kind.is_directory() {
                self.path_text = entry.path.clone();
                self.status_message = Some("正在读取目录...".to_string());
                self.file_context_menu = None;
                self.send_command(ConnectionFileCommand::List { path: entry.path });
                self.schedule_poll(context);
            } else if entry.kind.is_file() {
                self.status_message = Some("正在打开预览...".to_string());
                self.file_context_menu = None;
                self.send_command(ConnectionFileCommand::Preview { path: entry.path });
                self.schedule_poll(context);
            }
            context.stop_propagation();
            context.notify();
            return;
        }

        if event.modifiers.control || event.modifiers.platform {
            if !self.selected_paths.insert(entry.path.clone()) {
                self.selected_paths.remove(&entry.path);
            }
        } else {
            self.selected_paths.clear();
            self.selected_paths.insert(entry.path);
        }
        context.stop_propagation();
        context.notify();
    }

    /// 打开文件行右键菜单。
    fn open_entry_context_menu(
        &mut self,
        entry: ConnectionFileEntry,
        event: &MouseDownEvent,
        window: &Window,
        context: &mut Context<Self>,
    ) {
        if entry.kind.is_file() {
            self.selected_paths.clear();
            self.selected_paths.insert(entry.path.clone());
        }
        let viewport = window.viewport_size();
        let menu_height = CONNECTION_FILE_CONTEXT_MENU_ITEM_HEIGHT * 4.0
            + CONNECTION_FILE_CONTEXT_MENU_VERTICAL_PADDING;
        let x = f32::from(event.position.x).clamp(
            0.0,
            (f32::from(viewport.width) - CONNECTION_FILE_CONTEXT_MENU_WIDTH).max(0.0),
        );
        let y = f32::from(event.position.y)
            .clamp(0.0, (f32::from(viewport.height) - menu_height).max(0.0));
        self.file_context_menu = Some(ConnectionFileEntryContextMenu { entry, x, y });
        context.stop_propagation();
        context.notify();
    }

    /// 执行文件行右键菜单动作。
    fn handle_entry_context_menu_action(
        &mut self,
        action: ConnectionFileEntryContextMenuAction,
        context: &mut Context<Self>,
    ) {
        let Some(menu) = self.file_context_menu.take() else {
            return;
        };
        match action {
            ConnectionFileEntryContextMenuAction::Preview => {
                if menu.entry.kind.is_file() {
                    self.status_message = Some("正在打开预览...".to_string());
                    self.send_command(ConnectionFileCommand::Preview {
                        path: menu.entry.path,
                    });
                    self.schedule_poll(context);
                }
            }
            ConnectionFileEntryContextMenuAction::Download => {
                if menu.entry.kind.is_file() {
                    self.selected_paths.clear();
                    self.selected_paths.insert(menu.entry.path);
                    self.begin_download_prompt(context);
                }
            }
            ConnectionFileEntryContextMenuAction::Upload => {
                self.begin_upload_prompt(context);
            }
            ConnectionFileEntryContextMenuAction::Delete => {
                self.begin_delete_entry(menu.entry, context);
            }
        }
        context.stop_propagation();
        context.notify();
    }

    /// 进入删除确认状态。
    fn begin_delete_entry(&mut self, entry: ConnectionFileEntry, context: &mut Context<Self>) {
        if !entry.kind.is_deletable() {
            self.error_message = Some("仅支持删除普通文件、符号链接或空目录".to_string());
            context.notify();
            return;
        }
        self.pending_delete = Some(ConnectionFileDeleteRequest {
            path: entry.path,
            name: entry.name,
            kind: entry.kind,
        });
        self.file_context_menu = None;
        self.error_message = None;
        context.notify();
    }

    /// 用户确认删除后向后台发送删除命令。
    fn confirm_delete(&mut self, context: &mut Context<Self>) {
        let Some(request) = self.pending_delete.take() else {
            return;
        };
        self.status_message = Some(format!("正在删除 {}...", request.name));
        self.error_message = None;
        self.send_command(ConnectionFileCommand::Delete(request));
        self.schedule_poll(context);
        context.notify();
    }

    /// 取消删除确认弹窗。
    fn cancel_delete(&mut self, context: &mut Context<Self>) {
        self.pending_delete = None;
        context.notify();
    }

    /// 请求取消当前传输任务。
    fn cancel_transfer(&mut self, context: &mut Context<Self>) {
        self.backend.cancel_transfer();
        self.status_message = Some("正在取消传输...".to_string());
        context.notify();
    }

    /// 打开上传文件选择器。
    fn begin_upload_prompt(&mut self, context: &mut Context<Self>) {
        let view_entity = context.entity();
        context
            .spawn(async move |_view, app| {
                let options = PathPromptOptions {
                    files: true,
                    directories: false,
                    multiple: true,
                    prompt: Some("选择要上传的文件".into()),
                };
                let receiver = match app.update(|app| app.prompt_for_paths(options)) {
                    Ok(receiver) => receiver,
                    Err(_) => return,
                };
                let selected = match receiver.await {
                    Ok(Ok(Some(paths))) => paths,
                    _ => return,
                };
                let _ = view_entity.update(app, move |view, context| {
                    view.upload_paths(selected, context);
                });
            })
            .detach();
    }

    /// 上传所选本地文件。
    fn upload_paths(&mut self, paths: Vec<PathBuf>, context: &mut Context<Self>) {
        let source_paths = paths
            .into_iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        if source_paths.is_empty() {
            return;
        }
        self.status_message = Some("正在上传文件...".to_string());
        self.error_message = None;
        self.transfer_progress = None;
        self.file_context_menu = None;
        self.send_command(ConnectionFileCommand::Upload(
            ConnectionFileTransferRequest {
                source_paths,
                target_dir: self.current_path.clone(),
                conflict_policy: ConnectionFileConflictPolicy::Ask,
            },
        ));
        self.schedule_poll(context);
        context.notify();
    }

    /// 打开下载目标目录选择器。
    fn begin_download_prompt(&mut self, context: &mut Context<Self>) {
        let selected = self.selected_file_paths();
        if selected.is_empty() {
            self.error_message = Some("请先选择一个或多个普通文件".to_string());
            context.notify();
            return;
        }
        let view_entity = context.entity();
        context
            .spawn(async move |_view, app| {
                let options = PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: Some("选择下载目标目录".into()),
                };
                let receiver = match app.update(|app| app.prompt_for_paths(options)) {
                    Ok(receiver) => receiver,
                    Err(_) => return,
                };
                let target_dir = match receiver.await {
                    Ok(Ok(Some(paths))) => paths.into_iter().next(),
                    _ => None,
                };
                let Some(target_dir) = target_dir else {
                    return;
                };
                let _ = view_entity.update(app, move |view, context| {
                    view.download_to(target_dir, context);
                });
            })
            .detach();
    }

    /// 下载已选择文件。
    fn download_to(&mut self, target_dir: PathBuf, context: &mut Context<Self>) {
        let selected = self.selected_file_paths();
        if selected.is_empty() {
            return;
        }
        self.status_message = Some("正在下载文件...".to_string());
        self.error_message = None;
        self.transfer_progress = None;
        self.file_context_menu = None;
        self.send_command(ConnectionFileCommand::Download(
            ConnectionFileTransferRequest {
                source_paths: selected,
                target_dir: target_dir.to_string_lossy().to_string(),
                conflict_policy: ConnectionFileConflictPolicy::Ask,
            },
        ));
        self.schedule_poll(context);
        context.notify();
    }

    /// 返回当前选中的普通文件路径。
    fn selected_file_paths(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| {
                entry.kind.is_file() && self.selected_paths.contains(entry.path.as_str())
            })
            .map(|entry| entry.path.clone())
            .collect()
    }

    /// 按冲突策略继续执行等待中的传输命令。
    fn resolve_conflict(
        &mut self,
        policy: ConnectionFileConflictPolicy,
        context: &mut Context<Self>,
    ) {
        let Some(conflict) = self.pending_conflict.take() else {
            return;
        };
        let command = connection_file_command_with_policy(conflict.command, policy);
        self.status_message = Some(match policy {
            ConnectionFileConflictPolicy::Overwrite => "正在覆盖同名文件...".to_string(),
            ConnectionFileConflictPolicy::Skip => "正在跳过同名文件...".to_string(),
            ConnectionFileConflictPolicy::Ask => "正在处理文件...".to_string(),
        });
        self.transfer_progress = None;
        self.file_context_menu = None;
        self.send_command(command);
        self.schedule_poll(context);
        context.notify();
    }

    /// 渲染工具栏按钮。
    fn render_toolbar_button(
        &self,
        id: &'static str,
        _label: &'static str,
        icon: Icon,
        enabled: bool,
        palette: AppThemePalette,
        on_click: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .h(px(30.0))
            .w(px(30.0))
            .rounded(px(5.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(if enabled {
                palette.background
            } else {
                palette.surface
            }))
            .text_color(rgb(if enabled {
                palette.text
            } else {
                palette.muted_text
            }))
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |button| button.bg(rgb(palette.hover)))
                    .on_click(context.listener(on_click))
            })
            .child(MainView::render_lucide_icon(
                Some(icon),
                16.0,
                14.0,
                if enabled {
                    palette.muted_text
                } else {
                    palette.border
                },
            ))
    }

    /// 渲染路径栏。
    fn render_path_bar(
        &self,
        palette: AppThemePalette,
        window: &Window,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let focused = self.path_focus.is_focused(window);
        let text = if focused {
            format!("{}|", self.path_text)
        } else {
            self.path_text.clone()
        };
        div()
            .id("connection-file-path-input")
            .track_focus(&self.path_focus)
            .key_context("connection-file-path-input")
            .on_key_down(
                context.listener(|view, event: &KeyDownEvent, _window, context| {
                    view.handle_path_key_down(event, context);
                }),
            )
            .flex()
            .items_center()
            .h(px(30.0))
            .flex_1()
            .min_w_0()
            .px_2()
            .rounded(px(5.0))
            .border_1()
            .border_color(rgb(if focused {
                palette.accent
            } else {
                palette.border
            }))
            .bg(rgb(palette.background))
            .text_color(rgb(palette.text))
            .text_size(px(12.0))
            .overflow_hidden()
            .whitespace_nowrap()
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, _event: &MouseDownEvent, window, context| {
                    window.focus(&view.path_focus);
                    context.stop_propagation();
                }),
            )
            .child(text)
    }

    /// 渲染单个文件行。
    fn render_file_entry_row(
        &self,
        entry: ConnectionFileEntry,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.selected_paths.contains(entry.path.as_str());
        let icon = if entry.kind.is_directory() {
            Icon::Folder
        } else {
            Icon::File
        };
        let row_entry = entry.clone();
        let context_entry = entry.clone();
        div()
            .id(SharedString::from(format!(
                "connection-file-row-{}",
                entry.path
            )))
            .flex()
            .items_center()
            .flex_none()
            .h(px(CONNECTION_FILE_MANAGER_ROW_HEIGHT))
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(if selected {
                palette.selected
            } else {
                palette.background
            }))
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |row| {
                if selected {
                    row
                } else {
                    row.bg(rgb(palette.hover))
                }
            })
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.handle_entry_mouse_down(row_entry.clone(), event, context);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(move |view, event: &MouseDownEvent, window, context| {
                    view.open_entry_context_menu(context_entry.clone(), event, window, context);
                }),
            )
            .child(MainView::render_lucide_icon(
                Some(icon),
                18.0,
                14.0,
                palette.muted_text,
            ))
            .child(
                div()
                    .w(px(260.0))
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_size(px(12.0))
                    .child(entry.name),
            )
            .child(
                div()
                    .w(px(72.0))
                    .text_size(px(12.0))
                    .text_color(rgb(palette.muted_text))
                    .child(entry.kind.label()),
            )
            .child(
                div()
                    .w(px(92.0))
                    .text_size(px(12.0))
                    .text_color(rgb(palette.muted_text))
                    .child(format_file_size(entry.size)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_size(px(12.0))
                    .text_color(rgb(palette.muted_text))
                    .child(format_file_time(entry.modified_at_ms)),
            )
    }

    /// 渲染传输进度条。
    fn render_transfer_progress(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(progress) = self.transfer_progress.as_ref() else {
            return div().id("connection-file-progress-empty").hidden();
        };
        let ratio = progress.ratio();
        let percent = (ratio * 100.0).round() as u32;
        let speed = format_file_speed(progress.bytes_per_second());
        let message = format!(
            "{}中 {} / {} · {} / {} · {}",
            progress.operation.label(),
            progress.completed_files.min(progress.total_files),
            progress.total_files,
            format_file_size(Some(progress.transferred_bytes)),
            format_file_size(Some(progress.total_bytes)),
            speed
        );
        div()
            .id("connection-file-progress")
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .text_size(px(12.0))
                    .text_color(rgb(palette.muted_text))
                    .child(div().flex_1().min_w_0().child(message))
                    .child(format!("{percent}%"))
                    .child(
                        div()
                            .id("connection-file-transfer-cancel")
                            .flex()
                            .items_center()
                            .justify_center()
                            .h(px(22.0))
                            .w(px(22.0))
                            .rounded(px(4.0))
                            .cursor_pointer()
                            .hover(move |button| button.bg(rgb(palette.hover)))
                            .on_click(context.listener(
                                |view, _event: &ClickEvent, _window, context| {
                                    view.cancel_transfer(context);
                                },
                            ))
                            .child(MainView::render_lucide_icon(
                                Some(Icon::X),
                                14.0,
                                12.0,
                                palette.muted_text,
                            )),
                    ),
            )
            .child(
                div()
                    .h(px(6.0))
                    .w_full()
                    .rounded(px(3.0))
                    .overflow_hidden()
                    .bg(rgb(palette.border))
                    .child(div().h_full().w(relative(ratio)).bg(rgb(palette.accent))),
            )
    }

    /// 渲染文件列表右键菜单关闭层。
    fn render_file_context_menu_overlay(&self, context: &mut Context<Self>) -> impl IntoElement {
        if self.file_context_menu.is_none() {
            return div()
                .id("connection-file-context-menu-overlay-empty")
                .hidden();
        }
        div()
            .id("connection-file-context-menu-overlay")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    view.file_context_menu = None;
                    context.stop_propagation();
                    context.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    view.file_context_menu = None;
                    context.stop_propagation();
                    context.notify();
                }),
            )
    }

    /// 渲染文件列表右键菜单。
    fn render_file_context_menu(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(menu) = self.file_context_menu.as_ref() else {
            return div().id("connection-file-context-menu-empty").hidden();
        };
        let can_preview = menu.entry.kind.is_file();
        let can_download = menu.entry.kind.is_file();
        let can_delete = menu.entry.kind.is_deletable();
        div()
            .id("connection-file-context-menu")
            .absolute()
            .left(px(menu.x))
            .top(px(menu.y))
            .w(px(CONNECTION_FILE_CONTEXT_MENU_WIDTH))
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.background))
            .shadow_lg()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .child(self.render_file_context_menu_item(
                ConnectionFileEntryContextMenuAction::Preview,
                "预览",
                Icon::FileText,
                can_preview,
                palette,
                context,
            ))
            .child(self.render_file_context_menu_item(
                ConnectionFileEntryContextMenuAction::Download,
                "下载",
                Icon::Download,
                can_download,
                palette,
                context,
            ))
            .child(self.render_file_context_menu_item(
                ConnectionFileEntryContextMenuAction::Upload,
                "上传",
                Icon::Upload,
                true,
                palette,
                context,
            ))
            .child(self.render_file_context_menu_item(
                ConnectionFileEntryContextMenuAction::Delete,
                "删除",
                Icon::Trash2,
                can_delete,
                palette,
                context,
            ))
    }

    /// 渲染文件列表右键菜单项。
    fn render_file_context_menu_item(
        &self,
        action: ConnectionFileEntryContextMenuAction,
        label: &'static str,
        icon: Icon,
        enabled: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!("connection-file-menu-{label}")))
            .flex()
            .items_center()
            .gap_2()
            .h(px(CONNECTION_FILE_CONTEXT_MENU_ITEM_HEIGHT))
            .px_3()
            .text_size(px(13.0))
            .text_color(rgb(if enabled {
                palette.text
            } else {
                palette.muted_text
            }))
            .when(enabled, |item| {
                item.cursor_pointer()
                    .hover(move |item| item.bg(rgb(palette.hover)))
            })
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                    if enabled {
                        view.handle_entry_context_menu_action(action, context);
                    } else {
                        context.stop_propagation();
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .child(MainView::render_lucide_icon(
                Some(icon),
                16.0,
                14.0,
                if enabled {
                    palette.muted_text
                } else {
                    palette.border
                },
            ))
            .child(label)
    }

    /// 渲染冲突确认弹窗。
    fn render_conflict_dialog(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(conflict) = self.pending_conflict.as_ref() else {
            return div()
                .id("connection-file-conflict-empty")
                .hidden()
                .into_any_element();
        };
        let names = conflict
            .names
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("、");
        div()
            .id("connection-file-conflict-overlay")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000066))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .child(
                div()
                    .w(px(360.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.background))
                    .shadow_lg()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child("同名文件冲突"),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .line_height(px(18.0))
                            .text_color(rgb(palette.muted_text))
                            .child(format!(
                                "发现 {} 个同名文件：{}",
                                conflict.names.len(),
                                names
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                div()
                                    .id("connection-file-conflict-skip")
                                    .h(px(30.0))
                                    .px_3()
                                    .flex()
                                    .items_center()
                                    .rounded(px(5.0))
                                    .border_1()
                                    .border_color(rgb(palette.border))
                                    .cursor_pointer()
                                    .child("跳过")
                                    .on_click(context.listener(
                                        |view, _event: &ClickEvent, _window, context| {
                                            view.resolve_conflict(
                                                ConnectionFileConflictPolicy::Skip,
                                                context,
                                            );
                                        },
                                    )),
                            )
                            .child(
                                div()
                                    .id("connection-file-conflict-overwrite")
                                    .h(px(30.0))
                                    .px_3()
                                    .flex()
                                    .items_center()
                                    .rounded(px(5.0))
                                    .bg(rgb(palette.accent))
                                    .text_color(rgb(palette.on_accent))
                                    .cursor_pointer()
                                    .child("覆盖")
                                    .on_click(context.listener(
                                        |view, _event: &ClickEvent, _window, context| {
                                            view.resolve_conflict(
                                                ConnectionFileConflictPolicy::Overwrite,
                                                context,
                                            );
                                        },
                                    )),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// 渲染删除确认弹窗。
    fn render_delete_dialog(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(request) = self.pending_delete.as_ref() else {
            return div()
                .id("connection-file-delete-empty")
                .hidden()
                .into_any_element();
        };
        div()
            .id("connection-file-delete-overlay")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000066))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .child(
                div()
                    .w(px(380.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.background))
                    .shadow_lg()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child("确认删除"),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .line_height(px(18.0))
                            .text_color(rgb(palette.muted_text))
                            .child(format!(
                                "将删除“{}”。目录仅支持删除空目录，删除后无法从应用内恢复。",
                                request.name
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                div()
                                    .id("connection-file-delete-cancel")
                                    .h(px(30.0))
                                    .px_3()
                                    .flex()
                                    .items_center()
                                    .rounded(px(5.0))
                                    .border_1()
                                    .border_color(rgb(palette.border))
                                    .cursor_pointer()
                                    .child("取消")
                                    .on_click(context.listener(
                                        |view, _event: &ClickEvent, _window, context| {
                                            view.cancel_delete(context);
                                        },
                                    )),
                            )
                            .child(
                                div()
                                    .id("connection-file-delete-confirm")
                                    .h(px(30.0))
                                    .px_3()
                                    .flex()
                                    .items_center()
                                    .rounded(px(5.0))
                                    .bg(rgb(palette.error))
                                    .text_color(rgb(palette.on_accent))
                                    .cursor_pointer()
                                    .child("删除")
                                    .on_click(context.listener(
                                        |view, _event: &ClickEvent, _window, context| {
                                            view.confirm_delete(context);
                                        },
                                    )),
                            ),
                    ),
            )
            .into_any_element()
    }
}

impl Drop for ConnectionFileManagerWindowView {
    /// 窗口销毁时关闭后台文件会话。
    fn drop(&mut self) {
        self.backend.shutdown();
    }
}

impl Render for ConnectionFileManagerWindowView {
    /// 渲染文件管理窗口。
    fn render(&mut self, window: &mut Window, context: &mut Context<Self>) -> impl IntoElement {
        self.drain_events(context);
        let palette = self.main_view.read(context).palette();
        let selected_has_files = !self.selected_file_paths().is_empty();
        let entries = self.entries.clone();
        let file_rows = entries
            .into_iter()
            .map(|entry| {
                self.render_file_entry_row(entry, palette, context)
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        div()
            .id("connection-file-manager-window")
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(palette.background))
            .child(
                div()
                    .id("connection-file-toolbar")
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(CONNECTION_FILE_MANAGER_TOOLBAR_HEIGHT))
                    .px_3()
                    .border_b_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.panel))
                    .child(self.render_toolbar_button(
                        "connection-file-up",
                        "上级",
                        Icon::ArrowUp,
                        connection_file_parent_path(&self.current_path, self.remote).is_some(),
                        palette,
                        |view, _event, _window, context| {
                            view.open_parent_path(context);
                        },
                        context,
                    ))
                    .child(self.render_toolbar_button(
                        "connection-file-refresh",
                        "刷新",
                        Icon::RefreshCw,
                        true,
                        palette,
                        |view, _event, _window, context| {
                            view.refresh(context);
                        },
                        context,
                    ))
                    .child(self.render_path_bar(palette, window, context))
                    .child(self.render_toolbar_button(
                        "connection-file-upload",
                        "上传",
                        Icon::Upload,
                        true,
                        palette,
                        |view, _event, _window, context| {
                            view.begin_upload_prompt(context);
                        },
                        context,
                    ))
                    .child(self.render_toolbar_button(
                        "connection-file-download",
                        "下载",
                        Icon::Download,
                        selected_has_files,
                        palette,
                        |view, _event, _window, context| {
                            view.begin_download_prompt(context);
                        },
                        context,
                    )),
            )
            .when_some(self.error_message.as_ref(), |root, message| {
                root.child(
                    div()
                        .px_3()
                        .py_2()
                        .border_b_1()
                        .border_color(rgb(palette.border))
                        .bg(rgb(palette.surface))
                        .text_color(rgb(palette.error))
                        .text_size(px(12.0))
                        .child(message.clone()),
                )
            })
            .when_some(self.status_message.as_ref(), |root, message| {
                root.child(
                    div()
                        .px_3()
                        .py_1()
                        .border_b_1()
                        .border_color(rgb(palette.border))
                        .text_color(rgb(palette.muted_text))
                        .text_size(px(12.0))
                        .child(message.clone()),
                )
            })
            .child(self.render_transfer_progress(palette, context))
            .child(
                div()
                    .id("connection-file-table")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .child(
                        div()
                            .id("connection-file-list-header")
                            .flex()
                            .items_center()
                            .flex_none()
                            .h(px(CONNECTION_FILE_MANAGER_TABLE_HEADER_HEIGHT))
                            .px_3()
                            .gap_2()
                            .border_b_1()
                            .border_color(rgb(palette.border))
                            .bg(rgb(palette.panel))
                            .text_size(px(12.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.muted_text))
                            .child(div().w(px(18.0)))
                            .child(div().w(px(260.0)).child("名称"))
                            .child(div().w(px(72.0)).child("类型"))
                            .child(div().w(px(92.0)).child("大小"))
                            .child(div().flex_1().child("修改时间")),
                    )
                    .child(
                        div()
                            .id("connection-file-list-scroll")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .overflow_hidden()
                            .overflow_y_scroll()
                            .scrollbar_width(px(6.0))
                            .children(file_rows),
                    ),
            )
            .child(self.render_file_context_menu_overlay(context))
            .child(self.render_file_context_menu(palette, context))
            .child(self.render_conflict_dialog(palette, context))
            .child(self.render_delete_dialog(palette, context))
    }
}

/// 文件预览独立窗口。
///
/// 业务意图：
/// - 双击文件后以独立窗口展示预览，用户可以同时保留文件列表并查看多个文件。
/// - 该窗口只展示后台已经读取好的 `ConnectionFilePreview`，不会访问本地文件系统或远端 SFTP，避免阻塞 UI。
struct ConnectionFilePreviewWindowView {
    /// 主窗口实体，用于读取主题色并保持预览窗口明暗主题一致。
    main_view: Entity<MainView>,
    /// 后台预览结果，包含文本内容或不可预览原因。
    preview: ConnectionFilePreview,
}

impl ConnectionFilePreviewWindowView {
    /// 创建文件预览窗口状态。
    fn new(main_view: Entity<MainView>, preview: ConnectionFilePreview) -> Self {
        Self { main_view, preview }
    }
}

impl Render for ConnectionFilePreviewWindowView {
    /// 渲染文件预览窗口。
    fn render(&mut self, _window: &mut Window, context: &mut Context<Self>) -> impl IntoElement {
        let palette = self.main_view.read(context).palette();
        let metadata = format!(
            "{} · {}",
            format_file_size(self.preview.size),
            format_file_time(self.preview.modified_at_ms)
        );
        let body = if let Some(text) = self.preview.text.as_ref() {
            div()
                .id("connection-file-preview-text")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .scrollbar_width(px(6.0))
                .p_3()
                .rounded(px(5.0))
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .font_family(LOG_VIEWER_FONT_FAMILY)
                .text_size(px(12.0))
                .line_height(px(18.0))
                .text_color(rgb(palette.text))
                .child(text.clone())
                .into_any_element()
        } else {
            div()
                .id("connection-file-preview-message")
                .flex_1()
                .items_center()
                .justify_center()
                .rounded(px(5.0))
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .text_size(px(13.0))
                .text_color(rgb(palette.muted_text))
                .child(
                    self.preview
                        .message
                        .clone()
                        .unwrap_or_else(|| "该文件不支持预览".to_string()),
                )
                .into_any_element()
        };

        div()
            .id("connection-file-preview-window")
            .flex()
            .flex_col()
            .size_full()
            .gap_3()
            .p_3()
            .bg(rgb(palette.background))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child(self.preview.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(palette.muted_text))
                            .child(self.preview.path.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(palette.muted_text))
                            .child(metadata),
                    )
                    .when_some(self.preview.message.as_ref(), |header, message| {
                        header.child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(palette.muted_text))
                                .child(message.clone()),
                        )
                    }),
            )
            .child(body)
    }
}

/// 构造文件管理窗口配置。
pub(in crate::app) fn connection_file_manager_window_options(
    title: &str,
    app: &mut App,
) -> WindowOptions {
    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(SharedString::from(title.to_string())),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(
            size(
                px(CONNECTION_FILE_MANAGER_WINDOW_WIDTH),
                px(CONNECTION_FILE_MANAGER_WINDOW_HEIGHT),
            ),
            app,
        )),
        is_resizable: true,
        is_minimizable: true,
        window_min_size: Some(size(
            px(CONNECTION_FILE_MANAGER_WINDOW_MIN_WIDTH),
            px(CONNECTION_FILE_MANAGER_WINDOW_MIN_HEIGHT),
        )),
        ..Default::default()
    }
}

/// 构造文件预览窗口配置。
fn connection_file_preview_window_options(title: &str, app: &mut App) -> WindowOptions {
    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(SharedString::from(title.to_string())),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::centered(
            size(
                px(CONNECTION_FILE_PREVIEW_WINDOW_WIDTH),
                px(CONNECTION_FILE_PREVIEW_WINDOW_HEIGHT),
            ),
            app,
        )),
        is_resizable: true,
        is_minimizable: true,
        window_min_size: Some(size(
            px(CONNECTION_FILE_PREVIEW_WINDOW_MIN_WIDTH),
            px(CONNECTION_FILE_PREVIEW_WINDOW_MIN_HEIGHT),
        )),
        ..Default::default()
    }
}

/// 清理路径栏粘贴或输入文本。
fn sanitize_file_manager_path_text(text: &str) -> String {
    text.chars()
        .filter(|character| !matches!(character, '\n' | '\r' | '\t'))
        .collect()
}

/// 格式化文件大小。
fn format_file_size(size: Option<u64>) -> String {
    let Some(size) = size else {
        return "-".to_string();
    };
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let size = size as f64;
    if size >= GB {
        format!("{:.1} GB", size / GB)
    } else if size >= MB {
        format!("{:.1} MB", size / MB)
    } else if size >= KB {
        format!("{:.1} KB", size / KB)
    } else {
        format!("{} B", size as u64)
    }
}

/// 格式化文件传输速度。
fn format_file_speed(bytes_per_second: f64) -> String {
    if !bytes_per_second.is_finite() || bytes_per_second <= 0.0 {
        return "0 B/s".to_string();
    }
    format!("{}/s", format_file_size(Some(bytes_per_second as u64)))
}

/// 格式化修改时间。
fn format_file_time(modified_at_ms: Option<i64>) -> String {
    let Some(modified_at_ms) = modified_at_ms else {
        return "-".to_string();
    };
    Local
        .timestamp_millis_opt(modified_at_ms)
        .single()
        .map(|time| time.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "-".to_string())
}

/// 给文件传输命令替换冲突策略。
fn connection_file_command_with_policy(
    command: ConnectionFileCommand,
    policy: ConnectionFileConflictPolicy,
) -> ConnectionFileCommand {
    match command {
        ConnectionFileCommand::Upload(mut request) => {
            request.conflict_policy = policy;
            ConnectionFileCommand::Upload(request)
        }
        ConnectionFileCommand::Download(mut request) => {
            request.conflict_policy = policy;
            ConnectionFileCommand::Download(request)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证修改时间会转换为可读日期文本，而不是直接展示 epoch 毫秒。
    #[test]
    fn 文件修改时间会格式化为可读文本() {
        let formatted = format_file_time(Some(0));
        assert_ne!(formatted, "0");
        assert!(formatted.contains("1970"));
        assert!(formatted.contains(':'));
    }

    /// 验证缺失修改时间保持占位符，避免列表出现空白列。
    #[test]
    fn 文件修改时间缺失时显示占位符() {
        assert_eq!(format_file_time(None), "-");
    }
}
