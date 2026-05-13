// 搜索窗口任务编排。
//
// 业务意图：
// - 本文件由原主窗口工作区实现机械拆分而来，只承载 GPUI 壳层内的 搜索窗口打开关闭、搜索任务启动、结果合并和结果跳转。
// - 拆分过程保持所有状态字段、事件时序和用户可见行为不变，便于后续按视图职责继续收束。

use super::*;

impl MainView {
    /// 返回当前激活文件所在目录的展示标签。
    ///
    /// 业务意图：
    /// - 用户切换到“当前目录”搜索时，需要看到实际将被搜索的目录目标。
    /// - 这里仅从当前活动 tab 的 `LogFileSource` 派生展示文本，不访问磁盘，也不扩大已加载目录树的权限边界。
    pub(in crate::app) fn active_search_directory_label(&self) -> Option<String> {
        let active_tab_id = self.log.active_tab_id?;
        let active_tab = self
            .log
            .open_tabs
            .iter()
            .find(|tab| tab.id == active_tab_id)?;
        Some(source_location_label(&active_tab.source))
    }

    /// 判断某个文件来源是否匹配用户编辑后的目录目标文本。
    ///
    /// 业务意图：
    /// - 当前目录搜索的基础范围仍由 `collect_current_directory_sources` 从加载树中递归收集。
    /// - 用户修改目录目标时，只在这批已授权来源内按展示路径做二次过滤，满足缩小范围或输入子目录片段的需求。
    ///
    /// 边界条件：
    /// - 目标为空时不额外过滤，保持“当前文件所在目录递归搜索”的默认行为。
    /// - 路径大小写在 Windows 上通常不敏感；这里统一使用小写包含匹配，优先保证跨平台用户输入的容错性。
    pub(in crate::app) fn source_matches_directory_target(
        source: &LogFileSource,
        target: &str,
    ) -> bool {
        let target = target.trim();
        if target.is_empty() {
            return true;
        }

        source_location_label(source)
            .to_lowercase()
            .contains(&target.to_lowercase())
    }

    /// 记录一次搜索关键字。
    ///
    /// 业务意图：
    /// - 执行搜索后把关键字写入当前会话历史，让下一次没有正文选区时可以自动恢复最近关键字。
    /// - 历史不持久化到配置目录，避免日志关键字涉及业务数据或敏感信息时被长期保存。
    pub(in crate::app) fn remember_search_query(
        &mut self,
        query: &str,
        match_mode: SearchMatchMode,
    ) {
        Self::remember_search_query_in_history(
            &mut self.search.search_query_history,
            query,
            match_mode,
        );
    }

    /// 更新搜索关键字历史集合。
    ///
    /// 边界条件：
    /// - 空白关键字不记录。
    /// - 重复的“关键字 + 匹配模式”先移除旧位置再插入首位，保证列表按最近使用排序。
    /// - 超过上限时删除最旧记录，避免会话状态无界增长。
    pub(in crate::app) fn remember_search_query_in_history(
        history: &mut Vec<SearchQueryHistoryItem>,
        query: &str,
        match_mode: SearchMatchMode,
    ) {
        let Some(item) = SearchQueryHistoryItem::new(query, match_mode) else {
            return;
        };

        history.retain(|existing| {
            existing.query != item.query || existing.match_mode != item.match_mode
        });
        history.insert(0, item);
        history.truncate(SEARCH_QUERY_HISTORY_LIMIT);
    }

    /// 返回最近一次搜索关键字。
    ///
    /// 业务意图：
    /// - 打开搜索窗口时如果没有日志选区，就使用最近关键字预填，满足用户“显示上一次搜索关键字”的要求。
    pub(in crate::app) fn last_search_query(&self) -> Option<SearchQueryHistoryItem> {
        self.search.search_query_history.first().cloned()
    }

    /// 全选搜索关键字输入框内容。
    ///
    /// 业务意图：
    /// - 用户通过工具栏、快捷键或点击重新激活搜索框时，通常是在替换关键字而不是追加文本。
    /// - 独立搜索窗口使用自绘输入框，因此需要在业务状态中显式维护 UTF-8 选区。
    ///
    /// 边界条件：
    /// - 空关键字会得到 `0..0` 选区；组合输入范围必须清空，避免激活后继续提交旧 IME 组合文本。
    pub(in crate::app) fn select_all_search_query(dialog: &mut SearchDialogState) {
        dialog.query_input.marked_range = None;
        dialog.query_input.selection_range = 0..dialog.query_input.text.len();
        dialog.query_input.horizontal_scroll_px = 0.0;
    }

    /// 把用户选择的历史关键字填入搜索对话框。
    ///
    /// 业务意图：
    /// - 历史下拉菜单只负责选择已有关键字，不立即启动搜索，用户仍可继续编辑范围、大小写和目录目标。
    /// - 选择后把光标放到关键字末尾，并清除组合文本和当前文件计数缓存，保证后续搜索使用完整新关键字。
    ///
    /// 边界条件：
    /// - 空白历史项不会由历史管理产生；这里仍做防御性忽略，避免未来调用方传入非法值时清空当前输入。
    pub(in crate::app) fn apply_search_history_query(
        dialog: &mut SearchDialogState,
        item: &SearchQueryHistoryItem,
    ) {
        let query = item.query.trim();
        if query.is_empty() {
            dialog.query_history_menu_open = false;
            return;
        }

        dialog.query_input.text = query.to_string();
        dialog.match_mode = item.match_mode;
        let cursor = dialog.query_input.text.len();
        dialog.query_input.selection_range = cursor..cursor;
        dialog.query_input.marked_range = None;
        dialog.query_history_menu_open = false;
        dialog.current_file_match_count = None;
        dialog.current_file_navigation_match = None;
        dialog.message = "已选择历史关键字，按 Enter 或点击搜索".to_string();
    }

    /// 停止搜索对话框当前正在运行的搜索任务，并返回需要标记取消的任务 ID。
    ///
    /// 业务意图：
    /// - 搜索窗口中的“停止”按钮只中断当前后台任务，不关闭窗口，也不清空用户已经输入的关键字和范围。
    /// - 后台任务可能已经在读取或搜索大文件；这里通过任务 ID 失效和 `is_searching=false` 丢弃后续回调，
    ///   让 UI 立即恢复可编辑状态，同时保留取消前已经收集到的结果。
    ///
    /// 边界条件：
    /// - 如果当前没有运行中的任务，只关闭历史下拉菜单并返回 `None`，避免重复点击停止按钮错误标记旧记录。
    pub(in crate::app) fn stop_search_dialog_task(dialog: &mut SearchDialogState) -> Option<usize> {
        dialog.query_history_menu_open = false;
        if !dialog.is_searching {
            return None;
        }

        dialog.is_searching = false;
        dialog.message = "搜索已停止，可修改条件后重新搜索".to_string();
        Some(dialog.job_id)
    }

    /// 从已加载目录树中收集匹配用户目录目标文本的来源。
    ///
    /// 业务意图：
    /// - “当前目录”搜索默认落在当前文件所在目录；当用户手动修改目标目录时，应允许定位到加载树中的其它目录或子目录片段。
    /// - 搜索仍只遍历已加载树中存在的 `LogFileSource`，不会因为用户输入路径而额外扫描磁盘或解压压缩包。
    ///
    /// 边界条件：
    /// - 同一来源可能因压缩包单文件快捷入口等原因重复出现在树中，必须按稳定键去重。
    pub(in crate::app) fn collect_sources_matching_directory_target(
        tree: &LoadedLogTree,
        target: &str,
    ) -> Vec<LogFileSource> {
        let mut seen_keys = HashSet::new();
        let mut sources = Vec::new();

        for source in tree.rows.iter().filter_map(|row| row.source.as_ref()) {
            if !Self::source_matches_directory_target(source, target) {
                continue;
            }

            let key = source.stable_key();
            if seen_keys.insert(key) {
                sources.push(source.clone());
            }
        }

        sources
    }

    /// 标记指定搜索历史记录已取消。
    ///
    /// 业务意图：
    /// - 关闭搜索对话框会让后台任务回调失效，结果面板必须同步从“搜索中”切换为“已取消”。
    /// - 只修改对应任务的记录，避免影响面板中其它历史搜索。
    pub(in crate::app) fn mark_search_record_canceled(&mut self, job_id: usize) {
        let Some(panel) = self.search.search_results_panel.as_mut() else {
            return;
        };
        Self::mark_search_record_canceled_in_records(&mut panel.records, job_id);
    }

    /// 在记录集合中标记指定搜索任务已取消。
    ///
    /// 业务意图：
    /// - 主窗口和单元测试都需要验证“旧搜索被新操作打断”时记录状态会从“搜索中”切换为“已取消”。
    /// - 该函数只修改数据，不触发重绘；调用方负责在 UI 上下文中 `notify`。
    pub(in crate::app) fn mark_search_record_canceled_in_records(
        records: &mut [SearchHistoryRecord],
        job_id: usize,
    ) -> bool {
        if let Some(record) = records.iter_mut().find(|record| record.job_id == job_id) {
            record.canceled = true;
            return true;
        }
        false
    }

    /// 在 `MainView` 更新租借结束后打开设置窗口。
    ///
    /// 业务意图：
    /// - 工具栏设置按钮通过该入口打开独立窗口，保证重复点击只激活已有窗口而不是创建多个窗口。
    /// - 设置窗口只读写 `MainView` 中的设置状态，不访问文件系统、不请求网络，也不影响日志加载或搜索任务。
    ///
    /// 边界条件：
    /// - 如果旧窗口句柄失效，清空后重新创建。
    /// - 创建失败时仅清理 pending 状态；当前没有用户可见错误面板，避免把设置窗口失败混入日志内容区。
    pub(in crate::app) fn open_settings_window_after_main_update(
        main_view: Entity<MainView>,
        app: &mut App,
    ) {
        let existing_settings_window = main_view.update(app, |view, context| {
            view.settings.settings_window_open_pending = false;
            view.log.tab_context_menu = None;
            view.log.encoding_dropdown_menu = None;
            view.search.search_results_context_menu = None;
            view.log.log_viewer_context_menu = None;
            context.notify();
            view.settings.settings_window
        });

        if let Some(settings_window) = existing_settings_window {
            if settings_window
                .update(app, |_, window, _| {
                    window.activate_window();
                })
                .is_ok()
            {
                return;
            }
            main_view.update(app, |view, context| {
                view.discard_thread_analysis_filter_edit(context);
                view.discard_quick_search_keywords_edit(context);
                view.settings.settings_window = None;
            });
        }

        let main_view_for_window = main_view.clone();
        let main_view_for_close = main_view.clone();
        let settings_window_options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("设置".into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::centered(
                size(px(SETTINGS_WINDOW_WIDTH), px(SETTINGS_WINDOW_HEIGHT)),
                app,
            )),
            is_resizable: false,
            is_minimizable: true,
            window_min_size: Some(size(px(SETTINGS_WINDOW_WIDTH), px(SETTINGS_WINDOW_HEIGHT))),
            ..Default::default()
        };

        match app.open_window(settings_window_options, move |window, app| {
            window.on_window_should_close(app, move |_, app| {
                main_view_for_close.update(app, |view, context| {
                    view.discard_thread_analysis_filter_edit(context);
                    view.discard_quick_search_keywords_edit(context);
                    view.settings.settings_window = None;
                    view.settings.settings_window_open_pending = false;
                    context.notify();
                });
                true
            });
            app.new(|context| SettingsWindowView::new(main_view_for_window, context))
        }) {
            Ok(settings_window) => {
                main_view.update(app, |view, context| {
                    view.settings.settings_window = Some(settings_window);
                    context.notify();
                });
            }
            Err(_error) => {
                main_view.update(app, |view, context| {
                    view.settings.settings_window = None;
                    view.settings.settings_window_open_pending = false;
                    context.notify();
                });
            }
        }
    }

    /// 在 `MainView` 更新租借结束后切换到 HPROF 页并启动解析。
    ///
    /// 业务意图：
    /// - HPROF 解析已经迁入主窗口大功能页，选择文件后应切到该页并复用内嵌分析实体。
    /// - 分析实体观察主视图主题，因此创建实体必须发生在主视图更新闭包内，并在实体内部启动后台解析。
    ///
    /// 边界条件：
    /// - 重复选择新文件时复用旧实体，旧后台任务由 `HprofAnalysisView::start_new_analysis` 通过取消标记和代次保护停止或丢弃结果。
    pub(in crate::app) fn open_hprof_analysis_page_after_main_update(
        main_view: Entity<MainView>,
        file_path: PathBuf,
        app: &mut App,
    ) {
        main_view.update(app, |view, context| {
            view.navigation.active_main_feature = MainFeature::HprofAnalysis;
            view.log.tab_context_menu = None;
            view.log.encoding_dropdown_menu = None;
            view.search.search_results_context_menu = None;
            view.log.log_viewer_context_menu = None;
            view.log.log_tree_context_menu = None;
            view.log.load_source_menu = None;

            let hprof_view = if let Some(hprof_view) = view.hprof_analysis_view.clone() {
                hprof_view
            } else {
                let main_view_for_hprof = context.entity();
                let hprof_view =
                    context.new(|context| HprofAnalysisView::new(main_view_for_hprof, context));
                view.hprof_analysis_view = Some(hprof_view.clone());
                hprof_view
            };
            hprof_view.update(context, |hprof_view, context| {
                hprof_view.start_new_analysis(file_path, context);
            });
            context.notify();
        });
    }

    /// 返回 HPROF 页的内嵌分析视图，如果尚未创建则创建空态视图。
    ///
    /// 业务意图：
    /// - 用户第一次点击 HPROF 导航时应看到主窗口内的空态页，而不是打开独立窗口或立即读取磁盘。
    pub(in crate::app) fn hprof_analysis_view(
        &mut self,
        context: &mut Context<Self>,
    ) -> Entity<HprofAnalysisView> {
        if let Some(hprof_view) = self.hprof_analysis_view.clone() {
            return hprof_view;
        }
        let main_view = context.entity();
        let hprof_view = context.new(|context| HprofAnalysisView::new(main_view, context));
        self.hprof_analysis_view = Some(hprof_view.clone());
        hprof_view
    }

    /// 准备搜索对话框状态。
    ///
    /// 业务意图：
    /// - 如果日志正文当前存在选区，打开或再次唤起搜索框时用选中文本预填关键字，减少复制再搜索的重复操作。
    /// - 如果没有日志选区，首次打开使用最近一次搜索关键字；已有对话框只在查询词为空时恢复最近关键字，避免覆盖用户正在编辑的输入。
    ///
    /// 实现原因：
    /// - 这里只修改 `MainView` 自身状态，不创建窗口；独立搜索窗口会在 `MainView::update` 返回后再创建。
    /// - 这样可以避免搜索窗口根视图初始化或渲染时读取 `MainView`，和当前 `MainView` 更新租借发生重叠。
    pub(in crate::app) fn prepare_search_dialog_state(&mut self) {
        let selected_query = self.selected_log_text_for_search_query();
        let mut query_replaced = false;
        if self.search.search_dialog.is_none() {
            let directory_target = self.active_search_directory_label().unwrap_or_default();
            let last_query = self.last_search_query();
            let (query, match_mode) = if let Some(selected_query) = selected_query.clone() {
                // 正文选区是普通文本片段，预填时默认使用普通文本模式，避免选中内容中的正则元字符改变含义。
                (selected_query, SearchMatchMode::Literal)
            } else if let Some(last_query) = last_query.clone() {
                (last_query.query, last_query.match_mode)
            } else {
                (String::new(), SearchMatchMode::Literal)
            };
            let mut query_input = SingleLineTextInputState::from_text(query);
            query_input.selection_range = 0..query_input.text.len();
            self.search.search_dialog = Some(SearchDialogState {
                query_input,
                query_history_menu_open: false,
                scope: SearchScope::CurrentFile,
                directory_input: SingleLineTextInputState::from_text(directory_target),
                case_sensitive: false,
                match_mode,
                current_file_match_count: None,
                current_file_navigation_match: None,
                is_searching: false,
                progress: SearchProgress::default(),
                message: if selected_query.is_some() {
                    "已填入选中文本，按 Enter 或点击搜索".to_string()
                } else if last_query.is_some() {
                    "已填入上次搜索关键字，按 Enter 或点击搜索".to_string()
                } else {
                    "输入关键字后按 Enter 或点击搜索".to_string()
                },
                job_id: 0,
            });
        } else if let Some(selected_query) = selected_query
            && let Some(dialog) = self.search.search_dialog.as_mut()
        {
            let cursor = selected_query.len();
            dialog.query_input.text = selected_query;
            dialog.query_input.selection_range = cursor..cursor;
            dialog.query_input.marked_range = None;
            dialog.query_history_menu_open = false;
            dialog.current_file_match_count = None;
            dialog.current_file_navigation_match = None;
            dialog.match_mode = SearchMatchMode::Literal;
            Self::select_all_search_query(dialog);
            dialog.message = "已填入选中文本，按 Enter 或点击搜索".to_string();
            query_replaced = true;
        } else if let Some(last_query) = self.last_search_query()
            && let Some(dialog) = self.search.search_dialog.as_mut()
            && dialog.query_input.text.trim().is_empty()
        {
            dialog.query_input.text = last_query.query;
            dialog.match_mode = last_query.match_mode;
            dialog.query_input.marked_range = None;
            dialog.query_history_menu_open = false;
            dialog.current_file_match_count = None;
            dialog.current_file_navigation_match = None;
            Self::select_all_search_query(dialog);
            dialog.message = "已填入上次搜索关键字，按 Enter 或点击搜索".to_string();
            query_replaced = true;
        }
        if query_replaced {
            self.search.current_file_navigation_request_id += 1;
            self.clear_active_log_tab_search_match_highlight();
        }
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
    }

    /// 在 `MainView` 更新租借结束后打开搜索对话框。
    ///
    /// 业务意图：
    /// - 工具栏按钮、`Ctrl+F` 和 `Cmd+F` 都通过该入口打开搜索窗口，避免不同入口出现不同状态规则。
    /// - 搜索窗口是独立 GPUI 窗口，它的根视图会读取并观察 `MainView`；因此窗口创建必须发生在 `MainView::update` 闭包外。
    ///
    /// 边界条件：
    /// - 如果已有搜索窗口仍有效，则只激活并聚焦输入框。
    /// - 如果旧句柄已经失效，则清空后重新创建；创建失败时把错误写回搜索对话框状态。
    pub(in crate::app) fn open_search_dialog_after_main_update(
        main_view: Entity<MainView>,
        _current_window: &mut Window,
        app: &mut App,
    ) {
        let (search_input_focus, existing_search_window) =
            main_view.update(app, |view, context| {
                view.search.search_dialog_open_pending = false;
                view.prepare_search_dialog_state();
                if let Some(dialog) = view.search.search_dialog.as_mut() {
                    Self::select_all_search_query(dialog);
                }
                context.notify();
                (
                    view.search.search_input_focus.clone(),
                    view.search.search_dialog_window,
                )
            });

        if let Some(search_window) = existing_search_window {
            if search_window
                .update(app, |_, window, _| {
                    window.activate_window();
                    window.focus(&search_input_focus);
                })
                .is_ok()
            {
                return;
            }
            main_view.update(app, |view, _| {
                view.search.search_dialog_window = None;
            });
        }

        let main_view_for_window = main_view.clone();
        let main_view_for_close = main_view.clone();
        let search_window_options = WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::centered(
                size(px(SEARCH_DIALOG_WIDTH), px(SEARCH_DIALOG_WINDOW_HEIGHT)),
                app,
            )),
            kind: WindowKind::Floating,
            is_resizable: false,
            is_minimizable: false,
            window_min_size: Some(size(
                px(SEARCH_DIALOG_WIDTH),
                px(SEARCH_DIALOG_WINDOW_HEIGHT),
            )),
            ..Default::default()
        };

        match app.open_window(search_window_options, move |window, app| {
            window.focus(&search_input_focus);
            window.on_window_should_close(app, move |_, app| {
                main_view_for_close.update(app, |view, context| {
                    view.search.search_dialog_window = None;
                    view.clear_search_dialog_state(true, context);
                });
                true
            });
            app.new(|context| SearchDialogWindowView::new(main_view_for_window, context))
        }) {
            Ok(search_window) => {
                main_view.update(app, |view, context| {
                    view.search.search_dialog_window = Some(search_window);
                    context.notify();
                });
            }
            Err(error) => {
                main_view.update(app, |view, context| {
                    view.search.search_dialog_window = None;
                    if let Some(dialog) = view.search.search_dialog.as_mut() {
                        dialog.message = format!("打开搜索窗口失败：{error}");
                        dialog.is_searching = false;
                    }
                    context.notify();
                });
            }
        }
    }

    /// 关闭搜索对话框并让当前后台搜索任务失效。
    ///
    /// 业务意图：
    /// - 用户关闭对话框时表示不再关注当前搜索过程；旧任务即使稍后返回，也不应继续更新进度或结果。
    /// - 结果面板不在这里清空，方便用户保留已完成的结果上下文；如果任务仍在运行，则标记为已取消，避免面板永远停留在进行中。
    pub(in crate::app) fn close_search_dialog(
        &mut self,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let search_window = self.search.search_dialog_window.take();
        let current_window_is_search = search_window
            .map(AnyWindowHandle::from)
            .is_some_and(|search_window| search_window == window.window_handle());
        self.clear_search_dialog_state(true, context);
        if current_window_is_search {
            window.remove_window();
        } else if let Some(search_window) = search_window {
            let _ = search_window.update(context, |_, window, _| {
                window.remove_window();
            });
            window.focus(&self.root_focus_handle);
        } else {
            window.focus(&self.root_focus_handle);
        }
        context.notify();
    }

    /// 清理搜索对话框状态。
    ///
    /// 业务意图：
    /// - 主动关闭搜索窗口和系统窗口关闭都需要同一套状态清理规则；搜索完成后不再调用该函数，窗口应继续保留。
    /// - 关闭时需要标记正在运行的记录为已取消，避免底部结果面板长期显示“搜索中”。
    pub(in crate::app) fn clear_search_dialog_state(
        &mut self,
        cancel_running: bool,
        context: &mut Context<Self>,
    ) {
        let running_job_id = self
            .search
            .search_dialog
            .as_ref()
            .filter(|dialog| cancel_running && dialog.is_searching)
            .map(|dialog| dialog.job_id);
        if self.search.search_dialog.is_none() {
            return;
        }
        self.search.search_dialog = None;
        self.search.next_search_job_id += 1;
        self.search.current_file_navigation_request_id += 1;
        if let Some(job_id) = running_job_id {
            self.mark_search_record_canceled(job_id);
        }
        context.notify();
    }

    /// 停止当前搜索任务但保留搜索窗口。
    ///
    /// 业务意图：
    /// - 用户点击“停止”表示只中断本轮搜索，不应丢失已输入关键字、范围、大小写和目录目标。
    /// - 通过推进 `next_search_job_id` 并清理 `is_searching`，后续后台回调会被 `is_current_search_job` 丢弃，
    ///   避免已经取消的任务继续更新进度或在完成时关闭搜索窗口。
    pub(in crate::app) fn stop_current_search(&mut self, context: &mut Context<Self>) {
        let canceled_job_id = self
            .search
            .search_dialog
            .as_mut()
            .and_then(Self::stop_search_dialog_task);
        let Some(job_id) = canceled_job_id else {
            context.notify();
            return;
        };

        self.search.next_search_job_id += 1;
        self.mark_search_record_canceled(job_id);
        context.notify();
    }

    /// 启动一次搜索。
    ///
    /// 业务意图：
    /// - 从搜索对话框读取当前查询词、范围和大小写规则，统一分发到当前文件或当前目录搜索。
    /// - 新搜索会保留历史记录，但如果上一轮搜索仍在运行，必须先标记为已取消，避免旧任务回调被丢弃后面板长期显示“搜索中”。
    pub(in crate::app) fn start_search(&mut self, context: &mut Context<Self>) {
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            dialog.query_history_menu_open = false;
        }
        let Some((
            query,
            scope,
            case_sensitive,
            match_mode,
            directory_target,
            previous_running_job_id,
        )) = ({
            self.search.search_dialog.as_ref().map(|dialog| {
                (
                    dialog.query_input.text.trim().to_string(),
                    dialog.scope,
                    dialog.case_sensitive,
                    dialog.match_mode,
                    dialog.directory_input.text.trim().to_string(),
                    dialog.is_searching.then_some(dialog.job_id),
                )
            })
        })
        else {
            return;
        };
        let options = SearchOptions::single_with_mode(query.clone(), case_sensitive, match_mode);
        if options.is_empty_query() {
            self.update_search_start_failure_message(
                previous_running_job_id,
                "请输入要搜索的关键字",
                context,
            );
            return;
        }
        if let Err(error) = options.validate() {
            self.update_search_start_failure_message(
                previous_running_job_id,
                error.to_string(),
                context,
            );
            return;
        }
        self.remember_search_query(&query, match_mode);

        self.start_search_job(
            SearchJobRequest {
                record_query: query,
                options,
                scope,
                case_sensitive,
                match_mode,
                directory_target,
                previous_running_job_id,
            },
            context,
        );
    }

    /// 启动一次快搜。
    ///
    /// 业务意图：
    /// - 快搜使用“设置-日志”中已保存的英文逗号分隔关键字，按 OR 语义搜索任一关键字命中的行。
    /// - 快搜复用搜索窗口当前范围、目录目标和大小写选项，但不修改搜索框文本，也不写入普通搜索历史。
    pub(in crate::app) fn start_quick_search(&mut self, context: &mut Context<Self>) {
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            dialog.query_history_menu_open = false;
        }
        let previous_running_job_id = self
            .search
            .search_dialog
            .as_ref()
            .and_then(|dialog| dialog.is_searching.then_some(dialog.job_id));
        let keywords = self.effective_quick_search_keywords();
        if keywords.is_empty() {
            self.update_search_start_failure_message(
                previous_running_job_id,
                "请先在设置-日志中配置快搜关键字",
                context,
            );
            return;
        }
        let Some((scope, case_sensitive, directory_target, previous_running_job_id)) = ({
            self.search.search_dialog.as_ref().map(|dialog| {
                (
                    dialog.scope,
                    dialog.case_sensitive,
                    dialog.directory_input.text.trim().to_string(),
                    dialog.is_searching.then_some(dialog.job_id),
                )
            })
        }) else {
            return;
        };
        let record_query = format!("快搜：{}", keywords.join(", "));
        let options = SearchOptions::any(keywords, case_sensitive);

        self.start_search_job(
            SearchJobRequest {
                record_query,
                options,
                scope,
                case_sensitive,
                match_mode: SearchMatchMode::Literal,
                directory_target,
                previous_running_job_id,
            },
            context,
        );
    }

    /// 按给定选项创建搜索任务。
    ///
    /// 业务意图：
    /// - 普通搜索和快搜只在关键字来源和结果记录标题上不同，真正的范围校验、后台任务、进度和结果面板应复用同一条路径。
    /// - 该函数是搜索任务编排边界，参数来自 UI 表单快照；使用参数对象让普通搜索和快搜共享同一入口。
    pub(in crate::app) fn start_search_job(
        &mut self,
        request: SearchJobRequest,
        context: &mut Context<Self>,
    ) {
        let SearchJobRequest {
            record_query,
            options,
            scope,
            case_sensitive,
            match_mode,
            directory_target,
            previous_running_job_id,
        } = request;

        if options.is_empty_query() {
            self.update_search_start_failure_message(
                previous_running_job_id,
                "请输入要搜索的关键字",
                context,
            );
            return;
        }
        if let Err(error) = options.validate() {
            self.update_search_start_failure_message(
                previous_running_job_id,
                error.to_string(),
                context,
            );
            return;
        }

        let Some(active_tab_id) = self.log.active_tab_id else {
            self.update_search_start_failure_message(
                previous_running_job_id,
                "请先从左侧打开一个日志文件",
                context,
            );
            return;
        };
        let Some(active_tab) = self
            .log
            .open_tabs
            .iter()
            .find(|tab| tab.id == active_tab_id)
        else {
            self.update_search_start_failure_message(
                previous_running_job_id,
                "当前日志 tab 不存在，请重新选择文件",
                context,
            );
            return;
        };

        let search_target = match scope {
            SearchScope::CurrentFile => match &active_tab.state {
                LogTabState::Ready { document } => SearchTarget::CurrentFile {
                    source: active_tab.source.clone(),
                    document: document.clone(),
                },
                LogTabState::Loading { .. } => {
                    self.update_search_start_failure_message(
                        previous_running_job_id,
                        "当前文件仍在加载，完成后再搜索",
                        context,
                    );
                    return;
                }
                LogTabState::Failed { .. } => {
                    self.update_search_start_failure_message(
                        previous_running_job_id,
                        "当前文件打开失败，无法搜索正文",
                        context,
                    );
                    return;
                }
            },
            SearchScope::CurrentDirectory => {
                let LogTreeLoadState::Loaded(tree_state) = &self.log.load_state else {
                    self.update_search_start_failure_message(
                        previous_running_job_id,
                        "请先加载日志目录后再搜索当前目录",
                        context,
                    );
                    return;
                };
                let sources = if directory_target.is_empty() {
                    collect_current_directory_sources(&tree_state.tree, &active_tab.source)
                } else {
                    Self::collect_sources_matching_directory_target(
                        &tree_state.tree,
                        &directory_target,
                    )
                };
                if sources.is_empty() {
                    self.update_search_start_failure_message(
                        previous_running_job_id,
                        "当前目录中没有可搜索的日志文件",
                        context,
                    );
                    return;
                }
                SearchTarget::CurrentDirectory { sources }
            }
        };

        let job_id = self.search.next_search_job_id;
        self.search.next_search_job_id += 1;

        let total_files = match &search_target {
            SearchTarget::CurrentFile { .. } => 1,
            SearchTarget::CurrentDirectory { sources } => sources.len(),
        };
        let panel_height = self
            .search
            .search_results_panel
            .as_ref()
            .map(|panel| panel.height)
            .unwrap_or(SEARCH_RESULTS_PANEL_DEFAULT_HEIGHT);
        let new_record = SearchHistoryRecord {
            job_id,
            query: record_query,
            scope,
            directory_target: (scope == SearchScope::CurrentDirectory)
                .then(|| directory_target.clone())
                .filter(|target| !target.is_empty()),
            case_sensitive,
            match_mode,
            progress: SearchProgress {
                searched_files: 0,
                total_files,
                matched_lines: 0,
            },
            results: Vec::new(),
            errors: Vec::new(),
            canceled: false,
            expanded: true,
            expanded_file_keys: HashSet::new(),
        };
        if let Some(previous_job_id) = previous_running_job_id {
            self.mark_search_record_canceled(previous_job_id);
        }
        if let Some(panel) = self.search.search_results_panel.as_mut() {
            for record in &mut panel.records {
                record.expanded = false;
            }
            panel.records.push(new_record);
            panel.rows = Self::search_results_panel_rows_from_records(&panel.records);
            panel.scroll_handle = UniformListScrollHandle::new();
        } else {
            let records = vec![new_record];
            let rows = Self::search_results_panel_rows_from_records(&records);
            self.search.search_results_panel = Some(SearchResultsPanelState {
                records,
                rows,
                height: panel_height,
                scroll_handle: UniformListScrollHandle::new(),
            });
        }
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            dialog.job_id = job_id;
            dialog.is_searching = true;
            dialog.progress = SearchProgress {
                searched_files: 0,
                total_files,
                matched_lines: 0,
            };
            dialog.message = format!("正在搜索 0/{total_files} 个文件...");
        }
        context.notify();

        match search_target {
            SearchTarget::CurrentFile { source, document } => {
                self.spawn_current_file_search(job_id, source, *document, options, context);
            }
            SearchTarget::CurrentDirectory { sources } => {
                self.spawn_current_directory_search(job_id, sources, options, context);
            }
        }
    }

    /// 处理新搜索启动失败时的旧任务收尾。
    ///
    /// 业务意图：
    /// - 用户可能在上一轮搜索仍运行时按 Enter 发起新搜索；如果新条件校验失败，旧后台回调会因为对话框停止搜索而失效。
    /// - 这种情况下必须同步把旧结果记录标记为已取消，否则底部面板会长期显示“搜索中”。
    pub(in crate::app) fn update_search_start_failure_message(
        &mut self,
        previous_running_job_id: Option<usize>,
        message: impl Into<String>,
        context: &mut Context<Self>,
    ) {
        if let Some(job_id) = previous_running_job_id {
            self.search.next_search_job_id += 1;
            self.mark_search_record_canceled(job_id);
        }
        self.update_search_dialog_message(message, context);
    }

    /// 更新搜索对话框提示文案。
    pub(in crate::app) fn update_search_dialog_message(
        &mut self,
        message: impl Into<String>,
        context: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            dialog.message = message.into();
            dialog.is_searching = false;
        }
        context.notify();
    }

    /// 启动当前文件后台搜索。
    ///
    /// 业务意图：
    /// - 当前文件虽然已经解码，但逐行搜索大日志仍可能耗时，因此放到后台执行器运行。
    /// - 这里传入 `Arc<Vec<String>>`，只共享已解码行集合，不在 UI 线程复制整份日志文本。
    pub(in crate::app) fn spawn_current_file_search(
        &self,
        job_id: usize,
        source: LogFileSource,
        document: LogTabDocument,
        options: SearchOptions,
        context: &mut Context<Self>,
    ) {
        context
            .spawn(async move |view, app| {
                let results = app
                    .background_executor()
                    .spawn(async move {
                        match document {
                            LogTabDocument::InMemory(document) => {
                                search_lines(&source, document.lines.as_ref(), &options)
                            }
                            LogTabDocument::Paged(document) => {
                                let outcome = search_paged_document(&document, &options);
                                let _ = outcome.truncated;
                                outcome.results
                            }
                        }
                    })
                    .await;

                view.update(app, |view, context| {
                    view.apply_search_file_result(job_id, Ok(results), 1);
                    view.finish_search_job(job_id, context);
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 启动当前目录递归后台搜索。
    ///
    /// 业务意图：
    /// - 目录搜索可能涉及多个本地文件或压缩包成员，必须逐文件放到后台读取和解码。
    /// - 每个文件完成后立即回传进度，让搜索对话框显示真实进展，而不是等全部完成才更新。
    /// - 串行搜索能避免同一压缩包被多个后台任务并发打开和解压，适合日志包内大量成员的常见场景。
    pub(in crate::app) fn spawn_current_directory_search(
        &self,
        job_id: usize,
        sources: Vec<LogFileSource>,
        options: SearchOptions,
        context: &mut Context<Self>,
    ) {
        let total_files = sources.len();
        context
            .spawn(async move |view, app| {
                for source in sources {
                    let search_options = options.clone();
                    let result = app
                        .background_executor()
                        .spawn(async move { Self::search_one_source(source, search_options) })
                        .await;

                    let should_continue = view
                        .update(app, |view, context| {
                            let current =
                                view.apply_search_file_result(job_id, result, total_files);
                            context.notify();
                            current
                        })
                        .unwrap_or(false);

                    if !should_continue {
                        return;
                    }
                }

                view.update(app, |view, context| {
                    view.finish_search_job(job_id, context);
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 搜索单个文件来源。
    ///
    /// 业务意图：
    /// - 目录搜索中的每个文件独立读取、自动解码和搜索；失败时转换为文件级错误，调用方继续处理其它文件。
    /// - 这里复用现有大小上限和编码检测逻辑，避免搜索路径绕过日志打开边界。
    pub(in crate::app) fn search_one_source(
        source: LogFileSource,
        options: SearchOptions,
    ) -> Result<Vec<SearchResultItem>, SearchFileError> {
        let file_name = source.display_name();
        let opened = open_log_source_for_tab(source.clone(), EncodingChoice::Auto, &file_name)
            .map_err(|error| SearchFileError {
                file_name: file_name.clone(),
                message: error.to_string(),
            })?;

        match opened {
            LargeLogOpenResult::InMemoryReady { document, .. } => {
                Ok(search_lines(&source, &document.lines, &options))
            }
            LargeLogOpenResult::InMemoryDecodeFailed { message, .. } => {
                Err(SearchFileError { file_name, message })
            }
            LargeLogOpenResult::PagedReady { document } => {
                let outcome = search_paged_document(&document, &options);
                let _ = outcome.truncated;
                if let Some(temp_path) = document.materialized_temp_path.as_deref() {
                    cleanup_materialized_file(temp_path);
                }
                Ok(outcome.results)
            }
        }
    }

    /// 合并单个文件的搜索结果并返回任务是否仍然有效。
    ///
    /// 业务意图：
    /// - 后台任务回到 UI 线程时必须校验任务 ID，避免旧任务覆盖新搜索状态。
    /// - 文件级错误只累积到结果面板，不中断当前任务。
    pub(in crate::app) fn apply_search_file_result(
        &mut self,
        job_id: usize,
        result: Result<Vec<SearchResultItem>, SearchFileError>,
        total_files: usize,
    ) -> bool {
        if !self.is_current_search_job(job_id) {
            return false;
        }

        let mut matched_lines = 0usize;
        if let Some(panel) = self.search.search_results_panel.as_mut() {
            {
                let Some(record) = panel
                    .records
                    .iter_mut()
                    .find(|record| record.job_id == job_id)
                else {
                    return false;
                };
                match result {
                    Ok(mut results) => {
                        matched_lines = results.len();
                        for result in &results {
                            record.expanded_file_keys.insert(result.source_key.clone());
                        }
                        record.results.append(&mut results);
                    }
                    Err(error) => {
                        record.errors.push(error);
                    }
                }
                record.progress.searched_files += 1;
                record.progress.total_files = total_files;
                record.progress.matched_lines = record.results.len();
            }
            panel.rows = Self::search_results_panel_rows_from_records(&panel.records);
        }

        if let Some(dialog) = self.search.search_dialog.as_mut() {
            dialog.progress.searched_files += 1;
            dialog.progress.total_files = total_files;
            dialog.progress.matched_lines += matched_lines;
            dialog.message = format!(
                "正在搜索 {}/{} 个文件，已命中 {} 行",
                dialog.progress.searched_files,
                dialog.progress.total_files,
                dialog.progress.matched_lines
            );
        }

        true
    }

    /// 标记搜索任务完成并保留搜索对话框。
    ///
    /// 业务意图：
    /// - 搜索对话框既负责输入条件，也负责展示本次搜索的完成状态；任务完成后继续保留窗口，
    ///   方便用户调整关键字、点击“下一个”或再次搜索。
    /// - 结果面板保留历史记录和明细，用户可以继续查看、展开和点击定位。
    pub(in crate::app) fn finish_search_job(
        &mut self,
        job_id: usize,
        _context: &mut Context<Self>,
    ) {
        if !self.is_current_search_job(job_id) {
            return;
        }

        if let Some(panel) = self.search.search_results_panel.as_mut()
            && let Some(record) = panel
                .records
                .iter_mut()
                .find(|record| record.job_id == job_id)
        {
            record.canceled = false;
        }
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            dialog.is_searching = false;
            dialog.message = format!("搜索完成，共命中 {} 行", dialog.progress.matched_lines);
        }
    }

    /// 判断后台回调是否属于当前仍有效的搜索任务。
    pub(in crate::app) fn is_current_search_job(&self, job_id: usize) -> bool {
        self.search
            .search_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.job_id == job_id && dialog.is_searching)
    }

    /// 点击搜索结果后打开文件并滚动到命中行。
    ///
    /// 业务意图：
    /// - 搜索结果不仅用于查看，还应成为跨文件定位入口。
    /// - 如果目标文件尚未打开，则新建 tab；如果已经打开，则直接激活并滚动。
    pub(in crate::app) fn open_search_result(
        &mut self,
        result: SearchResultItem,
        context: &mut Context<Self>,
    ) {
        let source_key = result.source_key.clone();
        let line_index = result.line_index;
        let match_range = result.match_range.clone();

        if !self
            .log
            .open_tabs
            .iter()
            .any(|tab| tab.source_key == source_key)
        {
            self.open_log_file(result.source.clone(), context);
        }

        let Some(tab_index) = self
            .log
            .open_tabs
            .iter()
            .position(|tab| tab.source_key == source_key)
        else {
            return;
        };
        let tab_id = self.log.open_tabs[tab_index].id;
        let ready = matches!(
            self.log.open_tabs[tab_index].state,
            LogTabState::Ready { .. }
        );
        self.log.open_tabs[tab_index].pending_scroll_to_line = Some(line_index);
        self.log.open_tabs[tab_index].highlighted_search_line = None;
        self.log.open_tabs[tab_index].highlighted_search_match = Some(LogSearchMatchHighlight {
            line_index,
            match_range,
        });
        self.activate_tab(tab_id);
        if ready {
            self.log.open_tabs[tab_index].pending_scroll_to_line = None;
            self.scroll_log_tab_to_line(tab_id, line_index);
        }
        context.notify();
    }
}
