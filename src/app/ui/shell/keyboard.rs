// 主窗口键盘快捷键与滚动协调。
//
// 业务意图：
// - 本文件由原主窗口工作区实现机械拆分而来，只承载 GPUI 壳层内的 全局快捷键、键盘滚动区域和标记行跳转。
// - 拆分过程保持所有状态字段、事件时序和用户可见行为不变，便于后续按视图职责继续收束。

use super::*;

impl MainView {
    /// 处理全局键盘快捷键。
    ///
    /// 业务意图：
    /// - 搜索是日志查看器的核心工作流，需要即使焦点停在日志正文或目录树上也能通过 `Ctrl+F` 打开。
    /// - macOS 用户通常使用 `Cmd+F`，因此在保留用户要求的 `Ctrl+F` 同时支持平台键。
    /// - 该函数返回是否消费了快捷键，调用方据此阻止事件继续传给平台菜单或底层输入控件。
    ///
    /// 边界条件：
    /// - 这里不处理普通字符输入，避免全局监听截获搜索框或未来编辑控件的文本输入。
    /// - Enter 仅在搜索对话框打开时触发搜索，Esc 仅关闭搜索对话框。
    pub(in crate::app) fn handle_global_keystroke(
        &mut self,
        keystroke: Keystroke,
        window: &mut Window,
        context: &mut Context<Self>,
        allow_keyboard_scroll: bool,
    ) -> bool {
        let note_editor_input_focused = self.note_editor_input_focused(window, context);
        if note_editor_input_focused && Self::is_save_keystroke(&keystroke) {
            return self.save_active_note(context);
        }

        let editable_text_input_focused =
            self.editable_text_input_focused(window, context, note_editor_input_focused);
        if editable_text_input_focused
            && (Self::is_copy_keystroke(&keystroke) || Self::is_paste_keystroke(&keystroke))
        {
            return false;
        }

        if self.navigation.active_main_feature == MainFeature::Notes
            && Self::is_copy_keystroke(&keystroke)
            && self.copy_selected_note_source_text(context)
        {
            return true;
        }

        if self.navigation.active_main_feature == MainFeature::AiChat
            && Self::is_copy_keystroke(&keystroke)
            && self.copy_selected_ai_chat_message_text(context)
        {
            return true;
        }

        if Self::is_copy_keystroke(&keystroke) && self.copy_selected_log_text(context) {
            return true;
        }

        if Self::is_paste_keystroke(&keystroke)
            && self.paste_clipboard_text_into_search_dialog(window, context)
        {
            return true;
        }

        if self.search.search_dialog.is_some()
            && let Some(control_key) = Self::search_dialog_control_key(&keystroke)
        {
            if self.settings_text_input_focused(window) {
                return false;
            }
            match control_key {
                SearchDialogControlKey::Submit => self.start_search(context),
                SearchDialogControlKey::Close => self.close_search_dialog(window, context),
            }
            return true;
        }

        if Self::marker_jump_keystroke_for_focus(&keystroke, editable_text_input_focused)
            && self.jump_to_next_marked_line(context)
        {
            return true;
        }

        if allow_keyboard_scroll
            && let Some(command) =
                Self::keyboard_scroll_command_for_focus(&keystroke, editable_text_input_focused)
        {
            return self.handle_keyboard_scroll(command, context);
        }

        false
    }

    /// 处理主窗口根节点收到的键盘事件。
    ///
    /// 业务意图：
    /// - macOS 的 `Cmd+C` 可能走应用级 key equivalent，Windows 或部分焦点状态下的 `Ctrl+C` 则更可能走普通
    ///   `on_key_down`；日志正文是自绘只读列表，不能依赖系统文本控件自动复制。
    /// - 根节点复用全局快捷键处理逻辑，作为 `intercept_keystrokes` 的兜底入口，保证日志正文选区复制在不同平台路径下都有效。
    pub(in crate::app) fn handle_root_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if self.handle_global_keystroke(event.keystroke.clone(), window, context, true) {
            context.stop_propagation();
            context.notify();
        }
    }

    /// 判断当前焦点是否位于搜索窗口的任一文本输入框。
    ///
    /// 业务意图：
    /// - GPUI 的全局快捷键拦截可能早于输入框 `on_key_down` 触发；当关键字或目录输入框聚焦时，
    ///   `Ctrl+C` / `Ctrl+V` 必须优先交给输入框自身处理，不能被日志查看器的复制/粘贴搜索逻辑抢走。
    ///
    /// 边界条件：
    /// - 搜索窗口未打开时两个焦点句柄都不会命中，此时全局复制仍可服务日志正文选区。
    pub(in crate::app) fn search_text_input_focused(&self, window: &Window) -> bool {
        self.search.search_input_focus.is_focused(window)
            || self.search.search_directory_focus.is_focused(window)
    }

    /// 判断焦点是否位于设置窗口内的自绘文本输入框。
    ///
    /// 业务意图：
    /// - 搜索窗口打开时，全局 `Enter`/`Escape` 仍会生效；如果用户正在设置页编辑线程过滤或快搜关键字，
    ///   这些按键必须优先交给设置输入框，不能误触发搜索或关闭搜索窗口。
    pub(in crate::app) fn settings_text_input_focused(&self, window: &Window) -> bool {
        self.settings
            .thread_analysis_filter_focus
            .is_focused(window)
            || self.settings.quick_search_keywords_focus.is_focused(window)
            || self.active_model_config_input_kind(window).is_some()
    }

    /// 判断当前焦点是否位于笔记正文组件输入框。
    ///
    /// 业务意图：
    /// - 笔记正文使用项目内富文本编辑器，焦点状态保存在正文自己的 `FocusHandle` 上。
    /// - 该方法只处理笔记正文，搜索框、设置框和标题框仍通过各自的 GPUI 焦点句柄判断。
    pub(in crate::app) fn note_editor_input_focused(
        &self,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> bool {
        // 只读阅读器也会复用正文焦点来支持鼠标选区和复制，但它不是可编辑输入框。
        // 如果这里不区分编辑态，全局 `Ctrl/Cmd+C` 会跳过笔记只读选区复制，
        // `Ctrl/Cmd+S` 也可能在只读状态触发写库并把旧格式笔记静默转换为富文本。
        self.notes.is_editing && self.notes.rich_editor.focus.is_focused(window)
    }

    /// 判断当前焦点是否位于应用内自绘或组件库提供的可编辑文本输入框。
    ///
    /// 业务意图：
    /// - 笔记正文已经迁移到项目内富文本输入框，焦点由正文 `FocusHandle` 维护；搜索、设置和标题仍沿用项目内自绘输入框焦点。
    /// - 调用方预先传入正文焦点结果，避免同一次快捷键处理重复读取窗口根状态。
    pub(in crate::app) fn editable_text_input_focused(
        &self,
        window: &Window,
        _context: &mut Context<Self>,
        note_editor_input_focused: bool,
    ) -> bool {
        self.search_text_input_focused(window)
            || self.log.log_tree_search.focus.is_focused(window)
            || self.settings_text_input_focused(window)
            || self.ai_chat.input_focus.is_focused(window)
            || self.notes.title_focus.is_focused(window)
            || note_editor_input_focused
    }

    /// 延迟打开搜索对话框。
    ///
    /// 业务意图：
    /// - macOS 会把 `Cmd+F`、`Ctrl+F` 这类快捷键先作为 key equivalent 分发；如果在该原生回调栈里直接创建窗口，
    ///   GPUI 或平台窗口系统内部一旦 panic，就会跨 `extern "C"` 边界触发不可恢复 abort。
    /// - 这里使用 `Window::defer` 而不是 `Context::defer_in`：前者回调拿到的是 `App`，不会自动重新租借
    ///   `MainView`；后者会在闭包外包一层 `MainView::update`，导致搜索窗口读取 `MainView` 时发生重复借用。
    ///
    /// 边界条件：
    /// - 延迟执行仍读取当前日志选区，因此用户按下快捷键时已有的选中文本会正常预填到搜索框。
    /// - 同一帧内重复触发只保留一个打开请求，避免按键重放创建多个搜索窗口。
    pub(in crate::app) fn schedule_open_search_dialog(
        &mut self,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.schedule_open_search_dialog_with_preset(
            SearchDialogOpenPreset::Default,
            window,
            context,
        );
    }

    /// 按指定预设延迟打开搜索对话框。
    ///
    /// 业务意图：
    /// - 左侧目录树“选中搜索”需要把右键时的文件集合传递到下一帧创建的搜索窗口。
    /// - 默认工具栏和快捷键入口继续使用默认预设，保持原有当前文件搜索行为。
    pub(in crate::app) fn schedule_open_search_dialog_with_preset(
        &mut self,
        preset: SearchDialogOpenPreset,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if self.search.search_dialog_open_pending {
            if !matches!(preset, SearchDialogOpenPreset::Default) {
                self.search.search_dialog_open_preset = preset;
            }
            return;
        }

        self.search.search_dialog_open_pending = true;
        self.search.search_dialog_open_preset = preset;
        let main_view = context.entity();
        window.defer(context, move |window, app| {
            Self::open_search_dialog_after_main_update(main_view, window, app);
        });
        context.notify();
    }

    /// 判断是否为复制日志选中文本的快捷键。
    ///
    /// 业务意图：
    /// - macOS 用户习惯 `Cmd+C`，Windows 用户习惯 `Ctrl+C`，两者都应复制当前日志正文选区。
    /// - 当前只在日志正文有非空选择时消费该快捷键；没有选择时保持后续控件自己的复制行为空间。
    pub(in crate::app) fn is_copy_keystroke(keystroke: &Keystroke) -> bool {
        Self::keystroke_matches_letter_or_control_code(keystroke, "c", CONTROL_C_CODE)
            && (keystroke.modifiers.control
                || keystroke.modifiers.platform
                || Self::keystroke_matches_control_code(keystroke, CONTROL_C_CODE))
    }

    /// 判断是否为粘贴快捷键。
    ///
    /// 业务意图：
    /// - 搜索输入框和日志查看器都需要识别 `Ctrl+V` / `Cmd+V`。
    /// - 日志查看器是只读区域，粘贴行为会把剪贴板文本填入搜索框，避免用户按键后没有任何可见结果。
    pub(in crate::app) fn is_paste_keystroke(keystroke: &Keystroke) -> bool {
        Self::keystroke_matches_letter_or_control_code(keystroke, "v", CONTROL_V_CODE)
            && (keystroke.modifiers.control
                || keystroke.modifiers.platform
                || Self::keystroke_matches_control_code(keystroke, CONTROL_V_CODE))
    }

    /// 判断是否为剪切快捷键。
    ///
    /// 业务意图：
    /// - 设置页线程过滤输入区是可编辑文本，必须支持常见 `Ctrl+X` / `Cmd+X` 剪切行为。
    pub(in crate::app) fn is_cut_keystroke(keystroke: &Keystroke) -> bool {
        Self::keystroke_matches_letter_or_control_code(keystroke, "x", CONTROL_X_CODE)
            && (keystroke.modifiers.control
                || keystroke.modifiers.platform
                || Self::keystroke_matches_control_code(keystroke, CONTROL_X_CODE))
    }

    /// 判断按键是否是搜索窗口级控制键。
    ///
    /// 边界条件：
    /// - 这里只识别无文本意义的控制键，不处理 `Ctrl+F` 等打开窗口快捷键，也不处理普通字符输入。
    pub(in crate::app) fn search_dialog_control_key(
        keystroke: &Keystroke,
    ) -> Option<SearchDialogControlKey> {
        match keystroke.key.as_str() {
            "enter" => Some(SearchDialogControlKey::Submit),
            "escape" => Some(SearchDialogControlKey::Close),
            _ => None,
        }
    }

    /// 在鼠标进入或点击滚动区域时记录键盘滚动目标。
    ///
    /// 业务意图：
    /// - 用户可能先把鼠标移到搜索结果或目录树，再按 `PageDown`；此时滚动应落在鼠标关注的区域。
    /// - 该方法只更新内部路由状态，不触发重绘，避免高频 `mouse_move` 导致无意义刷新。
    pub(in crate::app) fn note_keyboard_scroll_region(&mut self, region: KeyboardScrollRegion) {
        self.navigation.keyboard_scroll_region = Some(region);
    }

    /// 返回当前键盘滚动目标，未记录时默认使用日志正文。
    ///
    /// 边界条件：
    /// - 启动后还没有鼠标进入任何滚动区时，默认日志正文可以保持日志查看器最常见的使用路径。
    pub(in crate::app) fn keyboard_scroll_region_or_default(
        region: Option<KeyboardScrollRegion>,
    ) -> KeyboardScrollRegion {
        region.unwrap_or(KeyboardScrollRegion::LogContent)
    }

    /// 根据焦点状态解析键盘滚动命令。
    ///
    /// 业务意图：
    /// - 可编辑文本框聚焦时，`PageUp`、`PageDown`、`Ctrl+Home` 和 `Ctrl+End` 属于编辑控件自己的导航行为，
    ///   主窗口不能拦截，否则会破坏搜索框、设置文本框和平台输入法体验。
    pub(in crate::app) fn keyboard_scroll_command_for_focus(
        keystroke: &Keystroke,
        editable_text_input_focused: bool,
    ) -> Option<KeyboardScrollCommand> {
        if editable_text_input_focused {
            return None;
        }

        Self::keyboard_scroll_command(keystroke)
    }

    /// 解析主窗口支持的键盘滚动快捷键。
    ///
    /// 业务意图：
    /// - `PageUp` 和 `PageDown` 使用裸按键，符合日志查看器中的滚动习惯。
    /// - 顶部和底部跳转只接受用户明确要求的 `Ctrl+Home` / `Ctrl+End`，不识别 `Ctrl+Top`。
    ///
    /// 跨平台约束：
    /// - GPUI 在不同系统或键盘路径下可能把按键名写成 `pageup`、`page_up` 或包含大小写差异，因此先做轻量规范化。
    /// - `Home` / `End` 只要求 Control 修饰键，不把 macOS Command 键混入，避免和平台级文本导航语义冲突。
    pub(in crate::app) fn keyboard_scroll_command(
        keystroke: &Keystroke,
    ) -> Option<KeyboardScrollCommand> {
        let key = Self::normalized_keyboard_key(&keystroke.key);
        let key_char = keystroke
            .key_char
            .as_deref()
            .map(Self::normalized_keyboard_key);
        let matches_key = |expected: &str| {
            key == expected || key_char.as_deref().is_some_and(|value| value == expected)
        };

        if !keystroke.modifiers.control && !keystroke.modifiers.platform && !keystroke.modifiers.alt
        {
            if matches_key("pageup") {
                return Some(KeyboardScrollCommand::PageUp);
            }
            if matches_key("pagedown") {
                return Some(KeyboardScrollCommand::PageDown);
            }
        }

        if keystroke.modifiers.control && !keystroke.modifiers.platform && !keystroke.modifiers.alt
        {
            if matches_key("home") {
                return Some(KeyboardScrollCommand::Top);
            }
            if matches_key("end") {
                return Some(KeyboardScrollCommand::Bottom);
            }
        }

        None
    }

    /// 根据焦点状态判断是否允许处理日志标记跳转快捷键。
    ///
    /// 业务意图：
    /// - `F2` 是日志正文的标记跳转入口，但用户在搜索框或设置输入框内编辑时，功能键应优先留给输入控件和系统。
    /// - 这里仅做按键解析，不检查是否真的存在标记；没有标记时由执行函数返回不消费，避免阻断其它默认行为。
    pub(in crate::app) fn marker_jump_keystroke_for_focus(
        keystroke: &Keystroke,
        editable_text_input_focused: bool,
    ) -> bool {
        !editable_text_input_focused && Self::marker_jump_keystroke(keystroke)
    }

    /// 判断是否为普通 `F2` 标记跳转快捷键。
    ///
    /// 边界条件：
    /// - 本次需求只支持向后循环跳转，因此 `Shift+F2`、`Ctrl+F2`、`Cmd+F2` 等组合键都不被识别。
    /// - GPUI 可能把功能键名称放在 `key` 或 `key_char` 中，解析时同时兼容两种来源。
    pub(in crate::app) fn marker_jump_keystroke(keystroke: &Keystroke) -> bool {
        if keystroke.modifiers.control
            || keystroke.modifiers.platform
            || keystroke.modifiers.alt
            || keystroke.modifiers.shift
        {
            return false;
        }

        let key = Self::normalized_keyboard_key(&keystroke.key);
        let key_char = keystroke
            .key_char
            .as_deref()
            .map(Self::normalized_keyboard_key);
        key == "f2" || key_char.as_deref().is_some_and(|value| value == "f2")
    }

    /// 切换指定行的标记状态。
    ///
    /// 业务意图：
    /// - 行号点击需要在添加和取消之间快速切换；返回值告诉调用方点击后该行是否仍处于标记状态。
    /// - 独立成纯状态函数后，UI 点击和单元测试可以复用同一套规则，避免行号交互和快捷键跳转使用不同语义。
    pub(in crate::app) fn toggle_marked_line(
        marked_lines: &mut BTreeSet<usize>,
        line_index: usize,
    ) -> bool {
        if marked_lines.insert(line_index) {
            true
        } else {
            marked_lines.remove(&line_index);
            false
        }
    }

    /// 执行当前活动 tab 的下一处标记跳转。
    ///
    /// 业务意图：
    /// - `F2` 应围绕当前活动日志 tab 工作，避免用户在多 tab 场景中跳到后台文件。
    /// - 跳转复用搜索结果定位的滚动与临时高亮能力，让标记目标在密集日志中有一致的视觉提示。
    ///
    /// 边界条件：
    /// - 加载中、读取失败或当前 tab 没有任何标记时不消费按键。
    /// - 如果用户手动滚动离开上次跳转目标，下一次 `F2` 会从新的可视顶部重新寻找最近后续标记。
    pub(in crate::app) fn jump_to_next_marked_line(&mut self, context: &mut Context<Self>) -> bool {
        let Some(active_tab_id) = self.log.active_tab_id else {
            return false;
        };
        let Some(tab_index) = self
            .log
            .open_tabs
            .iter()
            .position(|tab| tab.id == active_tab_id)
        else {
            return false;
        };

        let Some((target_line, tab_id)) = self.log.open_tabs.get(tab_index).and_then(|tab| {
            let (visible_start, visible_end) = Self::marker_visible_line_range(tab)?;
            let target_line = Self::next_marked_line(
                &tab.marked_lines,
                visible_start,
                visible_end,
                tab.last_marker_jump_line,
            )?;
            Some((target_line, tab.id))
        }) else {
            return false;
        };

        if let Some(tab) = self.log.open_tabs.get_mut(tab_index) {
            tab.last_marker_jump_line = Some(target_line);
            tab.highlighted_search_line = Some(target_line);
            tab.highlighted_search_match = None;
        }
        self.log.log_viewer_context_menu = None;
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.search.search_results_context_menu = None;
        self.scroll_log_tab_to_line(tab_id, target_line);
        context.notify();
        true
    }

    /// 计算指定 tab 当前日志正文的可视行号范围。
    ///
    /// 业务意图：
    /// - 标记跳转需要判断“上次跳转的标记是否还在当前视口内”，否则用户滚动后继续按 `F2` 会从旧位置跳转，违背当前阅读上下文。
    /// - 内存日志读取 GPUI 虚拟列表滚动句柄，分页日志读取应用侧 `PagedLogScrollState`，保持两种渲染路径的行为一致。
    pub(in crate::app) fn marker_visible_line_range(tab: &OpenLogTab) -> Option<(usize, usize)> {
        match &tab.state {
            LogTabState::Ready { document } => match document.as_ref() {
                LogTabDocument::InMemory(document) => {
                    let viewport_height =
                        Self::uniform_list_vertical_viewport_height(&tab.scroll_handle)
                            .unwrap_or(px(LOG_VIEWER_ROW_HEIGHT * 24.0));
                    let scroll_top = {
                        let state = tab.scroll_handle.0.borrow();
                        f64::from((-state.base_handle.offset().y).max(px(0.0)))
                    };
                    Self::marker_visible_range_from_scroll(
                        document.lines.len(),
                        scroll_top,
                        viewport_height,
                    )
                }
                LogTabDocument::Paged(document) => {
                    let viewport_height = tab.paged_viewport_handle.bounds().size.height;
                    let viewport_height = if viewport_height > px(0.0) {
                        viewport_height
                    } else {
                        px(LOG_VIEWER_ROW_HEIGHT * 24.0)
                    };
                    let (visible_start, _) = Self::paged_log_visible_start(
                        tab.paged_scroll.top_px,
                        document.line_count(),
                    );
                    Self::marker_visible_range_from_first_line(
                        document.line_count(),
                        visible_start,
                        viewport_height,
                    )
                }
            },
            LogTabState::Loading { .. } | LogTabState::Failed { .. } => None,
        }
    }

    /// 根据滚动像素计算标记跳转使用的可视行号范围。
    ///
    /// 边界条件：
    /// - 首帧尚未完成测量时调用方会传入 fallback 高度；这里仍处理空文档和异常负滚动值，避免快捷键路径 panic。
    pub(in crate::app) fn marker_visible_range_from_scroll(
        line_count: usize,
        scroll_top: f64,
        viewport_height: Pixels,
    ) -> Option<(usize, usize)> {
        if line_count == 0 {
            return None;
        }

        let row_height = f64::from(px(LOG_VIEWER_ROW_HEIGHT)).max(1.0);
        let first_line = (scroll_top.max(0.0) / row_height).floor() as usize;
        Self::marker_visible_range_from_first_line(line_count, first_line, viewport_height)
    }

    /// 根据首个可见行和视口高度计算标记跳转使用的可视行号范围。
    ///
    /// 业务意图：
    /// - 范围末尾多包含一行缓冲，覆盖半行滚动或分页渲染中的小数偏移，避免刚好露出一点的标记被误判为不可见。
    pub(in crate::app) fn marker_visible_range_from_first_line(
        line_count: usize,
        first_line: usize,
        viewport_height: Pixels,
    ) -> Option<(usize, usize)> {
        if line_count == 0 {
            return None;
        }

        let row_height = f64::from(px(LOG_VIEWER_ROW_HEIGHT)).max(1.0);
        let viewport_height = f64::from(viewport_height).max(row_height);
        let visible_rows = (viewport_height / row_height).ceil().max(1.0) as usize + 1;
        let visible_start = first_line.min(line_count.saturating_sub(1));
        let visible_end = visible_start
            .saturating_add(visible_rows.saturating_sub(1))
            .min(line_count.saturating_sub(1));
        Some((visible_start, visible_end))
    }

    /// 选择下一处应跳转的标记行。
    ///
    /// 业务意图：
    /// - 如果上次 `F2` 目标仍在视口内，就从它后一行继续找；否则从当前可视顶部重新找最近后续标记。
    /// - 使用 `BTreeSet::range` 直接查找有序集合中的后续标记，末尾没有结果时回到最小标记行。
    pub(in crate::app) fn next_marked_line(
        marked_lines: &BTreeSet<usize>,
        visible_start: usize,
        visible_end: usize,
        last_marker_jump_line: Option<usize>,
    ) -> Option<usize> {
        let start_line = match last_marker_jump_line {
            Some(line) if (visible_start..=visible_end).contains(&line) => line.saturating_add(1),
            _ => visible_start,
        };

        marked_lines
            .range(start_line..)
            .next()
            .copied()
            .or_else(|| marked_lines.iter().next().copied())
    }

    /// 规范化 GPUI 按键名称。
    ///
    /// 业务意图：
    /// - 键盘滚动只关心少数功能键，去掉空格、下划线和短横线可以兼容 `PageUp` / `page_up` / `page-up` 等常见写法。
    pub(in crate::app) fn normalized_keyboard_key(key: &str) -> String {
        key.chars()
            .filter(|character| {
                !character.is_ascii_whitespace() && *character != '_' && *character != '-'
            })
            .flat_map(|character| character.to_lowercase())
            .collect()
    }

    /// 执行当前目标区域的键盘滚动命令。
    ///
    /// 业务意图：
    /// - 全局按键处理只负责命令分发，具体滚动仍写入各区域已有滚动句柄，避免为键盘路径维护第二套状态。
    /// - 只有目标区域真实可滚动时才消费事件；例如搜索结果未打开或目录树内容不足时，按键继续交给默认路径。
    pub(in crate::app) fn handle_keyboard_scroll(
        &mut self,
        command: KeyboardScrollCommand,
        context: &mut Context<Self>,
    ) -> bool {
        let handled =
            match Self::keyboard_scroll_region_or_default(self.navigation.keyboard_scroll_region) {
                KeyboardScrollRegion::LogContent => self.scroll_log_content_by_keyboard(command),
                KeyboardScrollRegion::SearchResults => {
                    self.scroll_search_results_by_keyboard(command)
                }
                KeyboardScrollRegion::LogTree => self.scroll_log_tree_by_keyboard(command),
            };

        if handled {
            context.notify();
        }

        handled
    }

    /// 按键盘命令滚动当前日志正文。
    ///
    /// 业务意图：
    /// - 内存日志继续使用 `UniformListScrollHandle`，分页日志写入 `PagedLogScrollState.top_px`。
    /// - 两种模式共享同一套页距计算，保证用户切换大文件分页路径后快捷键手感一致。
    pub(in crate::app) fn scroll_log_content_by_keyboard(
        &mut self,
        command: KeyboardScrollCommand,
    ) -> bool {
        let Some(active_tab_id) = self.log.active_tab_id else {
            return false;
        };
        let Some(tab_index) = self
            .log
            .open_tabs
            .iter()
            .position(|tab| tab.id == active_tab_id)
        else {
            return false;
        };

        match &self.log.open_tabs[tab_index].state {
            LogTabState::Ready { document } => match document.as_ref() {
                LogTabDocument::InMemory(_) => {
                    let Some(metrics) = Self::log_vertical_scrollbar_metrics(
                        &self.log.open_tabs[tab_index].scroll_handle,
                    ) else {
                        return false;
                    };
                    let Some(viewport_height) = Self::uniform_list_vertical_viewport_height(
                        &self.log.open_tabs[tab_index].scroll_handle,
                    ) else {
                        return false;
                    };
                    Self::scroll_uniform_list_vertically_by_keyboard(
                        &self.log.open_tabs[tab_index].scroll_handle,
                        metrics.max_scroll,
                        viewport_height,
                        LOG_VIEWER_ROW_HEIGHT,
                        command,
                    )
                }
                LogTabDocument::Paged(document) => {
                    let viewport_height = self.log.open_tabs[tab_index]
                        .paged_viewport_handle
                        .bounds()
                        .size
                        .height;
                    if viewport_height <= px(0.0) {
                        return false;
                    }

                    let max_scroll = Self::paged_log_vertical_max_scroll_px(
                        document.line_count(),
                        viewport_height,
                    );
                    let Some(next_top) = Self::keyboard_scroll_position_px(
                        self.log.open_tabs[tab_index].paged_scroll.top_px,
                        max_scroll,
                        viewport_height,
                        LOG_VIEWER_ROW_HEIGHT,
                        command,
                    ) else {
                        return false;
                    };

                    self.log.open_tabs[tab_index].paged_scroll.top_px = next_top;
                    true
                }
            },
            LogTabState::Loading { .. } | LogTabState::Failed { .. } => false,
        }
    }

    /// 按键盘命令滚动搜索结果面板。
    ///
    /// 边界条件：
    /// - 面板关闭、结果不足一屏或首帧尚未完成滚动测量时不消费快捷键，避免用户按键后没有任何可见反馈。
    pub(in crate::app) fn scroll_search_results_by_keyboard(
        &mut self,
        command: KeyboardScrollCommand,
    ) -> bool {
        let Some(panel) = self.search.search_results_panel.as_ref() else {
            return false;
        };
        let Some(metrics) = Self::search_results_scrollbar_metrics(&panel.scroll_handle) else {
            return false;
        };
        let Some(viewport_height) =
            Self::uniform_list_vertical_viewport_height(&panel.scroll_handle)
        else {
            return false;
        };

        Self::scroll_uniform_list_vertically_by_keyboard(
            &panel.scroll_handle,
            metrics.max_scroll,
            viewport_height,
            SEARCH_RESULT_ROW_HEIGHT,
            command,
        )
    }

    /// 按键盘命令滚动左侧目录树。
    ///
    /// 边界条件：
    /// - 只有真实内容高度超过视口时才滚动；临时 fallback 滚动条的 `max_scroll` 为 0，不会误消费快捷键。
    pub(in crate::app) fn scroll_log_tree_by_keyboard(
        &mut self,
        command: KeyboardScrollCommand,
    ) -> bool {
        let Some(metrics) = Self::log_tree_scrollbar_metrics(&self.log.log_tree_scroll_handle)
        else {
            return false;
        };
        let Some(viewport_height) =
            Self::uniform_list_vertical_viewport_height(&self.log.log_tree_scroll_handle)
        else {
            return false;
        };

        Self::scroll_uniform_list_vertically_by_keyboard(
            &self.log.log_tree_scroll_handle,
            metrics.max_scroll,
            viewport_height,
            LOG_TREE_ROW_HEIGHT,
            command,
        )
    }

    /// 使用现有虚拟列表句柄执行一次纵向键盘滚动。
    ///
    /// 业务意图：
    /// - 日志正文、搜索结果和目录树都使用 `UniformListScrollHandle`，统一写入底层 `ScrollHandle` 可以保证滚轮、
    ///   自绘滚动条和键盘滚动共享同一个偏移来源。
    pub(in crate::app) fn scroll_uniform_list_vertically_by_keyboard(
        scroll_handle: &UniformListScrollHandle,
        max_scroll: Pixels,
        viewport_height: Pixels,
        row_height: f32,
        command: KeyboardScrollCommand,
    ) -> bool {
        let current_top = Self::uniform_list_vertical_scroll_top(scroll_handle, max_scroll);
        let Some(next_top) = Self::keyboard_scroll_position(
            current_top,
            max_scroll,
            viewport_height,
            row_height,
            command,
        ) else {
            return false;
        };

        Self::set_uniform_list_vertical_scroll_top(scroll_handle, next_top);
        true
    }

    /// 读取虚拟列表当前纵向滚动位置。
    ///
    /// 边界条件：
    /// - GPUI 底层偏移使用负数表示向下滚动，这里统一转换成业务侧非负 `scroll_top` 并夹在合法范围内。
    pub(in crate::app) fn uniform_list_vertical_scroll_top(
        scroll_handle: &UniformListScrollHandle,
        max_scroll: Pixels,
    ) -> Pixels {
        let state = scroll_handle.0.borrow();
        (-state.base_handle.offset().y).clamp(px(0.0), max_scroll)
    }

    /// 读取虚拟列表可见视口高度。
    ///
    /// 业务意图：
    /// - `PageUp` / `PageDown` 的滚动距离要按真实视口高度计算，不能写死固定行数，否则用户调整窗口或面板高度后手感会失真。
    pub(in crate::app) fn uniform_list_vertical_viewport_height(
        scroll_handle: &UniformListScrollHandle,
    ) -> Option<Pixels> {
        let state = scroll_handle.0.borrow();
        let bounds_height = state.base_handle.bounds().size.height;
        if bounds_height > px(0.0) {
            return Some(bounds_height);
        }

        state
            .last_item_size
            .map(|size| size.item.height)
            .filter(|height| *height > px(0.0))
    }

    /// 写入虚拟列表纵向滚动位置。
    ///
    /// 边界条件：
    /// - 设置 Y 偏移时保留当前 X 偏移，避免用户横向滚动长日志后按 PageDown 导致横向位置被重置。
    pub(in crate::app) fn set_uniform_list_vertical_scroll_top(
        scroll_handle: &UniformListScrollHandle,
        scroll_top: Pixels,
    ) {
        let base_scroll_handle = {
            // 先克隆底层句柄再释放 `RefCell` 借用，避免 `set_offset` 内部需要可变借用时产生嵌套借用。
            scroll_handle.0.borrow().base_handle.clone()
        };
        let current_offset = base_scroll_handle.offset();
        base_scroll_handle.set_offset(point(current_offset.x, -scroll_top));
    }

    /// 计算一次键盘滚动后的 `Pixels` 位置。
    ///
    /// 业务意图：
    /// - 页滚动距离按“视口高度减一行”计算，让用户翻页时保留一行上下文；视口很小时至少滚动一行。
    /// - 顶部、底部和翻页结果都统一 clamp，避免滚动条越界或出现负偏移。
    pub(in crate::app) fn keyboard_scroll_position(
        current_top: Pixels,
        max_scroll: Pixels,
        viewport_height: Pixels,
        row_height: f32,
        command: KeyboardScrollCommand,
    ) -> Option<Pixels> {
        if max_scroll <= px(0.0) {
            return None;
        }

        let current_top = current_top.clamp(px(0.0), max_scroll);
        let page_delta = Self::keyboard_scroll_page_delta(viewport_height, row_height);
        let next_top = match command {
            KeyboardScrollCommand::PageUp => current_top - page_delta,
            KeyboardScrollCommand::PageDown => current_top + page_delta,
            KeyboardScrollCommand::Top => px(0.0),
            KeyboardScrollCommand::Bottom => max_scroll,
        };

        Some(next_top.clamp(px(0.0), max_scroll))
    }

    /// 计算分页日志使用的 `f64` 逻辑滚动位置。
    ///
    /// 业务意图：
    /// - 分页日志可能非常大，滚动位置保存在 `f64` 中避免深位置 `f32` 精度不足；这里只把页距从 `Pixels` 转成 `f64`。
    pub(in crate::app) fn keyboard_scroll_position_px(
        current_top: f64,
        max_scroll: f64,
        viewport_height: Pixels,
        row_height: f32,
        command: KeyboardScrollCommand,
    ) -> Option<f64> {
        if max_scroll <= 0.0 {
            return None;
        }

        let current_top = current_top.clamp(0.0, max_scroll);
        let page_delta = f64::from(Self::keyboard_scroll_page_delta(
            viewport_height,
            row_height,
        ));
        let next_top = match command {
            KeyboardScrollCommand::PageUp => current_top - page_delta,
            KeyboardScrollCommand::PageDown => current_top + page_delta,
            KeyboardScrollCommand::Top => 0.0,
            KeyboardScrollCommand::Bottom => max_scroll,
        };

        Some(next_top.clamp(0.0, max_scroll))
    }

    /// 计算 PageUp/PageDown 的页距。
    ///
    /// 边界条件：
    /// - 如果视口高度尚小于一行或测量异常，仍至少移动一行，避免快捷键看起来失效。
    pub(in crate::app) fn keyboard_scroll_page_delta(
        viewport_height: Pixels,
        row_height: f32,
    ) -> Pixels {
        let row_height = px(row_height.max(1.0));
        if viewport_height > row_height {
            viewport_height - row_height
        } else {
            row_height
        }
    }

    /// 判断按键是否匹配指定字母或该字母的 ASCII 控制字符。
    ///
    /// 业务意图：
    /// - GPUI 的 `Keystroke::key` 和 `key_char` 在不同平台输入路径下可能保存不同形态：
    ///   普通 `Cmd+F` 通常表现为 `key = "f"`，而 `Ctrl+F` 可能表现为 `"\u{6}"`。
    /// - 把匹配逻辑集中在这里，可以让搜索和复制快捷键都兼容真实键盘、AppleScript 自动化和不同键盘布局。
    ///
    /// 边界条件：
    /// - 该函数只用于带控制键语义的快捷键，不参与普通文本输入，避免把不可见控制字符误写入搜索框。
    pub(in crate::app) fn keystroke_matches_letter_or_control_code(
        keystroke: &Keystroke,
        letter: &str,
        control_code: &str,
    ) -> bool {
        keystroke.key.eq_ignore_ascii_case(letter)
            || Self::keystroke_matches_control_code(keystroke, control_code)
            || keystroke
                .key_char
                .as_deref()
                .is_some_and(|key_char| key_char.eq_ignore_ascii_case(letter))
    }

    /// 判断按键是否是指定 ASCII 控制字符。
    pub(in crate::app) fn keystroke_matches_control_code(
        keystroke: &Keystroke,
        control_code: &str,
    ) -> bool {
        keystroke.key == control_code || keystroke.key_char.as_deref() == Some(control_code)
    }
}
