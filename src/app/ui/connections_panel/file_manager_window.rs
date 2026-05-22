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

use std::{sync::mpsc, time::Instant};

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

/// 文件列表排序列。
///
/// 业务意图：
/// - 文件管理列表的表头需要像常规表格一样支持点击排序。
/// - 枚举列定义比字符串判断更稳定，后续增加列宽或国际化文案时不影响排序逻辑。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectionFileSortColumn {
    /// 按文件名排序。
    Name,
    /// 按文件类型排序。
    Kind,
    /// 按文件大小排序。
    Size,
    /// 按修改时间排序。
    ModifiedAt,
}

/// 文件列表排序状态。
///
/// 边界条件：
/// - 排序只影响当前文件管理窗口的展示顺序，不回写后端，也不改变目录实际内容。
/// - 目录始终排在普通文件之前，避免按大小或时间排序后目录被埋到文件列表中。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ConnectionFileSortState {
    /// 当前排序列。
    column: ConnectionFileSortColumn,
    /// 是否升序排列。
    ascending: bool,
}

/// 文件列表滚动条拖动状态。
///
/// 业务意图：
/// - 原生 overlay scrollbar 在 macOS 上经常不可见，文件列表需要自绘可见滚动条提示当前位置。
/// - 保存鼠标在滑块内的按下偏移，可以避免拖动开始时滑块跳到鼠标中心。
#[derive(Clone, Copy)]
struct ConnectionFileListScrollbarDrag {
    /// 鼠标按下点相对滑块顶部的偏移。
    cursor_offset: Pixels,
}

/// 文件管理路径输入框最近一次绘制布局。
///
/// 业务意图：
/// - 地址栏现在复用通用 `TextInputElement`，鼠标定位、拖拽选区和 IME 候选窗口都依赖上一帧的字形布局。
/// - 文件管理窗口是独立 `Entity`，不能把这些布局缓存写回主窗口状态，因此在窗口状态内保存一份。
#[derive(Clone)]
struct ConnectionFilePathInputLayout {
    /// 路径文本的单行字形布局。
    line: ShapedLine,
    /// 输入框当前帧的窗口坐标边界。
    bounds: Bounds<Pixels>,
    /// 当前水平滚动偏移。
    horizontal_scroll_px: f32,
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
/// 文件列表自绘纵向滚动条宽度。
const CONNECTION_FILE_LIST_SCROLLBAR_WIDTH: f32 = 6.0;
/// 文件列表自绘纵向滚动条最小滑块高度。
const CONNECTION_FILE_LIST_SCROLLBAR_MIN_THUMB_HEIGHT: f32 = 36.0;
/// 文件列表自绘纵向滚动条内边距。
const CONNECTION_FILE_LIST_SCROLLBAR_PADDING: f32 = 3.0;

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
    /// 路径栏编辑状态。
    ///
    /// 业务意图：
    /// - 文件管理地址栏需要支持长路径水平滚动、中文 IME、复制粘贴、选区和鼠标定位。
    /// - 使用通用单行输入状态后，地址栏不再维护一套简化的字符串拼接逻辑。
    path_input: SingleLineTextInputState,
    /// 路径栏焦点。
    path_focus: FocusHandle,
    /// 路径栏最近一次字形布局。
    path_layout: Option<ConnectionFilePathInputLayout>,
    /// 路径栏光标最近一次用户活动时间。
    ///
    /// 业务意图：
    /// - 通用输入框组件只询问宿主“本帧是否显示光标”，具体闪烁节奏由宿主维护。
    /// - 文件管理窗口是独立窗口，不能复用主窗口搜索输入框的时间戳，因此这里保存自己的活动时间。
    path_cursor_last_activity: Instant,
    /// 已加载的目录项。
    entries: Vec<ConnectionFileEntry>,
    /// 已按当前表头状态排序后的目录项缓存。
    ///
    /// 业务意图：
    /// - 文件列表使用虚拟滚动渲染，滚动时会频繁请求可见 range。
    /// - 排序本身是全量 O(n log n) 操作，必须只在目录刷新或用户切换排序列时执行，避免滚动过程中反复克隆和排序大量目录项。
    /// - 缓存保存完整 `ConnectionFileEntry` 克隆，是因为右键菜单、双击预览和选择逻辑都需要拿到完整条目，而不仅是索引。
    sorted_entries: Vec<ConnectionFileEntry>,
    /// 文件列表虚拟滚动句柄。
    ///
    /// 业务意图：
    /// - 文件列表行高固定，使用 `UniformListScrollHandle` 可以让大量文件只渲染可见行。
    /// - 同一个句柄同时驱动滚轮、触控板和自绘纵向滚动条，避免维护两套滚动位置。
    list_scroll_handle: UniformListScrollHandle,
    /// 文件列表当前排序状态。
    sort: ConnectionFileSortState,
    /// 当前选中文件路径集合。
    selected_paths: HashSet<String>,
    /// 文件列表自绘滚动条拖动状态。
    list_scrollbar_drag: Option<ConnectionFileListScrollbarDrag>,
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
            path_input: SingleLineTextInputState::from_text(initial_path),
            path_focus: context.focus_handle(),
            path_layout: None,
            path_cursor_last_activity: Instant::now(),
            entries: Vec::new(),
            sorted_entries: Vec::new(),
            list_scroll_handle: UniformListScrollHandle::new(),
            sort: ConnectionFileSortState {
                column: ConnectionFileSortColumn::Name,
                ascending: true,
            },
            selected_paths: HashSet::new(),
            list_scrollbar_drag: None,
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

    /// 替换路径栏文本并重置单行输入派生状态。
    ///
    /// 业务意图：
    /// - 后端列目录成功、双击进入目录或点击上级时，路径栏应整体切换到新目录并把光标放到末尾。
    /// - 旧的选区、IME 组合范围和水平滚动偏移都属于旧路径，继续复用会导致光标位置和鼠标命中错位。
    fn set_path_text(&mut self, text: String) {
        self.path_input.set_text(text);
        self.path_layout = None;
        self.touch_path_input_cursor_activity();
    }

    /// 标记文件管理地址栏光标发生用户活动。
    ///
    /// 业务意图：
    /// - 点击定位、方向键移动、粘贴、IME 提交和程序切换路径后，光标应先保持短暂常亮再进入闪烁。
    /// - 与主窗口通用输入框保持同一节奏，避免同一组件在不同窗口里看起来像两套控件。
    fn touch_path_input_cursor_activity(&mut self) {
        self.path_cursor_last_activity = Instant::now();
    }

    /// 判断窗口坐标是否位于地址栏输入区域。
    fn path_input_contains_position(&self, position: Point<Pixels>) -> bool {
        self.path_layout
            .as_ref()
            .is_some_and(|layout| layout.bounds.contains(&position))
    }

    /// 点击地址栏以外位置时取消地址栏焦点。
    ///
    /// 业务意图：
    /// - GPUI 的焦点不会因为点击普通列表行自动清空；如果文件行没有自己的焦点句柄，地址栏会继续显示蓝色焦点边框。
    /// - 在窗口根节点统一处理“输入框外点击失焦”，可以让文件列表、工具栏和状态栏的点击都恢复普通状态。
    fn blur_path_input_if_click_outside(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left || !self.path_focus.is_focused(window) {
            return;
        }
        if self.path_input_contains_position(event.position) {
            return;
        }
        self.path_input.selection_drag = None;
        self.path_input.marked_range = None;
        window.blur();
        context.notify();
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
                ConnectionFileEvent::SmbConnected {
                    profile_id,
                    connected_at_ms,
                } => {
                    let main_view = self.main_view.clone();
                    context.defer(move |app| {
                        let _ = main_view.update(app, |view, context| {
                            if let Some(path) = view.connections.database_path.clone() {
                                let _ = update_smb_connection_last_connected_at(
                                    &path,
                                    &profile_id,
                                    connected_at_ms,
                                );
                            }
                            if let Some(profile) = view
                                .connections
                                .smb_profiles
                                .iter_mut()
                                .find(|profile| profile.id == profile_id)
                            {
                                profile.last_connected_at_ms = Some(connected_at_ms);
                            }
                            context.notify();
                        });
                    });
                }
                ConnectionFileEvent::Listed { path, entries } => {
                    self.current_path = path.clone();
                    self.set_path_text(path);
                    self.entries = entries;
                    self.rebuild_sorted_entries();
                    // 切换目录或刷新完成后列表内容已经变化，旧滚动位置和拖动状态不能继续复用。
                    self.list_scroll_handle = UniformListScrollHandle::new();
                    self.list_scrollbar_drag = None;
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
        let next = self.path_input.text.trim().to_string();
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
        self.set_path_text(parent.clone());
        self.status_message = Some("正在读取目录...".to_string());
        self.send_command(ConnectionFileCommand::List { path: parent });
        self.schedule_poll(context);
        context.notify();
    }

    /// 处理路径栏按键。
    fn handle_path_key_down(&mut self, event: &KeyDownEvent, context: &mut Context<Self>) {
        if MainView::is_paste_keystroke(&event.keystroke) {
            if let Some(text) = context.read_from_clipboard().and_then(|item| item.text()) {
                replace_text_input_selection(
                    &mut self.path_input,
                    &sanitize_file_manager_path_text(&text),
                );
                self.path_layout = None;
                self.touch_path_input_cursor_activity();
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
                self.set_path_text(self.current_path.clone());
                context.stop_propagation();
                context.notify();
            }
            _ => {
                if MainView::is_copy_keystroke(&event.keystroke) {
                    if let Some(text) = text_input_selected_text(&self.path_input) {
                        context.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                    context.stop_propagation();
                    return;
                }
                if MainView::is_cut_keystroke(&event.keystroke) {
                    if let Some(text) = text_input_selected_text(&self.path_input) {
                        context.write_to_clipboard(ClipboardItem::new_string(text));
                        replace_text_input_selection(&mut self.path_input, "");
                        self.path_layout = None;
                        self.touch_path_input_cursor_activity();
                    }
                    context.stop_propagation();
                    context.notify();
                    return;
                }
                if MainView::is_select_all_keystroke(&event.keystroke) {
                    select_all_text_input(&mut self.path_input);
                    self.touch_path_input_cursor_activity();
                    context.stop_propagation();
                    context.notify();
                    return;
                }

                let outcome = match event.keystroke.key.as_str() {
                    "left" => {
                        move_text_input_left(&mut self.path_input, event.keystroke.modifiers.shift)
                    }
                    "right" => {
                        move_text_input_right(&mut self.path_input, event.keystroke.modifiers.shift)
                    }
                    "home" | "up" => {
                        move_text_input_home(&mut self.path_input, event.keystroke.modifiers.shift)
                    }
                    "end" | "down" => {
                        move_text_input_end(&mut self.path_input, event.keystroke.modifiers.shift)
                    }
                    "backspace" => backspace_text_input(&mut self.path_input),
                    "delete" => delete_text_input(&mut self.path_input),
                    _ => TextInputEditOutcome::default(),
                };
                if outcome.consumed {
                    if outcome.changed {
                        self.path_layout = None;
                    }
                    self.touch_path_input_cursor_activity();
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
                self.set_path_text(entry.path.clone());
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

    /// 重建文件列表排序缓存。
    ///
    /// 业务意图：
    /// - 后端返回目录优先、名称升序的事实列表；UI 排序是当前窗口的展示状态。
    /// - 该方法只在数据源或排序条件变化时调用，避免虚拟列表滚动期间重复排序。
    fn rebuild_sorted_entries(&mut self) {
        self.sorted_entries = sorted_connection_file_entries(&self.entries, self.sort);
    }

    /// 返回当前可见 range 对应的排序目录项。
    ///
    /// 边界条件：
    /// - `uniform_list` 正常会传入合法 range；这里仍做边界夹紧，避免目录刷新与渲染批次交错时出现越界。
    /// - 返回可见项克隆，事件回调可以安全持有条目，不借用窗口状态跨过 GPUI 元素构造过程。
    fn sorted_entries_in_range(&self, range: std::ops::Range<usize>) -> Vec<ConnectionFileEntry> {
        let start = range.start.min(self.sorted_entries.len());
        let end = range.end.min(self.sorted_entries.len());
        if start >= end {
            return Vec::new();
        }
        self.sorted_entries[start..end].to_vec()
    }

    /// 点击表头后更新文件列表排序。
    ///
    /// 边界条件：
    /// - 点击同一列在升序/降序之间切换。
    /// - 切换列时默认升序，并重置滚动条，避免用户停留在排序后列表深处看不到顶部结果。
    fn sort_by_column(&mut self, column: ConnectionFileSortColumn, context: &mut Context<Self>) {
        let ascending = if self.sort.column == column {
            !self.sort.ascending
        } else {
            true
        };
        self.sort = ConnectionFileSortState { column, ascending };
        self.rebuild_sorted_entries();
        self.list_scroll_handle = UniformListScrollHandle::new();
        self.list_scrollbar_drag = None;
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
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, _event: &MouseDownEvent, window, context| {
                    window.focus(&view.path_focus);
                    context.stop_propagation();
                }),
            )
            .child(TextInputElement {
                view: context.entity(),
                binding: TextInputBinding::FileManagerPath,
                focus_handle: self.path_focus.clone(),
                placeholder: "输入路径",
                palette,
            })
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
            .w_full()
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
                    // 名称是文件管理中最需要完整展示的字段，因此默认占用除固定信息列外的剩余宽度。
                    .flex_1()
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
                    .w(px(180.0))
                    .flex_none()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_size(px(12.0))
                    .text_color(rgb(palette.muted_text))
                    .child(format_file_time(entry.modified_at_ms)),
            )
    }

    /// 渲染文件列表表头。
    ///
    /// 业务意图：
    /// - 表头与文件行使用相同列宽，点击列名即可改变当前窗口内的排序方式。
    /// - 当前排序列显示方向图标，帮助用户判断正在使用升序还是降序。
    fn render_file_table_header(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("connection-file-list-header")
            .flex()
            .items_center()
            .flex_none()
            .w_full()
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
            .child(self.render_file_table_header_cell(
                "名称",
                ConnectionFileSortColumn::Name,
                None,
                palette,
                context,
            ))
            .child(self.render_file_table_header_cell(
                "类型",
                ConnectionFileSortColumn::Kind,
                Some(72.0),
                palette,
                context,
            ))
            .child(self.render_file_table_header_cell(
                "大小",
                ConnectionFileSortColumn::Size,
                Some(92.0),
                palette,
                context,
            ))
            .child(self.render_file_table_header_cell(
                "修改时间",
                ConnectionFileSortColumn::ModifiedAt,
                Some(180.0),
                palette,
                context,
            ))
    }

    /// 渲染单个可排序表头单元格。
    fn render_file_table_header_cell(
        &self,
        label: &'static str,
        column: ConnectionFileSortColumn,
        fixed_width: Option<f32>,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let active = self.sort.column == column;
        let ascending = self.sort.ascending;
        div()
            .id(SharedString::from(format!(
                "connection-file-header-{label}"
            )))
            .when_some(fixed_width, |cell, width| cell.w(px(width)).flex_none())
            .when(fixed_width.is_none(), |cell| cell.flex_1().min_w_0())
            .h_full()
            .flex()
            .items_center()
            .justify_between()
            .gap_1()
            .overflow_hidden()
            .cursor_pointer()
            .hover(move |cell| cell.bg(rgb(palette.hover)))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                    view.sort_by_column(column, context);
                    context.stop_propagation();
                }),
            )
            .child(div().truncate().child(label))
            .when(active, |cell| {
                cell.child(MainView::render_lucide_icon(
                    Some(if ascending {
                        Icon::ChevronUp
                    } else {
                        Icon::ChevronDown
                    }),
                    12.0,
                    12.0,
                    palette.accent,
                ))
            })
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

    /// 渲染窗口底部状态栏。
    ///
    /// 业务意图：
    /// - 文件列表需要尽量占据中间可视区域，目录刷新、错误和传输状态统一停靠在窗口底部。
    /// - 上传/下载进度也属于状态反馈，放在底部可以避免列表高度在顶部反复跳动。
    fn render_status_bar(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let message = self
            .error_message
            .as_ref()
            .or(self.status_message.as_ref())
            .cloned()
            .unwrap_or_else(|| "就绪".to_string());
        let is_error = self.error_message.is_some();

        div()
            .id("connection-file-status-bar")
            .flex()
            .flex_col()
            .flex_none()
            .border_t_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .child(self.render_transfer_progress(palette, context))
            .child(
                div()
                    .id("connection-file-status-line")
                    .h(px(28.0))
                    .px_3()
                    .flex()
                    .items_center()
                    .text_size(px(12.0))
                    .text_color(rgb(if is_error {
                        palette.error
                    } else {
                        palette.muted_text
                    }))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(message),
            )
    }

    /// 渲染文件列表自绘纵向滚动条。
    ///
    /// 业务意图：
    /// - 原生滚动条在部分系统设置下只在滚动瞬间出现，文件很多时用户无法确认列表是否还能继续向下滚动。
    /// - 自绘滑块与虚拟列表使用同一个 `UniformListScrollHandle`，滚轮、拖动和触控板位置保持一致。
    fn render_file_list_scrollbar(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(metrics) = self.file_list_scrollbar_metrics() else {
            return div().id("connection-file-list-scrollbar-empty").hidden();
        };

        div()
            .id("connection-file-list-scrollbar")
            .absolute()
            .top(metrics.thumb_start)
            .right(px(CONNECTION_FILE_LIST_SCROLLBAR_PADDING))
            .w(px(CONNECTION_FILE_LIST_SCROLLBAR_WIDTH))
            .h(metrics.thumb_length)
            .rounded(px(CONNECTION_FILE_LIST_SCROLLBAR_WIDTH / 2.0))
            .bg(rgb(palette.scrollbar))
            .cursor_pointer()
            .hover(move |thumb| thumb.bg(rgb(palette.scrollbar_hover)))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, event: &MouseDownEvent, _window, context| {
                    view.start_file_list_scrollbar_drag(event, context);
                    context.stop_propagation();
                }),
            )
    }

    /// 计算文件列表滚动条滑块位置。
    fn file_list_scrollbar_metrics(&self) -> Option<LogScrollbarMetrics> {
        let state = self.list_scroll_handle.0.borrow();
        let size = state.last_item_size?;
        let viewport_height = size.item.height;
        let content_height = size.contents.height;
        if viewport_height <= px(0.0) || content_height <= viewport_height {
            return None;
        }

        let max_scroll = content_height - viewport_height;
        let scroll_top = (-state.base_handle.offset().y).clamp(px(0.0), max_scroll);
        let track_start = px(CONNECTION_FILE_LIST_SCROLLBAR_PADDING);
        let track_length = (viewport_height - track_start * 2.0).max(px(1.0));
        let min_thumb_length =
            px(CONNECTION_FILE_LIST_SCROLLBAR_MIN_THUMB_HEIGHT).min(track_length);
        let thumb_length = (viewport_height * (viewport_height / content_height))
            .clamp(min_thumb_length, track_length);
        let movable_length = (track_length - thumb_length).max(px(0.0));
        let thumb_start = track_start + movable_length * (scroll_top / max_scroll);

        Some(LogScrollbarMetrics {
            thumb_start,
            thumb_length,
            track_start,
            track_length,
            max_scroll,
            max_scroll_px: f64::from(max_scroll),
        })
    }

    /// 开始拖动文件列表滚动条。
    fn start_file_list_scrollbar_drag(
        &mut self,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let Some(metrics) = self.file_list_scrollbar_metrics() else {
            return;
        };
        if metrics.max_scroll <= px(0.0) {
            return;
        }
        let bounds = self.list_scroll_handle.0.borrow().base_handle.bounds();
        if bounds.size.height <= px(0.0) {
            return;
        }
        self.list_scrollbar_drag = Some(ConnectionFileListScrollbarDrag {
            cursor_offset: event.position.y - bounds.top() - metrics.thumb_start,
        });
        context.notify();
    }

    /// 根据鼠标移动更新文件列表滚动条拖动。
    fn update_file_list_scrollbar_drag(
        &mut self,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        let Some(drag) = self.list_scrollbar_drag else {
            return;
        };
        if !event.dragging() {
            self.finish_file_list_scrollbar_drag(context);
            return;
        }
        let Some(metrics) = self.file_list_scrollbar_metrics() else {
            self.finish_file_list_scrollbar_drag(context);
            return;
        };
        let base_scroll_handle = {
            // 先克隆底层 ScrollHandle 再释放 RefCell 借用，避免 set_offset 时触发嵌套借用。
            self.list_scroll_handle.0.borrow().base_handle.clone()
        };
        let bounds = base_scroll_handle.bounds();
        let movable_length = (metrics.track_length - metrics.thumb_length).max(px(0.0));
        if metrics.max_scroll <= px(0.0)
            || movable_length <= px(0.0)
            || bounds.size.height <= px(0.0)
        {
            return;
        }

        let requested_thumb_start = event.position.y - bounds.top() - drag.cursor_offset;
        let thumb_start =
            requested_thumb_start.clamp(metrics.track_start, metrics.track_start + movable_length);
        let scroll_ratio = f64::from((thumb_start - metrics.track_start) / movable_length);
        let scroll_offset = px((metrics.max_scroll_px * scroll_ratio) as f32);
        let current_offset = base_scroll_handle.offset();
        base_scroll_handle.set_offset(point(current_offset.x, -scroll_offset));
        context.notify();
    }

    /// 结束文件列表滚动条拖动。
    fn finish_file_list_scrollbar_drag(&mut self, context: &mut Context<Self>) {
        if self.list_scrollbar_drag.is_some() {
            self.list_scrollbar_drag = None;
            context.notify();
        }
    }

    /// 处理文件管理窗口鼠标移动。
    fn handle_window_mouse_move(&mut self, event: &MouseMoveEvent, context: &mut Context<Self>) {
        if self.list_scrollbar_drag.is_some() {
            self.update_file_list_scrollbar_drag(event, context);
            context.stop_propagation();
        }
    }

    /// 处理文件管理窗口鼠标释放。
    fn handle_window_mouse_up(&mut self, context: &mut Context<Self>) {
        if self.list_scrollbar_drag.is_some() {
            self.finish_file_list_scrollbar_drag(context);
            context.stop_propagation();
        }
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

impl ConnectionFileManagerWindowView {
    /// 将窗口坐标换算为路径栏真实文本的 UTF-8 字节下标。
    ///
    /// 边界条件：
    /// - 地址栏第一次绘制前还没有字形布局，此时回退到文本末尾，保证点击不会 panic。
    /// - 命中计算需要加入水平滚动偏移，否则长路径滚动后鼠标位置会映射到错误字符。
    fn path_input_index_for_point(&self, position: Point<Pixels>) -> usize {
        let text = &self.path_input.text;
        let Some(layout) = self.path_layout.as_ref() else {
            return text.len();
        };
        if position.y < layout.bounds.top() {
            return 0;
        }
        if position.y > layout.bounds.bottom() {
            return text.len();
        }
        let display_index = layout.line.closest_index_for_x(
            position.x - layout.bounds.left() + px(layout.horizontal_scroll_px),
        );
        text_input_clamp_byte_index(text, display_index.min(text.len()))
    }

    /// 返回 UTF-16 范围对应的窗口坐标，用于平台 IME 候选窗口定位。
    ///
    /// 实现原因：
    /// - GPUI 平台输入协议使用 UTF-16 范围，而地址栏内部保存 UTF-8 字符串。
    /// - 候选窗口必须跟随水平滚动后的光标位置，否则中文输入时候选栏会偏离可见光标。
    fn path_input_bounds_for_utf16_range(
        &self,
        range_utf16: Range<usize>,
        fallback_bounds: Bounds<Pixels>,
    ) -> Bounds<Pixels> {
        let Some(layout) = self.path_layout.as_ref() else {
            return fallback_bounds;
        };
        let range = text_input_range_from_utf16(&self.path_input.text, range_utf16);
        let start_x = f32::from(
            layout.bounds.left() - px(layout.horizontal_scroll_px)
                + layout.line.x_for_index(range.start),
        )
        .clamp(
            f32::from(layout.bounds.left()),
            f32::from(layout.bounds.right()),
        );
        let end_x = f32::from(
            layout.bounds.left() - px(layout.horizontal_scroll_px)
                + layout.line.x_for_index(range.end),
        )
        .clamp(
            f32::from(layout.bounds.left()),
            f32::from(layout.bounds.right()),
        );
        Bounds::from_corners(
            point(px(start_x.min(end_x)), layout.bounds.top()),
            point(
                px(start_x.max(end_x).max(start_x.min(end_x) + 1.0)),
                layout.bounds.bottom(),
            ),
        )
    }

    /// 将文件管理地址栏水平滚动同步到当前选区焦点。
    ///
    /// 业务意图：
    /// - 地址栏长路径常见于 SSH/SMB 深层目录，用户拖拽选中并越过可视边界时应自动横向滚动。
    /// - 文件管理窗口是独立 `Entity`，不能依赖主窗口的输入框状态同步逻辑，因此在这里复用同一套滚动计算。
    ///
    /// 边界条件：
    /// - 地址栏尚未完成一次布局时跳过；下一帧 `TextInputElement` 会根据光标位置重新夹紧滚动。
    fn sync_path_input_scroll_to_cursor(&mut self) {
        let Some(layout) = self.path_layout.as_ref() else {
            return;
        };
        let cursor_index = text_input_clamp_range(
            &self.path_input.text,
            self.path_input.selection_range.clone(),
        )
        .end;
        let content_width = if self.path_input.text.is_empty() {
            px(0.0)
        } else {
            layout.line.x_for_index(self.path_input.text.len())
        };
        self.path_input.horizontal_scroll_px = text_input_horizontal_scroll_offset(
            self.path_input.horizontal_scroll_px,
            layout.line.x_for_index(cursor_index),
            content_width,
            layout.bounds.size.width,
            true,
        );
    }

    /// 用平台提交文本替换地址栏选区。
    fn replace_path_input_range(&mut self, range_utf16: Option<Range<usize>>, text: &str) {
        let replacement = sanitize_file_manager_path_text(text);
        let range = range_utf16
            .map(|range| text_input_range_from_utf16(&self.path_input.text, range))
            .or_else(|| self.path_input.marked_range.clone())
            .unwrap_or_else(|| self.path_input.selection_range.clone());
        let range = text_input_clamp_range(&self.path_input.text, range);
        self.path_input
            .text
            .replace_range(range.clone(), &replacement);
        let cursor = range.start + replacement.len();
        self.path_input.selection_range = cursor..cursor;
        self.path_input.marked_range = None;
        self.path_input.selection_drag = None;
        self.path_input.horizontal_scroll_px = 0.0;
        self.path_layout = None;
        self.touch_path_input_cursor_activity();
    }

    /// 用平台组合文本替换地址栏选区并保留 marked 范围。
    fn replace_and_mark_path_input_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
    ) {
        let replacement = sanitize_file_manager_path_text(new_text);
        let range = range_utf16
            .map(|range| text_input_range_from_utf16(&self.path_input.text, range))
            .or_else(|| self.path_input.marked_range.clone())
            .unwrap_or_else(|| self.path_input.selection_range.clone());
        let range = text_input_clamp_range(&self.path_input.text, range);
        self.path_input
            .text
            .replace_range(range.clone(), &replacement);
        if replacement.is_empty() {
            self.path_input.marked_range = None;
        } else {
            self.path_input.marked_range = Some(range.start..range.start + replacement.len());
        }
        self.path_input.selection_range = new_selected_range_utf16
            .map(|utf16_range| text_input_range_from_utf16(&replacement, utf16_range))
            .map(|relative_range| {
                range.start + relative_range.start..range.start + relative_range.end
            })
            .unwrap_or_else(|| {
                let cursor = range.start + replacement.len();
                cursor..cursor
            });
        self.path_input.selection_drag = None;
        self.path_input.horizontal_scroll_px = 0.0;
        self.path_layout = None;
        self.touch_path_input_cursor_activity();
    }
}

impl EntityInputHandler for ConnectionFileManagerWindowView {
    /// 返回地址栏指定 UTF-16 范围的文本。
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<String> {
        if !self.path_focus.is_focused(window) {
            return None;
        }
        let range = text_input_range_from_utf16(&self.path_input.text, range_utf16);
        adjusted_range.replace(text_input_range_to_utf16(
            &self.path_input.text,
            range.clone(),
        ));
        Some(self.path_input.text[range].to_string())
    }

    /// 返回地址栏当前选区。
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        self.path_focus.is_focused(window).then(|| UTF16Selection {
            range: text_input_range_to_utf16(
                &self.path_input.text,
                self.path_input.selection_range.clone(),
            ),
            reversed: false,
        })
    }

    /// 返回地址栏当前 IME 组合范围。
    fn marked_text_range(
        &self,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        if !self.path_focus.is_focused(window) {
            return None;
        }
        self.path_input
            .marked_range
            .clone()
            .map(|range| text_input_range_to_utf16(&self.path_input.text, range))
    }

    /// 清理地址栏 IME 组合范围。
    fn unmark_text(&mut self, window: &mut Window, context: &mut Context<Self>) {
        if !self.path_focus.is_focused(window) {
            return;
        }
        self.path_input.marked_range = None;
        self.path_input.selection_drag = None;
        self.touch_path_input_cursor_activity();
        context.notify();
    }

    /// 提交普通文本到地址栏。
    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if !self.path_focus.is_focused(window) {
            return;
        }
        self.replace_path_input_range(range_utf16, text);
        context.notify();
    }

    /// 更新地址栏 IME 组合文本。
    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if !self.path_focus.is_focused(window) {
            return;
        }
        self.replace_and_mark_path_input_range(range_utf16, new_text, new_selected_range_utf16);
        context.notify();
    }

    /// 返回地址栏指定范围的候选窗口锚点。
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(self.path_input_bounds_for_utf16_range(range_utf16, element_bounds))
    }

    /// 返回点击位置对应的 UTF-16 下标。
    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<usize> {
        let index = self.path_input_index_for_point(point);
        Some(text_input_utf16_offset_from_byte(
            &self.path_input.text,
            index,
        ))
    }
}

impl TextInputElementHost for ConnectionFileManagerWindowView {
    /// 读取文件管理地址栏的通用输入快照。
    fn text_input_snapshot(&self, binding: TextInputBinding) -> Option<TextInputSnapshot> {
        (binding == TextInputBinding::FileManagerPath).then(|| {
            TextInputSnapshot::from_single_line(
                SingleLineTextInputSnapshot {
                    text: self.path_input.text.clone(),
                    selection_range: self.path_input.selection_range.clone(),
                    marked_range: self.path_input.marked_range.clone(),
                    horizontal_scroll_px: self.path_input.horizontal_scroll_px,
                },
                TextInputDisplayMode::Plain,
            )
        })
    }

    /// 保存文件管理地址栏最近一次布局。
    fn store_text_input_layout(
        &mut self,
        binding: TextInputBinding,
        line: ShapedLine,
        bounds: Bounds<Pixels>,
        horizontal_scroll_px: f32,
    ) {
        if binding != TextInputBinding::FileManagerPath {
            return;
        }
        self.path_input.horizontal_scroll_px = horizontal_scroll_px;
        self.path_layout = Some(ConnectionFilePathInputLayout {
            line,
            bounds,
            horizontal_scroll_px,
        });
    }

    /// 地址栏鼠标按下时定位光标或开始拖拽选区。
    fn begin_text_input_mouse_interaction(
        &mut self,
        binding: TextInputBinding,
        event: &MouseDownEvent,
        _was_focused: bool,
        context: &mut Context<Self>,
    ) -> bool {
        if binding != TextInputBinding::FileManagerPath {
            return false;
        }
        let index = self.path_input_index_for_point(event.position);
        begin_text_input_mouse_selection(
            &mut self.path_input,
            index,
            event.click_count,
            event.modifiers.shift,
        );
        self.touch_path_input_cursor_activity();
        context.notify();
        true
    }

    /// 地址栏鼠标拖拽时扩展选区。
    fn update_text_input_mouse_drag(
        &mut self,
        binding: TextInputBinding,
        position: Point<Pixels>,
        context: &mut Context<Self>,
    ) -> bool {
        if binding != TextInputBinding::FileManagerPath {
            return false;
        }
        let Some(anchor) = self.path_input.selection_drag else {
            return false;
        };
        let index = self.path_input_index_for_point(position);
        update_text_input_mouse_selection(&mut self.path_input, anchor, index);
        self.sync_path_input_scroll_to_cursor();
        self.touch_path_input_cursor_activity();
        context.notify();
        true
    }

    /// 地址栏鼠标释放时结束拖拽生命周期。
    fn finish_text_input_mouse_drag(
        &mut self,
        binding: TextInputBinding,
        context: &mut Context<Self>,
    ) -> bool {
        if binding != TextInputBinding::FileManagerPath {
            return false;
        }
        let handled = self.path_input.selection_drag.take().is_some();
        if handled {
            context.notify();
        }
        handled
    }

    /// 文件管理地址栏沿用主窗口通用输入框的闪烁节奏。
    fn text_input_cursor_visible(&self) -> bool {
        MainView::search_text_cursor_visible_for_elapsed(self.path_cursor_last_activity.elapsed())
    }
}

impl Render for ConnectionFileManagerWindowView {
    /// 渲染文件管理窗口。
    fn render(&mut self, window: &mut Window, context: &mut Context<Self>) -> impl IntoElement {
        self.drain_events(context);
        let palette = self.main_view.read(context).palette();
        let selected_has_files = !self.selected_file_paths().is_empty();
        let row_count = self.sorted_entries.len();
        let scroll_handle = self.list_scroll_handle.clone();

        div()
            .id("connection-file-manager-window")
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(palette.background))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, event: &MouseDownEvent, window, context| {
                    view.blur_path_input_if_click_outside(event, window, context);
                }),
            )
            .on_mouse_move(
                context.listener(|view, event: &MouseMoveEvent, _window, context| {
                    view.handle_window_mouse_move(event, context);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                context.listener(|view, _event: &MouseUpEvent, _window, context| {
                    view.handle_window_mouse_up(context);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                context.listener(|view, _event: &MouseUpEvent, _window, context| {
                    view.handle_window_mouse_up(context);
                }),
            )
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
            .child(
                div()
                    .id("connection-file-table")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .child(self.render_file_table_header(palette, context))
                    .child(
                        div()
                            .id("connection-file-list-wrapper")
                            .relative()
                            .flex()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .overflow_hidden()
                            .child(if row_count == 0 {
                                div()
                                    .id("connection-file-list-empty")
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size_full()
                                    .text_size(px(13.0))
                                    .text_color(rgb(palette.muted_text))
                                    .child("当前目录为空")
                                    .into_any_element()
                            } else {
                                uniform_list(
                                    "connection-file-list",
                                    row_count,
                                    context.processor(
                                        move |view,
                                              range: std::ops::Range<usize>,
                                              _window,
                                              context| {
                                            let palette = view.main_view.read(context).palette();
                                            view.sorted_entries_in_range(range)
                                                .into_iter()
                                                .map(|entry| {
                                                    view.render_file_entry_row(
                                                        entry, palette, context,
                                                    )
                                                    .into_any_element()
                                                })
                                                .collect::<Vec<_>>()
                                        },
                                    ),
                                )
                                .size_full()
                                .track_scroll(scroll_handle)
                                .into_any_element()
                            })
                            .child(self.render_file_list_scrollbar(palette, context)),
                    ),
            )
            .child(self.render_status_bar(palette, context))
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

/// 按 UI 排序状态生成文件列表展示顺序。
///
/// 业务意图：
/// - 文件管理后端只负责返回目录项事实，排序属于当前窗口的展示偏好。
/// - 目录优先是文件管理器的基础可用性要求，即使用户按大小或时间排序，也不应把目录混入文件中间。
fn sorted_connection_file_entries(
    entries: &[ConnectionFileEntry],
    sort: ConnectionFileSortState,
) -> Vec<ConnectionFileEntry> {
    let mut entries = entries.to_vec();
    entries.sort_by(|left, right| {
        let directory_order = right.kind.is_directory().cmp(&left.kind.is_directory());
        if directory_order != std::cmp::Ordering::Equal {
            return directory_order;
        }

        let column_order = match sort.column {
            ConnectionFileSortColumn::Name => compare_file_names(&left.name, &right.name),
            ConnectionFileSortColumn::Kind => file_kind_sort_key(left.kind)
                .cmp(&file_kind_sort_key(right.kind))
                .then_with(|| compare_file_names(&left.name, &right.name)),
            ConnectionFileSortColumn::Size => left
                .size
                .unwrap_or(0)
                .cmp(&right.size.unwrap_or(0))
                .then_with(|| compare_file_names(&left.name, &right.name)),
            ConnectionFileSortColumn::ModifiedAt => left
                .modified_at_ms
                .unwrap_or(i64::MIN)
                .cmp(&right.modified_at_ms.unwrap_or(i64::MIN))
                .then_with(|| compare_file_names(&left.name, &right.name)),
        };

        if sort.ascending {
            column_order
        } else {
            column_order.reverse()
        }
    });
    entries
}

/// 文件名排序使用大小写不敏感比较，并用原始名称作为稳定兜底。
fn compare_file_names(left: &str, right: &str) -> std::cmp::Ordering {
    left.to_lowercase()
        .cmp(&right.to_lowercase())
        .then_with(|| left.cmp(right))
}

/// 文件类型排序键。
///
/// 边界条件：
/// - 目录优先已经在外层处理；这里仍给出目录键值，方便测试和未来取消目录优先时保持确定顺序。
fn file_kind_sort_key(kind: ConnectionFileEntryKind) -> u8 {
    match kind {
        ConnectionFileEntryKind::Directory => 0,
        ConnectionFileEntryKind::File => 1,
        ConnectionFileEntryKind::Symlink => 2,
        ConnectionFileEntryKind::Other => 3,
    }
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

    /// 验证文件列表排序始终保持目录优先，并按所选列切换顺序。
    #[test]
    fn 文件列表排序保持目录优先并支持大小降序() {
        let entries = vec![
            ConnectionFileEntry {
                name: "small.log".to_string(),
                path: "/small.log".to_string(),
                kind: ConnectionFileEntryKind::File,
                size: Some(10),
                modified_at_ms: Some(20),
            },
            ConnectionFileEntry {
                name: "folder".to_string(),
                path: "/folder".to_string(),
                kind: ConnectionFileEntryKind::Directory,
                size: None,
                modified_at_ms: Some(1),
            },
            ConnectionFileEntry {
                name: "large.log".to_string(),
                path: "/large.log".to_string(),
                kind: ConnectionFileEntryKind::File,
                size: Some(100),
                modified_at_ms: Some(10),
            },
        ];

        let sorted = sorted_connection_file_entries(
            &entries,
            ConnectionFileSortState {
                column: ConnectionFileSortColumn::Size,
                ascending: false,
            },
        );

        assert_eq!(sorted[0].name, "folder");
        assert_eq!(sorted[1].name, "large.log");
        assert_eq!(sorted[2].name, "small.log");
    }
}
