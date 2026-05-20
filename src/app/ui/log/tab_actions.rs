// 日志 tab 控制与日志来源打开协调。
//
// 业务意图：
// - 本文件由原主窗口工作区实现机械拆分而来，只承载 GPUI 壳层内的 日志来源打开、tab 激活、编码重载和线程分析窗口启动适配。
// - 拆分过程保持所有状态字段、事件时序和用户可见行为不变，便于后续按视图职责继续收束。

use super::*;

/// 线程日志分析窗口进度轮询间隔。
///
/// 业务意图：
/// - 后台解析只在文件边界更新共享进度，UI 轮询不需要高频；80ms 能让进度条及时响应，同时避免持续唤醒主线程。
const THREAD_ANALYSIS_PROGRESS_POLL_INTERVAL: Duration = Duration::from_millis(80);

impl MainView {
    /// 打开日志智能分析独立窗口。
    ///
    /// 业务意图：
    /// - 右键菜单只负责收集来源，真实模型配置选择、窗口复用和后台任务启动集中在这里。
    /// - 使用默认模型配置；默认 ID 缺失时回退第一条配置，保持和 AI 对话新会话的默认选择一致。
    pub(in crate::app) fn open_log_ai_analysis_for_sources(
        &mut self,
        sources: Vec<LogFileSource>,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if sources.is_empty() {
            return;
        }
        let Some(profile) = self.default_log_ai_analysis_model_profile() else {
            return;
        };
        let targets = sources
            .into_iter()
            .map(LogAiAnalysisTarget::from_source)
            .collect::<Vec<_>>();
        let main_view = context.entity();
        window.defer(context, move |_window, app| {
            Self::open_log_ai_analysis_window_after_main_update(main_view, profile, targets, app);
        });
    }

    /// 返回日志智能分析使用的默认模型配置。
    pub(in crate::app) fn default_log_ai_analysis_model_profile(&self) -> Option<ModelProfile> {
        let default_id = ai_chat_default_model_profile_id(
            &self.model_config.model_config_profiles,
            self.model_config.model_config_default_profile_id.as_deref(),
        )?;
        self.model_config
            .model_config_profiles
            .iter()
            .find(|profile| profile.id == default_id)
            .cloned()
    }

    /// 在主视图更新租借结束后打开或更新日志智能分析窗口。
    ///
    /// 业务意图：
    /// - 智能分析窗口可重复使用；用户再次右键分析时直接替换任务并激活窗口。
    /// - 窗口创建需要在 `App` 上下文中执行，避免在菜单点击的 `MainView` 更新栈里重入读取同一个视图。
    pub(in crate::app) fn open_log_ai_analysis_window_after_main_update(
        main_view: Entity<MainView>,
        profile: ModelProfile,
        targets: Vec<LogAiAnalysisTarget>,
        app: &mut App,
    ) {
        let existing_window = main_view.update(app, |view, context| {
            view.log.log_tree_context_menu = None;
            context.notify();
            view.log_ai_analysis_window
        });
        if let Some(window_handle) = existing_window {
            if window_handle
                .update(app, |window_view, window, context| {
                    window_view.restart(profile.clone(), targets.clone(), context);
                    window.activate_window();
                })
                .is_ok()
            {
                return;
            }
            main_view.update(app, |view, _| {
                view.log_ai_analysis_window = None;
            });
        }

        let main_view_for_window = main_view.clone();
        let main_view_for_close = main_view.clone();
        let profile_for_window = profile.clone();
        let targets_for_window = targets.clone();
        let window_options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("日志智能分析".into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::centered(
                size(
                    px(LOG_AI_ANALYSIS_WINDOW_WIDTH),
                    px(LOG_AI_ANALYSIS_WINDOW_HEIGHT),
                ),
                app,
            )),
            is_resizable: true,
            is_minimizable: true,
            window_min_size: Some(size(px(760.0), px(460.0))),
            ..Default::default()
        };

        match app.open_window(window_options, move |window, app| {
            window.on_window_should_close(app, move |_, app| {
                main_view_for_close.update(app, |view, context| {
                    view.log_ai_analysis_window = None;
                    context.notify();
                });
                true
            });
            app.new(|context| {
                LogAiAnalysisWindowView::new(
                    main_view_for_window,
                    profile_for_window,
                    targets_for_window,
                    context,
                )
            })
        }) {
            Ok(window_handle) => {
                main_view.update(app, |view, context| {
                    view.log_ai_analysis_window = Some(window_handle);
                    context.notify();
                });
            }
            Err(_) => {
                main_view.update(app, |view, context| {
                    view.log_ai_analysis_window = None;
                    context.notify();
                });
            }
        }
    }

    pub(in crate::app) fn open_thread_analysis_for_sources(
        &mut self,
        sources: Vec<LogFileSource>,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if sources.is_empty() {
            return;
        }
        let source_count = sources.len();
        self.thread_analysis_generation = self.thread_analysis_generation.saturating_add(1);
        let generation = self.thread_analysis_generation;
        let mut filter_rules = Self::parse_thread_analysis_name_filter_rules(
            self.thread_analysis_name_filter_effective_text(),
        );
        filter_rules.extend(Self::parse_thread_analysis_filter_rules(
            self.thread_analysis_filter_effective_text(),
        ));
        let main_view = context.entity();
        let main_view_for_loading = main_view.clone();
        let initial_progress = ThreadAnalysisProgress::new(source_count);
        let progress_snapshot = Arc::new(std::sync::Mutex::new(initial_progress.clone()));
        let task_finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let loading_analysis = ThreadAnalysisData {
            title: "线程日志分析".to_string(),
            summary: format!("正在分析 {} 个文件...", source_count),
            progress: Some(initial_progress),
            snapshots: Vec::new(),
            thread_names: Vec::new(),
            matrix: Vec::new(),
        };
        // 先在当前事件循环结束后打开窗口，给用户即时反馈；后台读取和解析完成后再替换为真实结果。
        window.defer(context, move |_window, app| {
            Self::open_thread_analysis_window_if_current_after_main_update(
                main_view_for_loading,
                generation,
                loading_analysis,
                app,
            );
        });
        let main_view_for_progress = main_view.clone();
        let progress_snapshot_for_poll = Arc::clone(&progress_snapshot);
        let task_finished_for_poll = Arc::clone(&task_finished);
        context
            .spawn(async move |_view, app| {
                loop {
                    app.background_executor()
                        .timer(THREAD_ANALYSIS_PROGRESS_POLL_INTERVAL)
                        .await;
                    if task_finished_for_poll.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    let progress = progress_snapshot_for_poll
                        .lock()
                        .ok()
                        .map(|guard| guard.clone());
                    let Some(progress) = progress else {
                        continue;
                    };
                    let progress_analysis = ThreadAnalysisData {
                        title: "线程日志分析".to_string(),
                        summary: progress.message(),
                        progress: Some(progress),
                        snapshots: Vec::new(),
                        thread_names: Vec::new(),
                        matrix: Vec::new(),
                    };
                    let main_view_for_update = main_view_for_progress.clone();
                    app.update(move |app| {
                        Self::open_thread_analysis_window_if_current_after_main_update(
                            main_view_for_update,
                            generation,
                            progress_analysis,
                            app,
                        );
                    })
                    .ok();
                }
            })
            .detach();
        context
            .spawn(async move |view, app| {
                let progress_for_worker = Arc::clone(&progress_snapshot);
                let analysis = app
                    .background_executor()
                    .spawn(async move {
                        Self::analyze_thread_dump_sources_with_progress(
                            &sources,
                            &filter_rules,
                            |progress| {
                                if let Ok(mut current_progress) = progress_for_worker.lock() {
                                    *current_progress = progress;
                                }
                            },
                        )
                    })
                    .await;
                task_finished.store(true, std::sync::atomic::Ordering::Relaxed);

                let _ = view;
                app.update(move |app| {
                    Self::open_thread_analysis_window_if_current_after_main_update(
                        main_view, generation, analysis, app,
                    );
                })
                .ok();
            })
            .detach();
    }

    /// 读取并分析多个日志来源中的 Java thread dump，并汇报文件级进度。
    ///
    /// 业务意图：
    /// - 主视图后台任务通过该薄适配把进度写入共享快照，窗口轮询后显示进度条。
    pub(in crate::app) fn analyze_thread_dump_sources_with_progress<F>(
        sources: &[LogFileSource],
        filter_rules: &[ThreadAnalysisFilterRule],
        report_progress: F,
    ) -> ThreadAnalysisData
    where
        F: FnMut(ThreadAnalysisProgress),
    {
        analyze_thread_dump_sources_with_progress(sources, filter_rules, report_progress)
    }

    /// 解析线程日志分析过滤配置文本。
    ///
    /// 业务意图：
    /// - 该薄适配保留旧调用点；真实规则位于顶层 `thread_analysis`。
    pub(in crate::app) fn parse_thread_analysis_filter_rules(
        raw: &str,
    ) -> Vec<ThreadAnalysisFilterRule> {
        parse_thread_analysis_filter_rules(raw)
    }

    /// 解析线程日志分析线程名过滤配置文本。
    ///
    /// 业务意图：
    /// - MainView 只保留兼容 UI 调用点的薄适配，真实解析由顶层 `thread_analysis` 业务域负责。
    pub(in crate::app) fn parse_thread_analysis_name_filter_rules(
        raw: &str,
    ) -> Vec<ThreadAnalysisFilterRule> {
        parse_thread_analysis_name_filter_rules(raw)
    }

    /// 在主视图更新租借结束后打开或更新线程分析独立窗口。
    ///
    /// 业务意图：
    /// - 分析窗口可重复使用；如果用户重新分析另一组文件，直接替换窗口内容并激活。
    /// - 窗口创建需要在 `App` 上下文中执行，避免在菜单点击的 `MainView` 更新栈里重入读取同一个视图。
    pub(in crate::app) fn open_thread_analysis_window_after_main_update(
        main_view: Entity<MainView>,
        analysis: ThreadAnalysisData,
        app: &mut App,
    ) {
        let existing_window = main_view.update(app, |view, context| {
            view.log.log_tree_context_menu = None;
            context.notify();
            view.thread_analysis_window
        });
        if let Some(window_handle) = existing_window {
            if window_handle
                .update(app, |window_view, window, context| {
                    window_view.set_analysis(analysis.clone(), context);
                    window.activate_window();
                })
                .is_ok()
            {
                return;
            }
            main_view.update(app, |view, _| {
                view.thread_analysis_window = None;
            });
        }

        let main_view_for_window = main_view.clone();
        let main_view_for_close = main_view.clone();
        let analysis_for_window = analysis.clone();
        let window_options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("线程日志分析".into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::centered(
                size(
                    px(THREAD_ANALYSIS_WINDOW_WIDTH),
                    px(THREAD_ANALYSIS_WINDOW_HEIGHT),
                ),
                app,
            )),
            is_resizable: true,
            is_minimizable: true,
            window_min_size: Some(size(px(720.0), px(420.0))),
            ..Default::default()
        };

        match app.open_window(window_options, move |window, app| {
            window.on_window_should_close(app, move |_, app| {
                main_view_for_close.update(app, |view, context| {
                    if let Some(thread_window) = view.thread_analysis_window {
                        let _ = thread_window.update(context, |thread_view, _window, context| {
                            // 线程堆栈详情窗口的数据完全来自线程分析窗口；父窗口关闭时必须同步关闭，
                            // 否则加载新日志后主视图已经失去父窗口句柄，无法再清理旧详情窗口。
                            if let Some(stack_window) = thread_view.stack_window.take() {
                                let _ = stack_window.update(context, |_, stack_window, _| {
                                    stack_window.remove_window();
                                });
                            }
                        });
                    }
                    view.thread_analysis_window = None;
                    context.notify();
                });
                true
            });
            app.new(|context| {
                ThreadAnalysisWindowView::new(main_view_for_window, analysis_for_window, context)
            })
        }) {
            Ok(window_handle) => {
                main_view.update(app, |view, context| {
                    view.thread_analysis_window = Some(window_handle);
                    context.notify();
                });
            }
            Err(_) => {
                main_view.update(app, |view, context| {
                    view.thread_analysis_window = None;
                    context.notify();
                });
            }
        }
    }

    /// 仅当线程分析代次仍然有效时打开或更新分析窗口。
    ///
    /// 业务意图：
    /// - 旧后台任务的进度或结果可能晚于新任务返回；代次检查可以防止旧分析覆盖用户刚启动的新分析。
    pub(in crate::app) fn open_thread_analysis_window_if_current_after_main_update(
        main_view: Entity<MainView>,
        generation: usize,
        analysis: ThreadAnalysisData,
        app: &mut App,
    ) {
        let is_current = main_view.update(app, |view, _context| {
            view.thread_analysis_generation == generation
        });
        if is_current {
            Self::open_thread_analysis_window_after_main_update(main_view, analysis, app);
        }
    }

    /// 判断某个日志目录树节点当前是否展开。
    ///
    /// 业务意图：
    /// - 渲染虚拟列表可见行时需要给文件夹选择正确箭头方向。
    ///
    /// 边界条件：
    /// - 非已加载状态下没有目录树，统一返回 `false`，避免旧事件或重绘路径访问不存在的树状态。
    pub(in crate::app) fn is_log_tree_node_expanded(&self, node_id: usize) -> bool {
        match &self.log.load_state {
            LogTreeLoadState::Loaded(tree_state) => tree_state.is_expanded(node_id),
            LogTreeLoadState::Empty
            | LogTreeLoadState::Loading { .. }
            | LogTreeLoadState::Failed { .. } => false,
        }
    }

    /// 返回单文件压缩包根节点可以直接打开的内部文件来源。
    ///
    /// 业务意图：
    /// - 该方法把“压缩包只有一个文件时直接打开”的交互规则收口在主视图，避免渲染函数理解完整树扫描细节。
    /// - 非加载状态和非压缩包节点统一返回 `None`，调用方可以继续走普通展开/收起逻辑。
    pub(in crate::app) fn single_file_archive_source(
        &self,
        node_id: usize,
    ) -> Option<LogFileSource> {
        match &self.log.load_state {
            LogTreeLoadState::Loaded(tree_state) => {
                tree_state.single_file_source_for_archive(node_id)
            }
            LogTreeLoadState::Empty
            | LogTreeLoadState::Loading { .. }
            | LogTreeLoadState::Failed { .. } => None,
        }
    }

    /// 切换日志目录树节点的展开状态。
    ///
    /// 业务意图：
    /// - 用户点击文件夹或压缩包行时，左侧树应立即展开或收起对应子树。
    /// - 切换后只重建可见行缓存，完整加载结果保持不变，避免重复扫描文件系统或压缩包。
    ///
    /// 边界条件：
    /// - 只有当前已加载状态会响应；加载中、失败或未加载状态下的旧点击事件会被忽略。
    pub(in crate::app) fn toggle_log_tree_node(
        &mut self,
        node_id: usize,
        context: &mut Context<Self>,
    ) {
        if let LogTreeLoadState::Loaded(tree_state) = &mut self.log.load_state {
            tree_state.toggle_node(node_id);
            context.notify();
        }
    }

    /// 打开左侧目录树中的日志文件。
    ///
    /// 业务意图：
    /// - 普通文件和压缩包成员都通过 `LogFileSource` 统一进入右侧 tab 工作区。
    /// - 同一个来源重复点击时只激活已有 tab，避免用户误打开多个相同文件。
    ///
    /// 边界条件：
    /// - 读取和自动解码都放到后台执行器，避免大文件或压缩包流式读取阻塞 GPUI 主线程。
    /// - 如果 tab 在后台任务完成前被关闭，结果合并时会因找不到 ID 而被忽略。
    pub(in crate::app) fn open_log_file(
        &mut self,
        source: LogFileSource,
        context: &mut Context<Self>,
    ) {
        let source_key = source.stable_key();
        if let Some(tab) = self
            .log
            .open_tabs
            .iter()
            .find(|tab| tab.source_key == source_key)
        {
            self.activate_tab(tab.id);
            context.notify();
            return;
        }

        let tab_id = self.log.next_tab_id;
        self.log.next_tab_id += 1;
        let title = source.display_name();
        if self.log.open_tabs.is_empty() {
            self.log.tab_bar_scroll_handle = ScrollHandle::new();
        }
        self.log.open_tabs.push(OpenLogTab {
            id: tab_id,
            source: source.clone(),
            source_key,
            title,
            encoding_choice: EncodingChoice::Auto,
            raw_bytes: None,
            state: LogTabState::Loading {
                message: "正在读取日志文件...".to_string(),
            },
            scroll_handle: UniformListScrollHandle::new(),
            paged_viewport_handle: ScrollHandle::new(),
            paged_scroll: PagedLogScrollState::default(),
            pending_scroll_to_line: None,
            highlighted_search_line: None,
            highlighted_search_match: None,
            marked_lines: BTreeSet::new(),
            last_marker_jump_line: None,
            text_selection: None,
            selection_drag_anchor: None,
        });
        self.activate_tab(tab_id);
        context.notify();
        self.spawn_log_tab_load(tab_id, source, context);
    }

    /// 激活指定日志 tab 并确保 tab 页签在横向滚动区域内可见。
    ///
    /// 业务意图：
    /// - 打开、点击、右键和后台内容刷新都会改变用户关注的 tab，tab 栏需要自动滚动到能看到当前 tab 的位置。
    /// - 将激活逻辑集中到一个方法，避免不同入口遗漏关闭弹层或遗漏页签自动定位。
    ///
    /// 边界条件：
    /// - 如果 tab 已经被关闭或不存在，直接忽略，避免旧异步任务或过期点击事件激活不存在的页面。
    pub(in crate::app) fn activate_tab(&mut self, tab_id: usize) {
        if !self.log.open_tabs.iter().any(|tab| tab.id == tab_id) {
            return;
        }

        self.log.active_tab_id = Some(tab_id);
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
        self.clear_search_current_file_match_count();
        self.scroll_tab_bar_to_tab(tab_id);
    }

    /// 将指定 tab 页签滚动到可视范围内。
    ///
    /// 业务意图：
    /// - tab 标题完整展示后，页签总宽度可能超过右侧区域；激活 tab 时自动定位能减少用户手动点箭头的次数。
    /// - GPUI `ScrollHandle::scroll_to_item` 会在下一次布局时按 child 下标做最小滚动，适合这里的横向页签列表。
    pub(in crate::app) fn scroll_tab_bar_to_tab(&self, tab_id: usize) {
        if let Some(index) = self.log.open_tabs.iter().position(|tab| tab.id == tab_id) {
            self.log.tab_bar_scroll_handle.scroll_to_item(index);
        }
    }

    /// 将指定 tab 的日志正文滚动到目标行。
    ///
    /// 业务意图：
    /// - 搜索结果点击后需要把命中行带到用户视野中央，而不是只打开文件让用户手动查找。
    /// - 使用 `UniformListScrollHandle::scroll_to_item_strict` 延迟到下一次布局执行，适合虚拟列表尚未完成测量的场景。
    ///
    /// 边界条件：
    /// - 只有已打开 tab 才能滚动；如果 tab 仍在加载，调用方应先把行号写入 `pending_scroll_to_line`。
    /// - 行号来自搜索时的解码结果，如果文件在搜索后被外部修改，滚动位置可能不再对应同一内容，这是当前未实现文件监听的已知边界。
    pub(in crate::app) fn scroll_log_tab_to_line(&mut self, tab_id: usize, line_index: usize) {
        if let Some(tab) = self.log.open_tabs.iter_mut().find(|tab| tab.id == tab_id) {
            match &tab.state {
                LogTabState::Ready { document } => match document.as_ref() {
                    LogTabDocument::Paged(document) => {
                        // 分页日志不再把完整行数交给 GPUI，因此跳转行号时直接更新应用侧的逻辑滚动位置。
                        // 首帧还没有真实视口高度时使用一屏常见行数估算，下一帧滚动条会按实际 bounds 修正比例。
                        let viewport_height = tab.paged_viewport_handle.bounds().size.height;
                        let fallback_height = px(LOG_VIEWER_ROW_HEIGHT * 24.0);
                        let viewport_height = if viewport_height > px(0.0) {
                            viewport_height
                        } else {
                            fallback_height
                        };
                        tab.paged_scroll.top_px = Self::paged_log_scroll_top_for_line(
                            line_index,
                            document.line_count(),
                            viewport_height,
                        );
                    }
                    LogTabDocument::InMemory(_) => {
                        tab.scroll_handle
                            .scroll_to_item_strict(line_index, ScrollStrategy::Center);
                    }
                },
                LogTabState::Loading { .. } | LogTabState::Failed { .. } => {}
            }
        }
    }

    /// 打开日志来源并定位到指定行。
    ///
    /// 业务意图：
    /// - 线程分析窗口和搜索结果都需要“打开文件并跳到某行”的行为；集中实现可以保证新建 tab、
    ///   已打开 tab、加载中 tab 的滚动和高亮状态一致。
    ///
    /// 边界条件：
    /// - 如果目标 tab 仍在后台读取，先记录 `pending_scroll_to_line`，等加载完成后再滚动。
    /// - 行号来自当前解析结果；如果文件在分析后被外部修改，定位可能落到相邻内容，这是无文件监听条件下的既有风险。
    pub(in crate::app) fn open_log_source_at_line(
        &mut self,
        source: LogFileSource,
        line_index: usize,
        context: &mut Context<Self>,
    ) {
        let source_key = source.stable_key();
        if !self
            .log
            .open_tabs
            .iter()
            .any(|tab| tab.source_key == source_key)
        {
            self.open_log_file(source, context);
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
        self.log.open_tabs[tab_index].highlighted_search_line = Some(line_index);
        self.activate_tab(tab_id);
        if ready {
            self.log.open_tabs[tab_index].pending_scroll_to_line = None;
            self.scroll_log_tab_to_line(tab_id, line_index);
        }
        context.notify();
    }

    /// 启动日志 tab 的后台读取任务。
    ///
    /// 业务意图：
    /// - 读取压缩包成员和解码大文本都可能耗时，必须离开 UI 线程执行。
    /// - 后台任务返回时只按 tab ID 合并状态，避免持有过期引用。
    pub(in crate::app) fn spawn_log_tab_load(
        &self,
        tab_id: usize,
        source: LogFileSource,
        context: &mut Context<Self>,
    ) {
        let source_name = source.display_name();
        context
            .spawn(async move |view, app| {
                let result = app
                    .background_executor()
                    .spawn(async move {
                        match open_log_source_for_tab(source, EncodingChoice::Auto, &source_name) {
                            Ok(LargeLogOpenResult::InMemoryReady {
                                raw_bytes,
                                document,
                            }) => LogTabLoadResult::Ready {
                                raw_bytes: Some(raw_bytes),
                                document: Box::new(LogTabDocument::InMemory(document)),
                            },
                            Ok(LargeLogOpenResult::InMemoryDecodeFailed { raw_bytes, message }) => {
                                LogTabLoadResult::DecodeFailed { raw_bytes, message }
                            }
                            Ok(LargeLogOpenResult::PagedReady { document }) => {
                                LogTabLoadResult::Ready {
                                    raw_bytes: None,
                                    document: Box::new(LogTabDocument::Paged(document)),
                                }
                            }
                            Err(error) => LogTabLoadResult::ReadFailed {
                                message: error.to_string(),
                            },
                        }
                    })
                    .await;

                view.update(app, |view, context| {
                    view.apply_log_tab_load_result(tab_id, result, context);
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 合并日志 tab 的后台读取结果。
    ///
    /// 业务意图：
    /// - 读取成功后保存原始字节；即使自动检测失败，也允许用户通过手动编码按钮重新解析。
    pub(in crate::app) fn apply_log_tab_load_result(
        &mut self,
        tab_id: usize,
        result: LogTabLoadResult,
        context: &mut Context<Self>,
    ) {
        let mut pending_scroll_to_line = None;
        self.drop_log_minimap_cache_for_tab(tab_id, context);
        {
            let Some(tab) = self.log.open_tabs.iter_mut().find(|tab| tab.id == tab_id) else {
                return;
            };

            tab.scroll_handle = UniformListScrollHandle::new();
            tab.paged_viewport_handle = ScrollHandle::new();
            tab.paged_scroll = PagedLogScrollState::default();
            tab.text_selection = None;
            tab.selection_drag_anchor = None;
            tab.marked_lines.clear();
            tab.last_marker_jump_line = None;
            match result {
                LogTabLoadResult::Ready {
                    raw_bytes,
                    document,
                } => {
                    tab.raw_bytes = raw_bytes;
                    tab.state = LogTabState::Ready { document };
                    pending_scroll_to_line = tab.pending_scroll_to_line.take();
                }
                LogTabLoadResult::DecodeFailed { raw_bytes, message } => {
                    tab.raw_bytes = Some(raw_bytes);
                    tab.state = LogTabState::Failed { message };
                    tab.pending_scroll_to_line = None;
                }
                LogTabLoadResult::ReadFailed { message } => {
                    tab.raw_bytes = None;
                    tab.state = LogTabState::Failed { message };
                    tab.pending_scroll_to_line = None;
                }
            }
        }

        if self.log.active_tab_id == Some(tab_id) {
            self.scroll_tab_bar_to_tab(tab_id);
            // 日志加载完成会让搜索窗口里的当前文件计数和“上一个/下一个”起点失效；
            // 但如果该 tab 是从搜索结果打开的，正文高亮已经在加载前写入，不能在这里清掉。
            self.clear_search_current_file_match_count_state();
        }
        if let Some(line_index) = pending_scroll_to_line {
            self.scroll_log_tab_to_line(tab_id, line_index);
        }
    }

    /// 为指定 tab 切换编码并重新解码。
    ///
    /// 业务意图：
    /// - 编码切换必须复用 tab 内保存的原始字节，避免重新读取压缩包成员或普通文件。
    /// - 切换后重置该 tab 的正文滚动句柄，让用户从文件开头重新检查解码结果。
    pub(in crate::app) fn select_tab_encoding(
        &mut self,
        tab_id: usize,
        encoding_choice: EncodingChoice,
        context: &mut Context<Self>,
    ) {
        let Some(tab) = self.log.open_tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };

        let raw_bytes = tab.raw_bytes.clone();
        let paged_document = match &tab.state {
            LogTabState::Ready { document } => match document.as_ref() {
                LogTabDocument::Paged(document) => Some(document.clone()),
                LogTabDocument::InMemory(_) => None,
            },
            LogTabState::Loading { .. } | LogTabState::Failed { .. } => None,
        };
        if raw_bytes.is_none() && paged_document.is_none() {
            // 原始字节或分页文档尚未读取完成时不能提前写入编码选择，否则后台自动加载完成后会出现
            // “下拉框显示手动编码、正文却来自自动识别”的状态不一致。
            self.log.tab_context_menu = None;
            self.log.encoding_dropdown_menu = None;
            self.log.log_viewer_context_menu = None;
            context.notify();
            return;
        }

        let source_name = tab.title.clone();
        tab.encoding_choice = encoding_choice;
        tab.scroll_handle = UniformListScrollHandle::new();
        tab.pending_scroll_to_line = None;
        tab.highlighted_search_line = None;
        tab.highlighted_search_match = None;
        tab.last_marker_jump_line = None;
        tab.text_selection = None;
        tab.selection_drag_anchor = None;
        tab.state = LogTabState::Loading {
            message: format!("正在按 {} 重新解析...", encoding_choice.label()),
        };
        self.drop_log_minimap_cache_for_tab(tab_id, context);
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
        if self
            .log
            .log_scrollbar_drag
            .is_some_and(|drag| drag.tab_id == tab_id)
        {
            self.log.log_scrollbar_drag = None;
        }
        if self
            .log
            .log_minimap_drag
            .is_some_and(|drag| drag.tab_id == tab_id)
        {
            self.log.log_minimap_drag = None;
        }
        context.notify();
        if let Some(raw_bytes) = raw_bytes {
            self.spawn_log_tab_decode(tab_id, raw_bytes, encoding_choice, source_name, context);
        } else if let Some(document) = paged_document {
            self.spawn_paged_log_tab_decode(tab_id, document, encoding_choice, context);
        }
    }

    /// 启动日志 tab 的后台重新解码任务。
    ///
    /// 业务意图：
    /// - 内存模式日志重新解码仍可能耗时，放到后台执行可以避免界面短暂停顿。
    pub(in crate::app) fn spawn_log_tab_decode(
        &self,
        tab_id: usize,
        raw_bytes: Arc<Vec<u8>>,
        encoding_choice: EncodingChoice,
        source_name: String,
        context: &mut Context<Self>,
    ) {
        context
            .spawn(async move |view, app| {
                let result = app
                    .background_executor()
                    .spawn(async move {
                        decode_log_bytes(&raw_bytes, encoding_choice, &source_name)
                            .map(|document| LogTabDecodeResult::Ready {
                                encoding_choice,
                                document: Box::new(LogTabDocument::InMemory(document)),
                            })
                            .unwrap_or_else(|error: LogContentError| LogTabDecodeResult::Failed {
                                encoding_choice,
                                message: error.to_string(),
                            })
                    })
                    .await;

                view.update(app, |view, context| {
                    view.apply_log_tab_decode_result(tab_id, result, context);
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 启动分页日志 tab 的后台编码切换任务。
    ///
    /// 业务意图：
    /// - 超大日志切换编码时不能重新读取压缩包或重建行索引，只更新分页文档的解码策略并清理可见行缓存。
    /// - 该操作仍放到后台执行，避免自动检测编码样本读取影响 UI 响应。
    pub(in crate::app) fn spawn_paged_log_tab_decode(
        &self,
        tab_id: usize,
        document: log_document::PagedLogDocument,
        encoding_choice: EncodingChoice,
        context: &mut Context<Self>,
    ) {
        context
            .spawn(async move |view, app| {
                let result = app
                    .background_executor()
                    .spawn(async move {
                        document
                            .with_encoding(encoding_choice)
                            .map(|document| LogTabDecodeResult::Ready {
                                encoding_choice,
                                document: Box::new(LogTabDocument::Paged(document)),
                            })
                            .unwrap_or_else(|error| LogTabDecodeResult::Failed {
                                encoding_choice,
                                message: error.to_string(),
                            })
                    })
                    .await;

                view.update(app, |view, context| {
                    view.apply_log_tab_decode_result(tab_id, result, context);
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 合并日志 tab 的重新解码结果。
    ///
    /// 边界条件：
    /// - 如果用户在解码完成前关闭了 tab，则结果会被忽略。
    /// - 如果用户在旧任务完成前再次切换编码，则旧结果会被丢弃，避免 UI 显示内容和编码按钮不一致。
    pub(in crate::app) fn apply_log_tab_decode_result(
        &mut self,
        tab_id: usize,
        result: LogTabDecodeResult,
        context: &mut Context<Self>,
    ) {
        let result_encoding_choice = match &result {
            LogTabDecodeResult::Ready {
                encoding_choice, ..
            }
            | LogTabDecodeResult::Failed {
                encoding_choice, ..
            } => *encoding_choice,
        };
        let Some(current_encoding_choice) = self
            .log
            .open_tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .map(|tab| tab.encoding_choice)
        else {
            return;
        };
        if current_encoding_choice != result_encoding_choice {
            return;
        }

        self.drop_log_minimap_cache_for_tab(tab_id, context);
        {
            let Some(tab) = self.log.open_tabs.iter_mut().find(|tab| tab.id == tab_id) else {
                return;
            };

            tab.scroll_handle = UniformListScrollHandle::new();
            tab.paged_viewport_handle = ScrollHandle::new();
            tab.paged_scroll = PagedLogScrollState::default();
            tab.text_selection = None;
            tab.selection_drag_anchor = None;
            tab.last_marker_jump_line = None;
            match result {
                LogTabDecodeResult::Ready { document, .. } => {
                    tab.state = LogTabState::Ready { document };
                }
                LogTabDecodeResult::Failed { message, .. } => {
                    tab.state = LogTabState::Failed { message };
                }
            }
        }

        if self.log.active_tab_id == Some(tab_id) {
            self.scroll_tab_bar_to_tab(tab_id);
            self.clear_search_current_file_match_count();
        }
    }
}
