//! 主窗口运行时和启动装配功能域。
//!
//! 业务意图：
//! - 该模块集中主窗口尺寸决策、窗口句柄缓存、macOS reopen、open-url 和启动参数加载流程。
//! - 根 `app` 模块只保留主视图实体和跨功能域协调，避免启动副作用与渲染状态继续混在同一个超大文件中。
//!
//! 跨平台约束：
//! - macOS 关闭最后一个窗口后进程仍可能存活，需要通过 `AsyncApp` 在平台回调中恢复主窗口。
//! - Windows 和开发期命令行启动主要通过启动参数传入路径，仍复用同一条主窗口加载入口。
//! - 窗口尺寸使用 GPUI 逻辑像素，避免直接绑定平台物理像素或显示缩放。

use super::*;

/// 判断历史尺寸是否像旧版本保存下来的最大化窗口宽度。
///
/// 业务意图：
/// - 大屏下无历史尺寸应默认使用 1600x900；如果旧配置保存了最大化宽度，继续尊重它会让窗口看起来仍然最大化。
/// - 这里只过滤接近当前主显示器宽度的历史值，普通用户手动调整过的窗口尺寸仍继续恢复。
///
/// 边界条件：
/// - 显示器宽度读取失败时不能判断是否最大化残留，保守保留历史尺寸。
/// - 小屏仍保留“历史尺寸优先”规则，避免用户在小屏上手动调整出的宽窗口被误判为最大化残留。
fn saved_size_looks_maximized(
    saved_size: MainWindowSizePreference,
    primary_display_width: Option<f32>,
) -> bool {
    let Some(display_width) = primary_display_width else {
        return false;
    };
    if !display_width.is_finite() || display_width <= 0.0 {
        return false;
    }
    if display_width < SMALL_SCREEN_MAXIMIZED_WIDTH_THRESHOLD {
        return false;
    }
    saved_size.width >= display_width * MAXIMIZED_RESTORED_WIDTH_RATIO
}

/// 主窗口首次启动时的尺寸策略。
///
/// 业务意图：
/// - 该枚举把“是否有历史尺寸”和“小屏默认最大化”的产品规则拆成纯数据，便于单元测试覆盖。
/// - 真正转换为 GPUI `WindowBounds` 时再依赖 `App`，避免测试环境必须启动真实窗口系统。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum MainWindowStartupDecision {
    /// 使用用户上次保存的宽高，窗口启动时仍重新居中。
    Remembered(MainWindowSizePreference),
    /// 使用固定 1600x900 居中窗口，适用于大屏或无法读取显示器信息的场景。
    DefaultWindowed,
    /// 使用系统最大化窗口，适用于首次在小屏电脑启动。
    DefaultMaximized,
}

/// 根据历史宽高和主显示器宽度决定主窗口启动策略。
///
/// 业务意图：
/// - 历史宽高代表用户明确调整过窗口大小，优先级高于小屏默认最大化。
/// - 没有历史宽高时，才按主显示器逻辑像素宽度判断是否最大化。
///
/// 边界条件：
/// - 显示器宽度读取失败时按大屏窗口化处理，避免在图形环境信息不完整时强行最大化。
pub(crate) fn decide_main_window_startup(
    saved_size: Option<MainWindowSizePreference>,
    primary_display_width: Option<f32>,
) -> MainWindowStartupDecision {
    if let Some(saved_size) = saved_size
        && !saved_size_looks_maximized(saved_size, primary_display_width)
    {
        return MainWindowStartupDecision::Remembered(saved_size);
    }

    if primary_display_width.is_some_and(|width| width < SMALL_SCREEN_MAXIMIZED_WIDTH_THRESHOLD) {
        MainWindowStartupDecision::DefaultMaximized
    } else {
        MainWindowStartupDecision::DefaultWindowed
    }
}

/// 把启动窗口尺寸限制在当前主显示器可见尺寸内。
///
/// 业务意图：
/// - 主窗口默认宽度是 1600px，但部分 macOS/Windows 设备的逻辑宽度介于 1440px 和 1600px 之间，
///   这类设备不会触发“小屏默认最大化”，如果仍按 1600px 居中，系统可能把超出屏幕的窗口挤回可见区域，导致启动时视觉上偏右。
/// - 启动前先把普通窗口尺寸限制到显示器尺寸内，再执行居中计算，可以保证首次打开和恢复历史尺寸时窗口中心落在屏幕中心。
///
/// 边界条件：
/// - 只在显示器宽高是有限正数时裁剪；图形环境读取失败或返回异常值时保留原尺寸，由 GPUI 原有默认策略兜底。
/// - 只裁剪超过屏幕的尺寸，不放大用户保存的小窗口，避免改变用户手动调整过的工作区尺寸偏好。
pub(crate) fn fit_main_window_size_to_display(
    requested_size: MainWindowSizePreference,
    display_size: Option<(f32, f32)>,
) -> MainWindowSizePreference {
    let Some((display_width, display_height)) = display_size else {
        return requested_size;
    };
    if !display_width.is_finite()
        || !display_height.is_finite()
        || display_width <= 0.0
        || display_height <= 0.0
    {
        return requested_size;
    }

    MainWindowSizePreference {
        width: requested_size.width.min(display_width),
        height: requested_size.height.min(display_height),
    }
}

/// 把主窗口启动策略转换成 GPUI 窗口边界。
///
/// 业务意图：
/// - 历史宽高和大屏默认都以居中窗口打开，不恢复上次位置。
/// - 小屏默认最大化时仍把 1600x900 作为恢复尺寸交给 GPUI，用户退出最大化后能得到稳定默认宽高。
fn main_window_bounds_for_decision(
    decision: MainWindowStartupDecision,
    display_id: Option<DisplayId>,
    display_size: Option<(f32, f32)>,
    app: &App,
) -> WindowBounds {
    let requested_size = match decision {
        MainWindowStartupDecision::Remembered(saved_size) => saved_size,
        MainWindowStartupDecision::DefaultWindowed
        | MainWindowStartupDecision::DefaultMaximized => MainWindowSizePreference {
            width: MAIN_WINDOW_WIDTH,
            height: MAIN_WINDOW_HEIGHT,
        },
    };
    let fitted_size = fit_main_window_size_to_display(requested_size, display_size);
    let centered_size = size(px(fitted_size.width), px(fitted_size.height));

    match decision {
        MainWindowStartupDecision::Remembered(_) | MainWindowStartupDecision::DefaultWindowed => {
            WindowBounds::Windowed(Bounds::centered(display_id, centered_size, app))
        }
        MainWindowStartupDecision::DefaultMaximized => {
            WindowBounds::Maximized(Bounds::centered(display_id, centered_size, app))
        }
    }
}

/// 读取当前主显示器的逻辑像素宽度。
///
/// 跨平台约束：
/// - GPUI 负责把 macOS Retina、Windows 缩放和平台显示器坐标转换为逻辑像素。
/// - 如果应用启动早期无法取得主显示器，则返回 `None`，由启动策略回退到固定窗口化。
fn primary_display_width(app: &App) -> Option<f32> {
    app.primary_display()
        .map(|display| display.bounds().size.width / px(1.0))
}

/// 读取当前主显示器的逻辑像素宽高。
///
/// 业务意图：
/// - 主窗口启动居中前需要知道显示器可见尺寸，用于裁剪超过屏幕的默认或历史窗口尺寸。
/// - 使用 GPUI 逻辑像素，和保存的窗口尺寸单位一致，避免 macOS Retina 或 Windows 缩放下出现物理像素误差。
fn primary_display_size(app: &App) -> Option<(f32, f32)> {
    app.primary_display().map(|display| {
        let size = display.bounds().size;
        (size.width / px(1.0), size.height / px(1.0))
    })
}

/// 读取当前主显示器 ID。
///
/// 业务意图：
/// - GPUI 创建窗口和计算居中位置都支持 display id；显式使用同一个主显示器可以避免多屏环境下
///   “按一个屏幕计算中心、实际在另一个屏幕打开”导致窗口看起来靠左。
fn primary_display_id(app: &App) -> Option<DisplayId> {
    app.primary_display().map(|display| display.id())
}

/// 构造主窗口启动边界。
///
/// 业务意图：
/// - 主入口只需要调用这个函数即可获得完整窗口策略，避免把配置读取、显示器判断和 GPUI 边界构造散落在 `main` 中。
fn build_main_window_bounds(app: &App) -> WindowBounds {
    let saved_size = load_main_window_size_preference();
    let decision = decide_main_window_startup(saved_size, primary_display_width(app));
    main_window_bounds_for_decision(
        decision,
        primary_display_id(app),
        primary_display_size(app),
        app,
    )
}

/// 主窗口运行期状态。
///
/// 业务意图：
/// - macOS 关闭最后一个窗口后进程仍会保留，Dock 再次点按只会触发 reopen 事件，不会重新执行 `run`。
/// - 这里把“当前主窗口句柄”和“可从平台回调重新进入 GPUI 的异步应用句柄”集中保存，让启动、open-url 和 reopen
///   三条入口都能复用同一套主窗口恢复流程。
///
/// 边界条件：
/// - `WindowHandle` 本身不会让窗口继续存活；窗口关闭后句柄可能失效，因此每次使用前都必须通过 `update` 验证。
/// - `AsyncApp` 只在应用启动后可用；启动完成前收到的 macOS open-url 事件需要暂存，等主窗口初始化完成后再处理。
#[derive(Default)]
struct MainWindowRuntime {
    /// 当前仍可能有效的主窗口句柄。
    ///
    /// 业务意图：
    /// - 保存主窗口句柄是为了在 macOS reopen 或 Finder/Dock 打开文件时优先复用已有窗口。
    /// - 主窗口关闭回调会清空该字段；如果因为平台时序导致仍残留旧句柄，后续 `ensure_main_window` 会通过 `update`
    ///   失败识别并创建新窗口。
    main_window: Option<WindowHandle<MainView>>,

    /// 可在平台回调中重新进入 GPUI 主线程的应用句柄。
    ///
    /// 业务意图：
    /// - `Application::on_open_urls` 不直接提供 `App`，但 macOS 可能在应用已经启动后继续从 Finder 或 Dock 交付文件。
    /// - 保存 `AsyncApp` 后，open-url 回调可以在当前进程内恢复主窗口并加载路径，而不是因为主窗口关闭而静默丢弃请求。
    async_app: Option<AsyncApp>,
}

impl MainWindowRuntime {
    /// 记录当前可用的异步应用句柄。
    ///
    /// 边界条件：
    /// - 该方法只保存 GPUI 提供的弱引用包装，不持有窗口或实体所有权，因此不会阻止应用正常退出。
    fn remember_app(&mut self, app: &App) {
        self.async_app = Some(app.to_async());
    }

    /// 记录一个刚创建或刚验证过仍有效的主窗口。
    ///
    /// 业务意图：
    /// - 同时刷新 `AsyncApp`，保证后续 open-url 回调使用的是最新应用上下文。
    fn remember_window(&mut self, main_window: WindowHandle<MainView>, app: &App) {
        self.main_window = Some(main_window);
        self.remember_app(app);
    }

    /// 清空主窗口句柄。
    ///
    /// 边界条件：
    /// - 只清理窗口句柄，不清理 `AsyncApp`；macOS 关闭所有窗口后仍需要通过 `AsyncApp` 响应 Finder/Dock 事件。
    fn clear_window(&mut self) {
        self.main_window = None;
    }

    /// 返回可供平台回调使用的异步应用句柄快照。
    ///
    /// 边界条件：
    /// - 返回 clone 是为了立即释放 `RefCell` 借用，避免在后续进入 GPUI 更新流程时发生运行期借用冲突。
    fn async_app(&self) -> Option<AsyncApp> {
        self.async_app.clone()
    }
}

/// 构造主窗口选项。
///
/// 业务意图：
/// - 启动创建窗口和 macOS reopen 恢复窗口必须使用同一套标题、尺寸和显示器策略。
/// - 把 `WindowOptions` 集中到这里可以避免后续新增菜单栏、最小尺寸或平台差异时只改了一条入口。
fn build_main_window_options(app: &App) -> WindowOptions {
    WindowOptions {
        // 显式设置系统标题栏标题，保证 macOS 和 Windows 的原生窗口标题都使用产品名。
        // 后续如果标题需要包含文件名或状态，应在业务规则明确后统一修改这里的标题策略。
        titlebar: Some(TitlebarOptions {
            title: Some(MAIN_WINDOW_TITLE.into()),
            ..Default::default()
        }),
        // 使用统一启动策略决定主窗口边界：历史宽高优先，其次小屏最大化，最后大屏固定 1600x900 居中。
        // 这里不恢复历史位置，并且显式绑定主显示器，避免多屏环境下计算居中和实际打开使用不同屏幕。
        window_bounds: Some(build_main_window_bounds(app)),
        display_id: primary_display_id(app),
        ..Default::default()
    }
}

/// 在主窗口关闭前保存可恢复的窗口尺寸。
///
/// 业务意图：
/// - 用户手动调整后的普通窗口尺寸应跨会话保留，保证日志查看工作区再次打开时仍符合用户习惯。
///
/// 边界条件：
/// - 最大化和全屏是平台窗口状态，不是用户希望下次以超大普通窗口打开的尺寸，因此跳过保存。
/// - 保存失败不阻止关闭，避免配置目录权限问题导致用户无法关闭日志查看客户端。
fn save_main_window_size_before_close(window: &Window) {
    if window.is_maximized() || window.is_fullscreen() {
        return;
    }

    let bounds = window.window_bounds().get_bounds();
    let width = bounds.size.width / px(1.0);
    let height = bounds.size.height / px(1.0);
    if let Some(size) = MainWindowSizePreference::new(width, height) {
        save_main_window_size_preference(size);
    }
}

/// 查找当前仍打开的主窗口。
///
/// 业务意图：
/// - 防御运行期状态丢失或句柄缓存被清空但窗口仍存在的情况，避免 macOS reopen 或 open-url 创建重复主窗口。
///
/// 边界条件：
/// - `AnyWindowHandle::downcast` 只按根视图类型判断；当前应用只有一个 `MainView` 主窗口，辅助窗口使用独立根视图类型。
fn find_open_main_window(app: &App) -> Option<WindowHandle<MainView>> {
    app.windows()
        .into_iter()
        .find_map(|window| window.downcast::<MainView>())
}

/// 激活一个已知主窗口，并确认句柄仍有效。
///
/// 业务意图：
/// - macOS reopen 和 Finder/Dock 打开文件时，如果主窗口仍存在，应优先把它带回前台，而不是创建第二个主窗口。
///
/// 边界条件：
/// - 如果窗口已经关闭或根视图类型不匹配，`update` 会失败；调用方据此清理缓存并创建新窗口。
fn activate_main_window(main_window: WindowHandle<MainView>, app: &mut App) -> bool {
    main_window
        .update(app, |view, window, _context| {
            view.main_window = Some(main_window);
            window.activate_window();
        })
        .is_ok()
}

/// 为主窗口安装全局快捷键拦截器。
///
/// 业务意图：
/// - GPUI 的应用级拦截器需要绑定一个当前有效的主窗口句柄，才能把 `Cmd+F`、复制等跨焦点快捷键派发回主视图。
/// - macOS 关闭主窗口后旧 `MainView` 会释放订阅；reopen 创建新主窗口时必须重新安装，否则新窗口的全局快捷键会失效。
///
/// 边界条件：
/// - 拦截器捕获的更新异常不能越过 Objective-C key equivalent 边界，否则 macOS 运行时可能直接 abort。
/// - 订阅保存到主视图里，让窗口关闭时自动释放，不需要额外的全局清理逻辑。
fn install_main_window_keystroke_subscription(main_window: WindowHandle<MainView>, app: &mut App) {
    let main_view_for_keys = main_window;
    let subscription = app.intercept_keystrokes(move |event, window, app| {
        // 应用级快捷键拦截器会收到插件窗口、线程分析窗口、搜索窗口等所有辅助窗口的按键。
        // 只有主窗口按键才应该走主视图的“粘贴到搜索框”等兜底逻辑；否则插件窗口里 `Cmd+V`
        // 会被误解释为日志区粘贴搜索，进而弹出搜索对话框。
        if AnyWindowHandle::from(main_view_for_keys) != window.window_handle() {
            return;
        }
        // GPUI 0.2.2 在 macOS 上会从 Objective-C `keyEquivalent` 回调进入这里；该回调不能让 Rust panic
        // 继续向外 unwind，否则运行时会直接 abort。快捷键处理本身不是不可恢复业务，因此这里在边界处兜住
        // 我们自己的状态更新异常，并让事件继续按默认路径传播，避免一次快捷键输入击穿整个进程。
        let handled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            main_view_for_keys
                .update(app, |view, _window, context| {
                    // 应用级拦截器服务搜索、复制和粘贴这类跨焦点快捷键；区域键盘滚动只允许主窗口根节点处理，
                    // 避免设置窗口或其它辅助窗口按 PageUp/PageDown 时滚动背后的日志内容并消费事件。
                    view.handle_global_keystroke(event.keystroke.clone(), window, context, false)
                })
                .unwrap_or(false)
        }))
        .unwrap_or(false);
        if handled {
            app.stop_propagation();
        }
    });
    main_window
        .update(app, |view, _window, _context| {
            view.global_keystroke_subscription = Some(subscription);
        })
        .ok();
}

/// 创建新的主窗口。
///
/// 业务意图：
/// - 启动和 macOS reopen 都通过该函数创建窗口，保证主题观察、关闭保存、焦点和全局快捷键订阅完全一致。
///
/// 边界条件：
/// - 该函数只创建空主窗口；启动参数、Finder/Dock 打开的路径由调用方在窗口创建成功后再加载，避免窗口创建失败时丢失错误边界。
fn create_main_window(
    runtime: &Rc<RefCell<MainWindowRuntime>>,
    app: &mut App,
) -> Result<WindowHandle<MainView>, String> {
    // 主窗口创建前先同步原生 AppKit 外观，避免 macOS 标题栏在应用强制暗色/亮色时仍停留在系统默认外观。
    apply_native_theme_preference(load_theme_preference());
    let window_options = build_main_window_options(app);
    let runtime_for_close = Rc::clone(runtime);
    let main_window = app
        .open_window(window_options, move |window, app| {
            let view = app.new(|context| {
                let mut view = MainView::new(context);
                view.system_window_appearance = window.appearance();
                view.window_appearance_subscription = Some(context.observe_window_appearance(
                    window,
                    |view, window, context| {
                        view.set_system_window_appearance(window.appearance(), context);
                    },
                ));
                view
            });
            window.on_window_should_close(app, move |window, _app| {
                save_main_window_size_before_close(window);
                runtime_for_close.borrow_mut().clear_window();
                true
            });
            window.focus(&view.read(app).root_focus_handle);
            view
        })
        .map_err(|error| format!("创建 LogClinic 主窗口失败：{error}"))?;

    main_window
        .update(app, |view, _window, context| {
            // 主窗口句柄只能在 `open_window` 成功返回后获得；回填到主视图供独立工具窗口激活主窗口使用。
            view.main_window = Some(main_window);
            context.notify();
        })
        .map_err(|error| format!("初始化 LogClinic 主窗口状态失败：{error}"))?;
    install_main_window_keystroke_subscription(main_window, app);
    runtime.borrow_mut().remember_window(main_window, app);

    Ok(main_window)
}

/// 确保当前进程内有可用主窗口。
///
/// 业务意图：
/// - macOS 关闭所有窗口后进程不退出，Dock 再点时必须恢复主窗口。
/// - Finder/Dock 在无窗口状态下再次打开日志文件时，也必须先恢复窗口再加载文件。
///
/// 边界条件：
/// - 优先验证缓存句柄，失败后再扫描 GPUI 当前窗口列表，最后才创建新窗口，避免重复窗口。
fn ensure_main_window(
    runtime: &Rc<RefCell<MainWindowRuntime>>,
    app: &mut App,
) -> Result<WindowHandle<MainView>, String> {
    let cached_main_window = runtime.borrow().main_window;
    if let Some(main_window) = cached_main_window {
        if activate_main_window(main_window, app) {
            runtime.borrow_mut().remember_window(main_window, app);
            return Ok(main_window);
        }
        runtime.borrow_mut().clear_window();
    }

    if let Some(main_window) = find_open_main_window(app)
        && activate_main_window(main_window, app)
    {
        runtime.borrow_mut().remember_window(main_window, app);
        return Ok(main_window);
    }

    create_main_window(runtime, app)
}

/// 在主窗口中打开一组系统传入路径。
///
/// 业务意图：
/// - 启动参数、macOS open-url 和延迟处理的 pending URL 都应走同一条入口，保证系统右键、拖拽和命令行启动行为一致。
/// - `.hprof/.bin` 进入 HPROF 解析页，其余文件、目录和压缩包继续进入日志分析页。
///
/// 边界条件：
/// - 空路径集合不触发加载或页面切换，避免覆盖用户当前工作区。
/// - HPROF 页当前一次只解析一个 dump，分流层只会把首个 HPROF 候选放入计划。
/// - 如果窗口在平台回调和加载之间被关闭，`update` 失败即可忽略，避免平台回调引发 panic。
fn open_paths_in_main_window(
    main_window: WindowHandle<MainView>,
    app: &mut App,
    paths: Vec<PathBuf>,
    message: &str,
) {
    let plan = classify_launch_paths(paths);
    if plan.log_paths.is_empty() && plan.hprof_path.is_none() {
        return;
    }

    let message = message.to_string();
    main_window
        .update(app, |view, window, context| {
            if !plan.log_paths.is_empty() {
                view.navigation.active_main_feature = MainFeature::LogAnalysis;
                view.start_log_source_load(plan.log_paths, message, context);
            }
            if let Some(hprof_path) = plan.hprof_path {
                view.open_hprof_analysis_page(hprof_path, context);
            }
            window.activate_window();
        })
        .ok();
}

/// 程序入口。
///
/// 业务意图：
/// - 初始化 GPUI 应用并创建唯一主窗口。
/// - macOS 关闭所有窗口后进程仍保留，因此额外注册 reopen 处理，在 Dock 再点时恢复主窗口。
///
/// 错误处理：
/// - 启动期主窗口创建失败意味着桌面应用无法进入可交互状态，属于不可恢复错误。
/// - macOS reopen 或 open-url 回调中的窗口恢复失败只能写入 stderr，因为此时可能没有任何可展示错误的窗口。
pub(crate) fn run() {
    let application = Application::new();
    let main_window_runtime: Rc<RefCell<MainWindowRuntime>> =
        Rc::new(RefCell::new(MainWindowRuntime::default()));
    let pending_open_urls: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let main_window_runtime = Rc::clone(&main_window_runtime);
        let pending_open_urls = Rc::clone(&pending_open_urls);
        application.on_open_urls(move |urls| {
            let paths = log_source_paths_from_open_urls(urls.clone());
            if paths.is_empty() {
                return;
            }

            let async_app = main_window_runtime.borrow().async_app();
            let Some(async_app) = async_app else {
                // 某些平台可能在主窗口创建前触发 open-url；先暂存，主窗口完成初始化后再统一处理。
                pending_open_urls.borrow_mut().extend(urls);
                return;
            };

            let main_window_runtime = Rc::clone(&main_window_runtime);
            async_app
                .update(move |app| {
                    main_window_runtime.borrow_mut().remember_app(app);
                    match ensure_main_window(&main_window_runtime, app) {
                        Ok(main_window) => {
                            open_paths_in_main_window(main_window, app, paths, "正在加载拖入的日志")
                        }
                        Err(error) => {
                            eprintln!("macOS open-url 恢复 LogClinic 主窗口失败：{error}");
                        }
                    }
                })
                .ok();
        });
    }
    {
        let main_window_runtime = Rc::clone(&main_window_runtime);
        application.on_reopen(move |app| {
            main_window_runtime.borrow_mut().remember_app(app);
            match ensure_main_window(&main_window_runtime, app) {
                Ok(_main_window) => {
                    // macOS Dock 再次点按应用图标时，应用可能处于后台；显式激活保证恢复出的主窗口可见。
                    app.activate(true);
                }
                Err(error) => {
                    eprintln!("macOS reopen 恢复 LogClinic 主窗口失败：{error}");
                }
            }
        });
    }

    application.run(move |app| {
        // 清理异常退出遗留的超大日志物化目录，避免压缩包大成员长期占用系统临时磁盘。
        cleanup_stale_large_log_cache();
        // 启动参数在 Windows 拖拽到程序图标、开发期命令行启动等场景中承载待打开路径。
        // macOS Dock/Finder 的“用应用打开”通常走下方 `on_open_urls` 回调，因此两条入口都保留。
        let launch_paths = log_source_paths_from_launch_arguments();
        app.bind_keys([
            KeyBinding::new("ctrl-f", OpenSearchDialog, None),
            KeyBinding::new("cmd-f", OpenSearchDialog, None),
        ]);
        // 注册 Lucide 图标字体和内置 JetBrains Mono 正文字体，确保 macOS 和 Windows 上的图标、日志等宽字体
        // 与笔记富文本斜体不依赖运行环境预装字体；如果注册失败，核心界面视觉无法可靠渲染，启动期应直接暴露错误。
        app.text_system()
            .add_fonts(vec![
                Cow::Borrowed(LUCIDE_FONT_BYTES),
                Cow::Borrowed(JETBRAINS_MONO_REGULAR_FONT_BYTES),
                Cow::Borrowed(JETBRAINS_MONO_ITALIC_FONT_BYTES),
            ])
            .expect("注册内置字体失败，工具栏图标或日志正文等宽字体无法可靠渲染");
        main_window_runtime.borrow_mut().remember_app(app);

        let main_window = ensure_main_window(&main_window_runtime, app)
            .expect("创建 LogClinic 主窗口失败，应用无法继续启动");
        open_paths_in_main_window(main_window, app, launch_paths, "正在加载启动传入的日志");

        let pending_paths = log_source_paths_from_open_urls(pending_open_urls.take());
        if !pending_paths.is_empty() {
            open_paths_in_main_window(main_window, app, pending_paths, "正在加载拖入的日志");
        }
    });
}
