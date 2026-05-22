// 应用层纯状态测试。
//
// 业务意图：
// - 该文件从历史 `app.rs` 底部拆出，保持测试仍在 `app` 模块作用域内，避免第一阶段重构改变私有辅助函数测试方式。
// - 后续当功能域升级为真正子模块时，再把测试逐步迁移到对应模块内。

use super::*;

// 该测试文件作为 `app::tests` 模块加载，内部模块使用语义化名称承载历史纯状态测试。
#[cfg(test)]
mod state_tests {
    //! 主界面纯状态逻辑测试。
    //!
    //! 业务意图：
    //! - GPUI 渲染交互主要依赖手动验收，但目录树状态这类纯数据规则可以通过单元测试锁定。
    //! - 本模块只验证不需要窗口系统的行为，避免测试环境依赖 macOS 或 Windows 图形能力。

    use crate::archive::{ArchiveFormat, MaterializedLogSource};

    use super::*;

    /// 构造测试用窗口尺寸。
    ///
    /// 业务意图：
    /// - 测试用例只关心合法尺寸是否被策略识别，集中构造可以避免重复 unwrap 逻辑分散在各个断言里。
    fn test_window_size(width: f32, height: f32) -> MainWindowSizePreference {
        MainWindowSizePreference::new(width, height).expect("测试尺寸应为合法正数")
    }

    /// 构造唯一的测试临时文件路径。
    ///
    /// 边界条件：
    /// - 测试只验证读写函数本身，不依赖 macOS 或 Windows 的真实配置目录。
    /// - 路径包含进程 ID，避免并行测试或重复运行时互相覆盖。
    fn test_window_size_file_path(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "logclinic3-window-size-test-{}-{}",
            std::process::id(),
            name
        ))
    }

    /// 构造唯一的主题配置测试路径。
    ///
    /// 业务意图：
    /// - 主题配置和窗口尺寸配置共用“简单文本文件”策略，但测试文件名分开，避免读写往返互相污染。
    fn test_theme_preference_file_path(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "logclinic3-theme-test-{}-{}",
            std::process::id(),
            name
        ))
    }

    /// 构造唯一的日志字号配置测试路径。
    ///
    /// 业务意图：
    /// - 日志字号配置和主题配置一样写入应用配置目录；测试使用独立临时路径，避免污染开发机真实偏好。
    fn test_log_font_size_file_path(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "logclinic3-log-font-size-test-{}-{}",
            std::process::id(),
            name
        ))
    }

    /// 构造唯一的日志 minimap 开关配置测试路径。
    ///
    /// 业务意图：
    /// - minimap 开关会跨会话保存性能偏好；测试使用独立临时路径，避免污染开发机真实配置。
    fn test_log_minimap_enabled_file_path(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "logclinic3-log-minimap-enabled-test-{}-{}",
            std::process::id(),
            name
        ))
    }

    /// 构造唯一的线程分析过滤配置测试路径。
    ///
    /// 业务意图：
    /// - 线程分析过滤配置会保存多行堆栈文本，测试必须使用独立临时目录，避免污染开发机真实设置。
    fn test_thread_analysis_filter_file_path(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "logclinic3-thread-filter-test-{}-{}",
            std::process::id(),
            name
        ))
    }

    /// 构造唯一的快搜关键字配置测试路径。
    ///
    /// 业务意图：
    /// - 快搜关键字配置会跨会话保存常用排障词，测试必须使用独立临时目录，避免污染开发机真实设置。
    fn test_quick_search_keywords_file_path(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "logclinic3-quick-search-test-{}-{}",
            std::process::id(),
            name
        ))
    }

    /// 构造唯一的模型配置测试路径。
    ///
    /// 业务意图：
    /// - 模型配置会保存 API Key 等敏感字段，测试必须使用临时目录，避免污染开发机真实应用配置。
    fn test_model_configs_file_path(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "logclinic3-model-configs-test-{}-{}",
            std::process::id(),
            name
        ))
    }

    /// 构造唯一的另存为测试目录。
    ///
    /// 业务意图：
    /// - 另存为测试会真实创建源文件和目标文件，必须隔离到临时目录并带进程 ID，避免覆盖开发机文件。
    fn test_save_as_directory(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "logclinic3-save-as-test-{}-{}",
            std::process::id(),
            name
        ))
    }

    /// 验证小屏首次启动会使用最大化窗口。
    ///
    /// 业务意图：
    /// - 用户明确要求主显示器逻辑宽度 `< 1440px` 时默认最大化，1439 是阈值下方的边界样本。
    #[test]
    fn 小屏无历史尺寸时默认最大化() {
        let decision = decide_main_window_startup(None, Some(1439.0));

        assert_eq!(decision, MainWindowStartupDecision::DefaultMaximized);
    }

    /// 验证 1440px 及以上不再按小屏处理。
    ///
    /// 业务意图：
    /// - 阈值规则是严格小于 1440，等于 1440 的屏幕应走固定 1600x900 居中窗口策略。
    #[test]
    fn 屏幕宽度等于阈值时默认窗口化() {
        let decision = decide_main_window_startup(None, Some(1440.0));

        assert_eq!(decision, MainWindowStartupDecision::DefaultWindowed);
    }

    /// 验证大屏下旧版最大化历史尺寸会被丢弃。
    ///
    /// 业务意图：
    /// - 旧版本可能在最大化关闭时写入接近显示器宽度的历史尺寸；大屏重新启动时应回到 1600x900，而不是继续像最大化。
    #[test]
    fn 大屏历史最大化尺寸回退默认窗口化() {
        let saved_size = test_window_size(2048.0, 1110.0);
        let decision = decide_main_window_startup(Some(saved_size), Some(2048.0));

        assert_eq!(decision, MainWindowStartupDecision::DefaultWindowed);
    }

    /// 验证大屏下普通历史尺寸仍会恢复。
    ///
    /// 业务意图：
    /// - 修复最大化残留不能破坏用户手动调整普通窗口大小的行为；明显小于显示器宽度的历史值仍应优先使用。
    #[test]
    fn 大屏普通历史尺寸仍会恢复() {
        let saved_size = test_window_size(1500.0, 820.0);
        let decision = decide_main_window_startup(Some(saved_size), Some(2048.0));

        assert_eq!(decision, MainWindowStartupDecision::Remembered(saved_size));
    }

    /// 验证默认窗口尺寸不会超过当前显示器宽度。
    ///
    /// 业务意图：
    /// - 部分设备逻辑宽度大于小屏阈值 1440px，但仍小于默认窗口宽度 1600px；这时如果不裁剪宽度，
    ///   系统会把超出屏幕的窗口挤回可见区域，造成启动位置看起来偏右。
    #[test]
    fn 中等屏默认窗口尺寸会先裁剪再居中() {
        let requested_size = test_window_size(1600.0, 900.0);
        let fitted = fit_main_window_size_to_display(requested_size, Some((1512.0, 982.0)));

        assert_eq!(fitted, test_window_size(1512.0, 900.0));
    }

    /// 验证历史窗口尺寸过大时也会被限制在显示器内。
    ///
    /// 业务意图：
    /// - 用户可能从外接大屏切回笔记本屏幕；历史尺寸仍应被尊重为“用户偏好”，但不能大到影响初始化居中。
    #[test]
    fn 历史窗口尺寸超过显示器时会裁剪() {
        let saved_size = test_window_size(1800.0, 1100.0);
        let fitted = fit_main_window_size_to_display(saved_size, Some((1512.0, 982.0)));

        assert_eq!(fitted, test_window_size(1512.0, 982.0));
    }

    /// 验证显示器尺寸不可用时保留原始窗口尺寸。
    ///
    /// 边界条件：
    /// - 图形环境初始化早期可能无法读取显示器信息；这时不应凭空改写启动尺寸，避免引入新的平台差异。
    #[test]
    fn 显示器尺寸不可用时保留请求尺寸() {
        let requested_size = test_window_size(1600.0, 900.0);

        assert_eq!(
            fit_main_window_size_to_display(requested_size, None),
            requested_size
        );
        assert_eq!(
            fit_main_window_size_to_display(requested_size, Some((0.0, 982.0))),
            requested_size
        );
    }

    /// 验证用户历史宽高优先于小屏最大化规则。
    ///
    /// 业务意图：
    /// - 一旦用户手动调整过窗口，下次启动应尊重用户选择，而不是继续套用首次启动的小屏默认行为。
    #[test]
    fn 历史尺寸优先于小屏默认最大化() {
        let saved_size = test_window_size(1200.0, 700.0);
        let decision = decide_main_window_startup(Some(saved_size), Some(1024.0));

        assert_eq!(decision, MainWindowStartupDecision::Remembered(saved_size));
    }

    /// 验证非法历史尺寸不会被接受。
    ///
    /// 边界条件：
    /// - 配置文件可能被用户手工修改或写入中断破坏；零、负数、NaN 和无穷大都必须回退默认策略。
    #[test]
    fn 非法历史尺寸会回退默认策略() {
        assert_eq!(MainWindowSizePreference::new(0.0, 900.0), None);
        assert_eq!(MainWindowSizePreference::new(1600.0, -1.0), None);
        assert_eq!(MainWindowSizePreference::new(f32::NAN, 900.0), None);
        assert_eq!(MainWindowSizePreference::new(1600.0, f32::INFINITY), None);

        assert_eq!(
            parse_main_window_size_preference("not-a-size"),
            None,
            "非数字配置应被丢弃"
        );
        assert_eq!(
            parse_main_window_size_preference("1600 0"),
            None,
            "零高度配置应被丢弃"
        );
        assert_eq!(
            parse_main_window_size_preference("1600 900 extra"),
            None,
            "多余字段表示格式不符合约定，应被丢弃"
        );
    }

    /// 验证支持混选的平台会同时允许选择文件和目录。
    ///
    /// 业务意图：
    /// - macOS 选择器支持文件和目录混选，加载入口应保留一次选择多个日志文件、压缩包和目录的工作流。
    #[test]
    fn 加载日志选择器在支持混选平台允许文件和目录() {
        let options = LoadPromptKind::Sources.to_prompt_options(true);

        assert!(options.files, "支持混选时应允许选择普通日志和压缩包文件");
        assert!(options.directories, "支持混选时应继续允许选择目录");
        assert!(options.multiple, "加载日志应允许一次选择多个来源");
    }

    /// 验证不支持混选的平台优先展示文件。
    ///
    /// 业务意图：
    /// - Windows 原生文件选择器的 `FOS_PICKFOLDERS` 会切换成只选目录模式；如果仍传 `directories=true`，
    ///   用户点击“加载日志”时就看不到 ZIP/RAR/7Z/TAR.GZ/GZ 等压缩包文件。
    #[test]
    fn 加载日志选择器在不支持混选平台优先显示文件() {
        let options = LoadPromptKind::Sources.to_prompt_options(false);

        assert!(options.files, "Windows 必须能看到普通日志和压缩包文件");
        assert!(!options.directories, "不支持混选时不能进入只选目录模式");
        assert!(options.multiple, "文件选择模式仍应允许一次选择多个来源");
    }

    /// 验证文件/压缩包菜单项只打开文件选择器。
    ///
    /// 业务意图：
    /// - Windows 菜单中的“文件/压缩包”必须让系统对话框展示 ZIP/RAR/7Z/TAR.GZ/GZ 等文件，不能再次落入只选目录模式。
    #[test]
    fn 加载日志文件压缩包菜单项只允许选择文件() {
        let options = LoadPromptKind::FilesOrArchives.to_prompt_options(false);

        assert!(options.files, "文件/压缩包入口应展示文件");
        assert!(!options.directories, "文件/压缩包入口不能切换到目录模式");
        assert!(options.multiple, "文件/压缩包入口应允许多选");
    }

    /// 验证目录菜单项只打开目录选择器。
    ///
    /// 业务意图：
    /// - Windows 菜单中的“目录”必须保留加载整个目录树的能力，避免修复压缩包可见性时丢掉目录加载入口。
    #[test]
    fn 加载日志目录菜单项只允许选择目录() {
        let options = LoadPromptKind::Directories.to_prompt_options(false);

        assert!(!options.files, "目录入口不应展示普通文件");
        assert!(options.directories, "目录入口应打开目录选择模式");
        assert!(options.multiple, "目录入口应允许一次选择多个目录");
    }

    /// 验证合法配置文本可以解析为窗口宽高。
    ///
    /// 业务意图：
    /// - 配置文件采用简单空白分隔格式，读取时需要兼容结尾换行。
    #[test]
    fn 合法窗口尺寸配置可以解析() {
        assert_eq!(
            parse_main_window_size_preference("1600 900\n"),
            Some(test_window_size(1600.0, 900.0))
        );
    }

    /// 验证窗口尺寸配置文件可以完成写入和读取往返。
    ///
    /// 边界条件：
    /// - 写入函数需要自动创建父目录，避免首次启动关闭时配置目录不存在导致保存失败。
    #[test]
    fn 窗口尺寸配置可以读写往返() {
        let path = test_window_size_file_path("roundtrip").join(MAIN_WINDOW_SIZE_FILE_NAME);
        let size = test_window_size(1234.4, 678.6);

        write_main_window_size_preference(&path, size).expect("测试配置应能写入临时目录");
        let restored = read_main_window_size_preference(&path).expect("刚写入的配置应能读取");

        assert_eq!(restored, test_window_size(1234.0, 679.0));

        let _ = fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    /// 验证缺失或损坏的配置文件不会阻断默认启动策略。
    ///
    /// 业务意图：
    /// - 窗口大小记忆只是体验优化，文件不存在或损坏时应视为无历史尺寸，而不是影响应用启动。
    #[test]
    fn 缺失和损坏的窗口尺寸配置会被忽略() {
        let missing_path = test_window_size_file_path("missing").join(MAIN_WINDOW_SIZE_FILE_NAME);
        assert_eq!(read_main_window_size_preference(&missing_path), None);

        let broken_path = test_window_size_file_path("broken").join(MAIN_WINDOW_SIZE_FILE_NAME);
        fs::create_dir_all(broken_path.parent().expect("测试路径应包含父目录"))
            .expect("测试目录应能创建");
        fs::write(&broken_path, "broken size").expect("测试损坏配置应能写入");

        assert_eq!(read_main_window_size_preference(&broken_path), None);

        let _ = fs::remove_file(&broken_path);
        if let Some(parent) = broken_path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    /// 验证主窗口大功能导航的名称和图标稳定。
    ///
    /// 业务意图：
    /// - 左侧大导航只显示图标，hover 气泡依赖 `label()` 返回中文名称；图标和名称错配会直接影响用户识别功能入口。
    #[test]
    fn 主功能导航名称和图标稳定() {
        assert_eq!(MainFeature::all().len(), 6);
        assert_eq!(MainFeature::LogAnalysis.label(), "日志分析");
        assert_eq!(
            char::from(MainFeature::LogAnalysis.icon()),
            char::from(Icon::Search)
        );
        assert_eq!(MainFeature::Notes.label(), "笔记");
        assert_eq!(
            char::from(MainFeature::Notes.icon()),
            char::from(Icon::NotebookText)
        );
        assert_eq!(MainFeature::Terminal.label(), "终端");
        assert_eq!(
            char::from(MainFeature::Terminal.icon()),
            char::from(Icon::SquareTerminal)
        );
        assert_eq!(MainFeature::Connections.label(), "连接");
        assert_eq!(
            char::from(MainFeature::Connections.icon()),
            char::from(Icon::Cable)
        );
        assert_eq!(MainFeature::HprofAnalysis.label(), "HPROF解析");
        assert_eq!(
            char::from(MainFeature::HprofAnalysis.icon()),
            char::from(Icon::ChartNoAxesCombined)
        );
        assert_eq!(MainFeature::AiChat.label(), "AI对话");
        assert_eq!(
            char::from(MainFeature::AiChat.icon()),
            char::from(Icon::BotMessageSquare)
        );
        assert_eq!(MainNavigationItem::Settings.label(), "设置");
        assert_eq!(
            char::from(MainNavigationItem::Settings.icon()),
            char::from(Icon::Settings)
        );
    }

    /// 验证连接行右键菜单坐标会限制在左侧栏内部。
    ///
    /// 业务意图：
    /// - 连接行右键菜单渲染在连接侧栏内，而鼠标事件是主窗口坐标；这里锁定主导航扣减和右边界夹紧，
    ///   避免菜单覆盖右侧终端或在靠近左边缘时出现负坐标。
    /// - 纵向坐标也必须按菜单高度夹紧，避免窗口底部右键时“删除”等底部菜单项被裁掉。
    #[test]
    fn 连接行右键菜单坐标会限制在侧栏内() {
        assert_eq!(
            MainView::connection_profile_context_menu_x(
                MAIN_NAV_WIDTH + 24.0,
                CONNECTIONS_TREE_DEFAULT_WIDTH
            ),
            24.0
        );
        assert_eq!(
            MainView::connection_profile_context_menu_x(8.0, CONNECTIONS_TREE_DEFAULT_WIDTH),
            0.0
        );
        assert_eq!(
            MainView::connection_profile_context_menu_x(
                MAIN_NAV_WIDTH + CONNECTIONS_TREE_DEFAULT_WIDTH + 40.0,
                CONNECTIONS_TREE_DEFAULT_WIDTH
            ),
            CONNECTIONS_TREE_DEFAULT_WIDTH - CONNECTIONS_CONTEXT_MENU_WIDTH
        );

        assert_eq!(
            MainView::connection_profile_context_menu_y(24.0, 480.0),
            24.0
        );
        assert_eq!(
            MainView::connection_profile_context_menu_y(-12.0, 480.0),
            0.0
        );
        assert_eq!(
            MainView::connection_profile_context_menu_y(460.0, 480.0),
            480.0
                - CONNECTIONS_CONTEXT_MENU_ITEM_HEIGHT * 3.0
                - CONNECTIONS_CONTEXT_MENU_VERTICAL_PADDING
        );
    }

    /// 验证设置窗口固定页签包含插件、存储和关于入口。
    ///
    /// 业务意图：
    /// - 插件管理、存储管理和关于功能都收入口设置窗口，页签顺序和图标必须稳定，避免用户找不到本地数据位置和关于信息。
    /// - 插件声明式设置页签只能由已启用插件贡献，不能出现在宿主固定页签列表中，否则未加载插件也会展示插件专属入口。
    #[test]
    fn 设置固定页签不包含插件声明式设置入口() {
        assert_eq!(
            SettingsTab::all(),
            &[
                SettingsTab::General,
                SettingsTab::Log,
                SettingsTab::Model,
                SettingsTab::Plugin,
                SettingsTab::Storage,
                SettingsTab::About
            ]
        );
        assert!(!SettingsTab::all().contains(&SettingsTab::PluginSettings));
        assert_eq!(SettingsTab::Plugin.label(), "插件");
        assert_eq!(
            char::from(SettingsTab::Plugin.icon()),
            char::from(Icon::FileArchive)
        );
        assert_eq!(SettingsTab::Storage.label(), "存储");
        assert_eq!(
            char::from(SettingsTab::Storage.icon()),
            char::from(Icon::Database)
        );
        assert_eq!(SettingsTab::About.label(), "关于");
        assert_eq!(
            char::from(SettingsTab::About.icon()),
            char::from(Icon::Info)
        );
    }

    #[test]
    fn 插件工具栏日志树快照输出相对路径和压缩包链路() {
        let tree = LoadedLogTree {
            summary: "测试树".to_string(),
            error_count: 0,
            temporary_paths: Vec::new(),
            rows: vec![
                LoadedLogTreeRow {
                    id: 1,
                    depth: 0,
                    label: "root".to_string(),
                    kind: LogTreeEntryKind::Directory,
                    has_children: true,
                    meta: None,
                    error_message: None,
                    source: None,
                },
                LoadedLogTreeRow {
                    id: 2,
                    depth: 1,
                    label: "memory_2026-05-23.log".to_string(),
                    kind: LogTreeEntryKind::File,
                    has_children: false,
                    meta: None,
                    error_message: None,
                    source: Some(LogFileSource::LocalFile {
                        path: PathBuf::from("/tmp/root/memory_2026-05-23.log"),
                    }),
                },
                LoadedLogTreeRow {
                    id: 3,
                    depth: 1,
                    label: "logs.zip".to_string(),
                    kind: LogTreeEntryKind::Archive,
                    has_children: true,
                    meta: None,
                    error_message: None,
                    source: None,
                },
                LoadedLogTreeRow {
                    id: 4,
                    depth: 2,
                    label: "inner".to_string(),
                    kind: LogTreeEntryKind::Directory,
                    has_children: true,
                    meta: None,
                    error_message: None,
                    source: None,
                },
                LoadedLogTreeRow {
                    id: 5,
                    depth: 3,
                    label: "ecology_20260523.log".to_string(),
                    kind: LogTreeEntryKind::File,
                    has_children: false,
                    meta: None,
                    error_message: None,
                    source: Some(LogFileSource::ArchiveMember {
                        archive_path: PathBuf::from("/tmp/root/logs.zip"),
                        archive_format: ArchiveFormat::Zip,
                        member_path: "inner/ecology_20260523.log".to_string(),
                    }),
                },
            ],
        };
        let snapshot = LoadedLogTreeState::new(tree).plugin_log_files_for_toolbar_snapshot();
        let paths = snapshot
            .iter()
            .map(|file| file.path_label.as_str())
            .collect::<Vec<_>>();

        assert!(paths.contains(&"root/memory_2026-05-23.log"));
        assert!(paths.contains(&"root/logs.zip!/inner/ecology_20260523.log"));
    }

    /// 验证插件工具栏快照会继续展开单文件内层压缩包。
    ///
    /// 业务意图：
    /// - 左侧树为了单文件压缩包可直接打开，会把只包含一个文件的内层 ZIP 当成文件展示。
    /// - 泛微日志分析需要遍历所有嵌套压缩包路径，因此工具栏快照必须额外展开这类 ZIP，直到看到真正的日志文件名。
    #[test]
    fn 插件工具栏日志树快照展开单文件嵌套压缩包() {
        use std::io::{Cursor, Write};
        use zip::write::SimpleFileOptions;

        let temp_root = test_save_as_directory("plugin-nested-archive-snapshot");
        let _ = fs::remove_dir_all(&temp_root);
        fs::create_dir_all(&temp_root).expect("应能创建测试临时目录");
        let outer_path = temp_root.join("outer.zip");

        let mut inner_bytes = Cursor::new(Vec::new());
        {
            let mut inner_writer = zip::ZipWriter::new(&mut inner_bytes);
            inner_writer
                .start_file("memory_2026-05-23.log", SimpleFileOptions::default())
                .expect("应能创建内层日志条目");
            inner_writer
                .write_all(b"INFO memory")
                .expect("应能写入内层日志内容");
            inner_writer.finish().expect("应能结束最内层 ZIP");
        }

        let mut middle_bytes = Cursor::new(Vec::new());
        {
            let mut middle_writer = zip::ZipWriter::new(&mut middle_bytes);
            middle_writer
                .start_file("inner.zip", SimpleFileOptions::default())
                .expect("应能创建中间 ZIP 条目");
            middle_writer
                .write_all(inner_bytes.get_ref())
                .expect("应能写入中间 ZIP 内容");
            middle_writer.finish().expect("应能结束中间 ZIP");
        }

        {
            let outer_file = fs::File::create(&outer_path).expect("应能创建外层 ZIP 文件");
            let mut outer_writer = zip::ZipWriter::new(outer_file);
            outer_writer
                .start_file("middle.zip", SimpleFileOptions::default())
                .expect("应能创建外层 ZIP 条目");
            outer_writer
                .write_all(middle_bytes.get_ref())
                .expect("应能写入外层 ZIP 内容");
            outer_writer.finish().expect("应能结束外层 ZIP");
        }

        let tree = LoadedLogTree {
            summary: "测试树".to_string(),
            error_count: 0,
            temporary_paths: Vec::new(),
            rows: vec![
                LoadedLogTreeRow {
                    id: 1,
                    depth: 0,
                    label: "root".to_string(),
                    kind: LogTreeEntryKind::Directory,
                    has_children: true,
                    meta: None,
                    error_message: None,
                    source: None,
                },
                LoadedLogTreeRow {
                    id: 2,
                    depth: 1,
                    label: "outer.zip".to_string(),
                    kind: LogTreeEntryKind::File,
                    has_children: false,
                    meta: None,
                    error_message: None,
                    source: Some(LogFileSource::LocalFile {
                        path: outer_path.clone(),
                    }),
                },
            ],
        };

        let snapshot = LoadedLogTreeState::new(tree).plugin_log_files_for_toolbar_snapshot();
        let paths = snapshot
            .iter()
            .map(|file| file.path_label.as_str())
            .collect::<Vec<_>>();

        assert!(paths.contains(&"root/outer.zip!/middle.zip!/inner.zip!/memory_2026-05-23.log"));

        fs::remove_dir_all(temp_root).expect("应能清理测试临时目录");
    }

    /// 验证顶层压缩包内部的标准线程 ZIP 会作为终端日志保留。
    ///
    /// 业务意图：
    /// - 现场线程日志常见形态是 `downLog.zip!/monitorThread/yyyyMMdd/thread_HHmmss.zip`。
    /// - 这种节点数量通常很大，路径本身已经能按泛微线程日志规则命中；工具栏快照不应再逐个解压内部 `.log`，否则会拖慢扫描。
    #[test]
    fn 插件工具栏日志树快照保留线程_zip_但不逐个展开() {
        use std::io::{Cursor, Write};
        use zip::write::SimpleFileOptions;

        let temp_root = test_save_as_directory("plugin-archive-member-thread-zip-snapshot");
        let _ = fs::remove_dir_all(&temp_root);
        fs::create_dir_all(&temp_root).expect("应能创建测试临时目录");
        let outer_path = temp_root.join("downLog.zip");
        let thread_zip_member = "2026-05-21/monitorThread/20260521/thread_000038.zip";

        let mut thread_zip_bytes = Cursor::new(Vec::new());
        {
            let mut thread_zip_writer = zip::ZipWriter::new(&mut thread_zip_bytes);
            thread_zip_writer
                .start_file("thread_000038.log", SimpleFileOptions::default())
                .expect("应能创建线程日志条目");
            thread_zip_writer
                .write_all(b"thread dump")
                .expect("应能写入线程日志内容");
            thread_zip_writer.finish().expect("应能结束线程 ZIP");
        }

        {
            let outer_file = fs::File::create(&outer_path).expect("应能创建外层 ZIP 文件");
            let mut outer_writer = zip::ZipWriter::new(outer_file);
            outer_writer
                .start_file(thread_zip_member, SimpleFileOptions::default())
                .expect("应能创建外层线程 ZIP 条目");
            outer_writer
                .write_all(thread_zip_bytes.get_ref())
                .expect("应能写入外层线程 ZIP 内容");
            outer_writer.finish().expect("应能结束外层 ZIP");
        }

        let tree = LoadedLogTree {
            summary: "测试树".to_string(),
            error_count: 0,
            temporary_paths: Vec::new(),
            rows: vec![
                LoadedLogTreeRow {
                    id: 1,
                    depth: 0,
                    label: "downLog.zip".to_string(),
                    kind: LogTreeEntryKind::Archive,
                    has_children: true,
                    meta: None,
                    error_message: None,
                    source: None,
                },
                LoadedLogTreeRow {
                    id: 2,
                    depth: 1,
                    label: "2026-05-21".to_string(),
                    kind: LogTreeEntryKind::Directory,
                    has_children: true,
                    meta: None,
                    error_message: None,
                    source: None,
                },
                LoadedLogTreeRow {
                    id: 3,
                    depth: 2,
                    label: "monitorThread".to_string(),
                    kind: LogTreeEntryKind::Directory,
                    has_children: true,
                    meta: None,
                    error_message: None,
                    source: None,
                },
                LoadedLogTreeRow {
                    id: 4,
                    depth: 3,
                    label: "20260521".to_string(),
                    kind: LogTreeEntryKind::Directory,
                    has_children: true,
                    meta: None,
                    error_message: None,
                    source: None,
                },
                LoadedLogTreeRow {
                    id: 5,
                    depth: 4,
                    label: "thread_000038.zip".to_string(),
                    kind: LogTreeEntryKind::File,
                    has_children: false,
                    meta: Some("38 KB".to_string()),
                    error_message: None,
                    source: Some(LogFileSource::ArchiveMember {
                        archive_path: outer_path.clone(),
                        archive_format: ArchiveFormat::Zip,
                        member_path: thread_zip_member.to_string(),
                    }),
                },
            ],
        };

        let snapshot = LoadedLogTreeState::new(tree).plugin_log_files_for_toolbar_snapshot();
        let paths = snapshot
            .iter()
            .map(|file| file.path_label.as_str())
            .collect::<Vec<_>>();

        assert!(
            paths.contains(&"downLog.zip!/2026-05-21/monitorThread/20260521/thread_000038.zip")
        );
        assert!(!paths.contains(
            &"downLog.zip!/2026-05-21/monitorThread/20260521/thread_000038.zip!/thread_000038.log"
        ));

        fs::remove_dir_all(temp_root).expect("应能清理测试临时目录");
    }

    /// 验证主窗口默认进入日志分析功能页。
    ///
    /// 业务意图：
    /// - 新增大导航后不能改变既有启动体验；用户打开应用后仍应先看到日志加载和分析工作区。
    #[test]
    fn 主窗口默认功能是日志分析() {
        assert_eq!(MainFeature::default(), MainFeature::LogAnalysis);
    }

    /// 验证主导航 hover 气泡使用根层覆盖坐标。
    ///
    /// 业务意图：
    /// - 气泡不再作为按钮子元素渲染，否则会被右侧功能页遮挡；顶部功能和底部入口必须分别保持稳定锚点。
    #[test]
    fn 主导航气泡锚点稳定() {
        assert_eq!(
            MainNavigationItem::Feature(MainFeature::LogAnalysis).tooltip_anchor(),
            MainNavigationTooltipAnchor::Top(MAIN_NAV_PADDING + MAIN_NAV_TOOLTIP_BUTTON_INSET)
        );
        assert_eq!(
            MainNavigationItem::Feature(MainFeature::Notes).tooltip_anchor(),
            MainNavigationTooltipAnchor::Top(
                MAIN_NAV_PADDING
                    + MAIN_NAV_BUTTON_SIZE
                    + MAIN_NAV_BUTTON_GAP
                    + MAIN_NAV_TOOLTIP_BUTTON_INSET
            )
        );
        assert_eq!(
            MainNavigationItem::Feature(MainFeature::Terminal).tooltip_anchor(),
            MainNavigationTooltipAnchor::Top(
                MAIN_NAV_PADDING
                    + (MAIN_NAV_BUTTON_SIZE + MAIN_NAV_BUTTON_GAP) * 2.0
                    + MAIN_NAV_TOOLTIP_BUTTON_INSET
            )
        );
        assert_eq!(
            MainNavigationItem::Feature(MainFeature::Connections).tooltip_anchor(),
            MainNavigationTooltipAnchor::Top(
                MAIN_NAV_PADDING
                    + (MAIN_NAV_BUTTON_SIZE + MAIN_NAV_BUTTON_GAP) * 3.0
                    + MAIN_NAV_TOOLTIP_BUTTON_INSET
            )
        );
        assert_eq!(
            MainNavigationItem::Feature(MainFeature::HprofAnalysis).tooltip_anchor(),
            MainNavigationTooltipAnchor::Top(
                MAIN_NAV_PADDING
                    + (MAIN_NAV_BUTTON_SIZE + MAIN_NAV_BUTTON_GAP) * 4.0
                    + MAIN_NAV_TOOLTIP_BUTTON_INSET
            )
        );
        assert_eq!(
            MainNavigationItem::Feature(MainFeature::AiChat).tooltip_anchor(),
            MainNavigationTooltipAnchor::Top(
                MAIN_NAV_PADDING
                    + (MAIN_NAV_BUTTON_SIZE + MAIN_NAV_BUTTON_GAP) * 5.0
                    + MAIN_NAV_TOOLTIP_BUTTON_INSET
            )
        );
        assert_eq!(
            MainNavigationItem::Settings.tooltip_anchor(),
            MainNavigationTooltipAnchor::Bottom(MAIN_NAV_PADDING + MAIN_NAV_TOOLTIP_BUTTON_INSET)
        );
    }

    /// 验证日志页右侧弹层坐标会扣除固定大导航宽度。
    ///
    /// 业务意图：
    /// - tab 菜单、编码下拉、日志正文菜单和搜索结果菜单都依赖右侧面板局部坐标；新增 56px 导航后必须整体右移补偿。
    #[test]
    fn 日志右侧面板偏移包含主导航宽度() {
        assert_eq!(
            MainView::right_panel_left_offset_for_layout(true, LEFT_PANEL_DEFAULT_WIDTH),
            MAIN_NAV_WIDTH
        );
        assert_eq!(
            MainView::right_panel_left_offset_for_layout(false, 300.0),
            MAIN_NAV_WIDTH + 300.0 + SPLITTER_VISIBLE_WIDTH
        );
    }

    /// 验证日志目录树右键菜单坐标会扣除固定大导航宽度。
    ///
    /// 边界条件：
    /// - 鼠标在导航区或目录树左缘附近右键时，菜单横坐标不能变成负数；靠近目录树右边缘时仍要限制在面板内。
    #[test]
    fn 日志目录树菜单横坐标包含主导航宽度() {
        assert_eq!(MainView::log_tree_context_menu_x(24.0, 300.0), 0.0);
        assert_eq!(
            MainView::log_tree_context_menu_x(MAIN_NAV_WIDTH + 80.0, 300.0),
            80.0
        );
        assert_eq!(
            MainView::log_tree_context_menu_x(MAIN_NAV_WIDTH + 280.0, 300.0),
            300.0 - LOG_TREE_CONTEXT_MENU_WIDTH
        );
    }

    /// 验证笔记树右键菜单坐标同样扣除固定大导航宽度并限制在笔记树面板内。
    #[test]
    fn 笔记树菜单横坐标包含主导航宽度() {
        assert_eq!(
            MainView::notes_tree_context_menu_x(24.0, NOTES_TREE_DEFAULT_WIDTH),
            0.0
        );
        assert_eq!(
            MainView::notes_tree_context_menu_x(MAIN_NAV_WIDTH + 72.0, NOTES_TREE_DEFAULT_WIDTH),
            72.0
        );
        assert_eq!(
            MainView::notes_tree_context_menu_x(MAIN_NAV_WIDTH + 270.0, NOTES_TREE_DEFAULT_WIDTH),
            NOTES_TREE_DEFAULT_WIDTH - LOG_TREE_CONTEXT_MENU_WIDTH
        );
    }

    /// 验证笔记树默认宽度与 AI 对话左侧栏保持一致。
    ///
    /// 业务意图：
    /// - 两个功能页左侧都承载资源列表，默认宽度一致可以避免用户在导航切换时感知到不必要的布局跳动。
    #[test]
    fn 笔记树默认宽度与_ai_侧栏一致() {
        assert_eq!(NOTES_TREE_DEFAULT_WIDTH, AI_CHAT_CONVERSATION_LIST_WIDTH);
    }

    /// 验证笔记树拖拽宽度会被限制在可用范围内。
    ///
    /// 边界条件：
    /// - 拖得过窄时仍保留树行图标和短标题空间。
    /// - 拖得过宽时不能吞掉右侧笔记工作区。
    /// - 异常非有限数不应写入布局状态。
    #[test]
    fn 笔记树拖拽宽度限制在可用范围内() {
        let wide_window = MAIN_NAV_WIDTH + NOTES_WORKSPACE_MIN_WIDTH + NOTES_TREE_MAX_WIDTH + 120.0;
        assert_eq!(
            MainView::clamp_notes_tree_width(10.0, wide_window),
            NOTES_TREE_MIN_WIDTH
        );
        assert_eq!(
            MainView::clamp_notes_tree_width(10_000.0, wide_window),
            NOTES_TREE_MAX_WIDTH
        );

        let narrow_window = MAIN_NAV_WIDTH + NOTES_WORKSPACE_MIN_WIDTH + 260.0;
        assert_eq!(
            MainView::clamp_notes_tree_width(10_000.0, narrow_window),
            260.0
        );
        assert_eq!(
            MainView::clamp_notes_tree_width(f32::NAN, wide_window),
            NOTES_TREE_DEFAULT_WIDTH
        );
    }

    /// 验证搜索输入框双击选择连续非空白片段。
    ///
    /// 业务意图：
    /// - 关键字输入框和目录输入框共用双击选词规则；搜索场景中路径、类名和普通关键字都应作为连续 token 选中。
    /// - 空白只作为 token 分隔符，避免双击路径分隔符或点号时只选中一个标点，导致复制和替换不方便。
    #[test]
    fn 搜索输入双击选择连续非空白片段() {
        assert_eq!(
            MainView::search_text_word_range_for_index("foo.bar baz", 2),
            0..7
        );
        assert_eq!(
            MainView::search_text_word_range_for_index("foo  bar", 4),
            3..5
        );
    }

    /// 验证搜索输入框范围会夹到合法 UTF-8 边界。
    ///
    /// 边界条件：
    /// - 鼠标命中和平台输入协议都可能给出非字符边界的字节下标；中文路径或中文关键字不能因此触发切片 panic。
    #[test]
    fn 搜索输入范围保持_utf8_边界() {
        assert_eq!(MainView::clamp_search_text_range("a中b", 2..99), 1..5);
        assert_eq!(
            MainView::clamp_search_text_range("a中b", std::ops::Range { start: 4, end: 0 }),
            0..4
        );
    }

    /// 验证搜索输入框方向键按 UTF-8 字符边界移动。
    ///
    /// 业务意图：
    /// - 搜索关键字和目录路径都可能包含中文；方向键移动光标时必须一次跨过完整中文字符。
    /// - 如果按字节移动，后续删除、替换或 IME 组合文本提交都会因为非法边界而存在崩溃风险。
    #[test]
    fn 搜索输入方向键移动保持_utf8_边界() {
        let text = "a中b";

        assert_eq!(MainView::next_search_text_boundary(text, 0), 1);
        assert_eq!(MainView::next_search_text_boundary(text, 1), 4);
        assert_eq!(MainView::previous_search_text_boundary(text, 4), 1);
        assert_eq!(MainView::previous_search_text_boundary(text, 1), 0);
    }

    /// 验证搜索输入框光标活动后立即进入闪烁节奏。
    ///
    /// 业务意图：
    /// - 用户要求去除 1 秒延迟，停止移动后直接闪烁；该规则影响键盘编辑时的位置反馈。
    /// - 测试使用纯时间差，避免依赖真实时钟导致用例不稳定。
    #[test]
    fn 搜索输入光标活动后直接闪烁() {
        assert!(MainView::search_text_cursor_visible_for_elapsed(
            Duration::from_millis(119)
        ));
        assert!(!MainView::search_text_cursor_visible_for_elapsed(
            Duration::from_millis(500)
        ));
        assert!(MainView::search_text_cursor_visible_for_elapsed(
            Duration::from_millis(1000)
        ));
    }

    /// 验证单行输入框会按光标位置调整水平滚动。
    ///
    /// 业务意图：
    /// - 搜索关键字、当前目录、模型配置和快搜关键字都是自绘单行输入；当文本超过输入框可视范围后，
    ///   光标向右移动必须推动内容左移，向左移动时也必须把内容滚回可见区域。
    #[test]
    fn 单行输入水平滚动会跟随光标() {
        assert_eq!(
            MainView::single_line_horizontal_scroll_offset(
                0.0,
                px(180.0),
                px(240.0),
                px(100.0),
                true,
            ),
            89.5
        );
        assert_eq!(
            MainView::single_line_horizontal_scroll_offset(
                120.0,
                px(20.0),
                px(240.0),
                px(100.0),
                true,
            ),
            12.0
        );
    }

    /// 验证短文本和非聚焦单行输入不会发生不必要的横向跳动。
    ///
    /// 边界条件：
    /// - 内容宽度小于输入框时偏移必须归零；未聚焦时只夹紧已有偏移，避免普通重绘导致文本显示位置变化。
    #[test]
    fn 单行输入水平滚动会夹紧边界() {
        assert_eq!(
            MainView::single_line_horizontal_scroll_offset(
                20.0,
                px(80.0),
                px(90.0),
                px(100.0),
                true,
            ),
            0.0
        );
        assert_eq!(
            MainView::single_line_horizontal_scroll_offset(
                300.0,
                px(20.0),
                px(240.0),
                px(100.0),
                false,
            ),
            149.5
        );
    }

    /// 验证光标位于文本末尾时仍会保留完整可见空间。
    ///
    /// 业务意图：
    /// - 自绘单行输入在文本末尾需要允许额外滚出一小段空白，否则光标会贴在输入框右边界并被裁剪。
    /// - 这里锁定右侧保护区计算，避免搜索框和当前目录输入在长文本末尾回归为“看不到光标”。
    #[test]
    fn 单行输入末尾光标不会被右边界遮挡() {
        let scroll = MainView::single_line_horizontal_scroll_offset(
            0.0,
            px(240.0),
            px(240.0),
            px(100.0),
            true,
        );
        let cursor_viewport_x = 240.0 - scroll;
        let right_limit = 100.0 - SINGLE_LINE_INPUT_SCROLL_MARGIN - SINGLE_LINE_INPUT_CARET_WIDTH;

        assert_eq!(scroll, 149.5);
        assert!(cursor_viewport_x <= right_limit);
        assert!(cursor_viewport_x + SINGLE_LINE_INPUT_CARET_WIDTH < 100.0);
        assert!(cursor_viewport_x >= SINGLE_LINE_INPUT_SCROLL_MARGIN);
    }

    /// 验证搜索窗口级控制键只识别 Enter 和 Escape。
    ///
    /// 业务意图：
    /// - 设置窗口内的自绘文本框也会经过应用级快捷键拦截；只有明确属于搜索窗口的控制键才允许触发搜索或关闭窗口。
    /// - 该纯函数测试锁定按键归类，避免后续扩展快捷键时误把普通编辑键纳入全局搜索控制。
    #[test]
    fn 搜索窗口控制键只识别_enter_escape() {
        let keystroke = |key: &str| Keystroke {
            modifiers: Default::default(),
            key: key.to_string(),
            key_char: None,
        };

        assert_eq!(
            MainView::search_dialog_control_key(&keystroke("enter")),
            Some(SearchDialogControlKey::Submit)
        );
        assert_eq!(
            MainView::search_dialog_control_key(&keystroke("escape")),
            Some(SearchDialogControlKey::Close)
        );
        assert_eq!(MainView::search_dialog_control_key(&keystroke("a")), None);
    }

    /// 验证日志区域键盘滚动快捷键的按键映射。
    ///
    /// 业务意图：
    /// - `PageUp` / `PageDown` 用于区域翻页，`Ctrl+Home` / `Ctrl+End` 用于顶底跳转；这些映射一旦变化会直接破坏用户肌肉记忆。
    /// - 用户已明确去除 `Ctrl+Top`，测试需要锁定它不会被误识别为顶部跳转。
    #[test]
    fn 日志区域键盘滚动快捷键映射稳定() {
        let keystroke = |key: &str, control: bool| Keystroke {
            modifiers: gpui::Modifiers {
                control,
                ..Default::default()
            },
            key: key.to_string(),
            key_char: None,
        };

        assert_eq!(
            MainView::keyboard_scroll_command(&keystroke("pageup", false)),
            Some(KeyboardScrollCommand::PageUp)
        );
        assert_eq!(
            MainView::keyboard_scroll_command(&keystroke("page_down", false)),
            Some(KeyboardScrollCommand::PageDown)
        );
        assert_eq!(
            MainView::keyboard_scroll_command(&keystroke("end", true)),
            Some(KeyboardScrollCommand::Bottom)
        );
        assert_eq!(
            MainView::keyboard_scroll_command(&keystroke("home", true)),
            Some(KeyboardScrollCommand::Top)
        );
        assert_eq!(
            MainView::keyboard_scroll_command(&keystroke("end", false)),
            None
        );
        assert_eq!(
            MainView::keyboard_scroll_command(&keystroke("home", false)),
            None
        );
        assert_eq!(
            MainView::keyboard_scroll_command(&keystroke("top", true)),
            None
        );
    }

    /// 验证可编辑文本框聚焦时不拦截滚动快捷键。
    ///
    /// 业务意图：
    /// - 搜索框和设置文本框需要保留平台默认编辑导航；全局滚动逻辑只能在非编辑焦点下生效。
    #[test]
    fn 可编辑文本框聚焦时放行键盘滚动快捷键() {
        let keystroke = Keystroke {
            modifiers: Default::default(),
            key: "pagedown".to_string(),
            key_char: None,
        };

        assert_eq!(
            MainView::keyboard_scroll_command_for_focus(&keystroke, false),
            Some(KeyboardScrollCommand::PageDown)
        );
        assert_eq!(
            MainView::keyboard_scroll_command_for_focus(&keystroke, true),
            None
        );
    }

    /// 验证日志标记跳转只识别普通 F2。
    ///
    /// 业务意图：
    /// - 本次只支持向后循环跳转，`Shift+F2` 等组合键需要保留给未来反向跳转或系统默认行为。
    /// - 输入框聚焦时必须放行，避免全局功能键破坏搜索和设置文本编辑体验。
    #[test]
    fn 日志标记跳转快捷键只识别普通_f2() {
        let keystroke = |key: &str, modifiers: gpui::Modifiers| Keystroke {
            modifiers,
            key: key.to_string(),
            key_char: None,
        };

        assert!(MainView::marker_jump_keystroke_for_focus(
            &keystroke("f2", Default::default()),
            false
        ));
        assert!(!MainView::marker_jump_keystroke_for_focus(
            &keystroke("f2", Default::default()),
            true
        ));
        assert!(!MainView::marker_jump_keystroke(&keystroke(
            "f2",
            gpui::Modifiers {
                shift: true,
                ..Default::default()
            }
        )));
        assert!(!MainView::marker_jump_keystroke(&keystroke(
            "f2",
            gpui::Modifiers {
                control: true,
                ..Default::default()
            }
        )));
    }

    /// 验证日志行标记点击状态的切换规则。
    ///
    /// 业务意图：
    /// - 同一行再次点击应取消标记；多个标记必须按行号排序，才能让 `F2` 后续查找稳定。
    #[test]
    fn 日志行标记点击切换并按行号排序() {
        let mut marked_lines = BTreeSet::new();

        assert!(MainView::toggle_marked_line(&mut marked_lines, 8));
        assert!(MainView::toggle_marked_line(&mut marked_lines, 2));
        assert_eq!(marked_lines.iter().copied().collect::<Vec<_>>(), vec![2, 8]);

        assert!(!MainView::toggle_marked_line(&mut marked_lines, 8));
        assert_eq!(marked_lines.iter().copied().collect::<Vec<_>>(), vec![2]);
    }

    /// 验证 F2 标记跳转目标按当前视口和上次跳转位置选择。
    ///
    /// 业务意图：
    /// - 第一次跳转从可视顶部找最近后续标记；连续 `F2` 从上一次目标之后继续；到末尾后循环回第一个标记。
    /// - 用户手动滚动后，如果上次跳转目标已不在视口内，下一次应重新以当前视口为起点。
    #[test]
    fn 日志标记跳转目标按视口和上次跳转计算() {
        let marked_lines = BTreeSet::from([3usize, 8, 12]);

        assert_eq!(
            MainView::next_marked_line(&marked_lines, 5, 9, None),
            Some(8)
        );
        assert_eq!(
            MainView::next_marked_line(&marked_lines, 5, 9, Some(8)),
            Some(12)
        );
        assert_eq!(
            MainView::next_marked_line(&marked_lines, 10, 14, Some(12)),
            Some(3)
        );
        assert_eq!(
            MainView::next_marked_line(&marked_lines, 10, 14, Some(8)),
            Some(12)
        );
        assert_eq!(
            MainView::next_marked_line(&BTreeSet::new(), 0, 10, None),
            None
        );
    }

    /// 验证标记跳转可视范围计算会处理滚动像素和文档边界。
    ///
    /// 边界条件：
    /// - 内存日志滚动句柄给出的是像素偏移，必须换算成 0 基行号；超出文档末尾时要夹到最后一行。
    #[test]
    fn 日志标记可视范围从滚动像素换算为行号() {
        let row_height = f64::from(px(LOG_VIEWER_ROW_HEIGHT));

        assert_eq!(
            MainView::marker_visible_range_from_scroll(100, row_height * 20.0 + 3.0, px(90.0)),
            Some((20, 25))
        );
        assert_eq!(
            MainView::marker_visible_range_from_first_line(10, 50, px(90.0)),
            Some((9, 9))
        );
        assert_eq!(
            MainView::marker_visible_range_from_scroll(0, 0.0, px(90.0)),
            None
        );
    }

    /// 验证键盘滚动目标未记录时默认回退到日志正文。
    ///
    /// 业务意图：
    /// - 应用启动后用户可能直接按翻页键；此时没有鼠标进入记录，应按日志查看器默认工作流滚动当前日志正文。
    #[test]
    fn 键盘滚动目标默认日志正文且可切换() {
        assert_eq!(
            MainView::keyboard_scroll_region_or_default(None),
            KeyboardScrollRegion::LogContent
        );
        assert_eq!(
            MainView::keyboard_scroll_region_or_default(Some(KeyboardScrollRegion::SearchResults)),
            KeyboardScrollRegion::SearchResults
        );
        assert_eq!(
            MainView::keyboard_scroll_region_or_default(Some(KeyboardScrollRegion::LogTree)),
            KeyboardScrollRegion::LogTree
        );
    }

    /// 验证键盘翻页滚动位置按视口高度计算并限制在合法范围。
    ///
    /// 业务意图：
    /// - 翻页距离应为“视口高度减一行”，保留上下文；顶底和边界位置必须 clamp，避免滚动条越界。
    #[test]
    fn 键盘滚动位置按页距计算并夹紧() {
        let viewport_height = px(100.0);
        let row_height = 20.0;

        assert_eq!(
            MainView::keyboard_scroll_position(
                px(10.0),
                px(200.0),
                viewport_height,
                row_height,
                KeyboardScrollCommand::PageDown,
            ),
            Some(px(90.0))
        );
        assert_eq!(
            MainView::keyboard_scroll_position(
                px(10.0),
                px(200.0),
                viewport_height,
                row_height,
                KeyboardScrollCommand::PageUp,
            ),
            Some(px(0.0))
        );
        assert_eq!(
            MainView::keyboard_scroll_position(
                px(195.0),
                px(200.0),
                viewport_height,
                row_height,
                KeyboardScrollCommand::PageDown,
            ),
            Some(px(200.0))
        );
        assert_eq!(
            MainView::keyboard_scroll_position(
                px(80.0),
                px(200.0),
                viewport_height,
                row_height,
                KeyboardScrollCommand::Top,
            ),
            Some(px(0.0))
        );
        assert_eq!(
            MainView::keyboard_scroll_position(
                px(80.0),
                px(200.0),
                viewport_height,
                row_height,
                KeyboardScrollCommand::Bottom,
            ),
            Some(px(200.0))
        );
        assert_eq!(
            MainView::keyboard_scroll_position(
                px(0.0),
                px(0.0),
                viewport_height,
                row_height,
                KeyboardScrollCommand::PageDown,
            ),
            None
        );
    }

    /// 验证分页日志的 f64 滚动计算和普通列表保持同一页距语义。
    ///
    /// 业务意图：
    /// - 超大日志走分页路径后不能出现快捷键翻页幅度不同的问题；该测试锁定 `PagedLogScrollState.top_px` 的计算策略。
    #[test]
    fn 分页日志键盘滚动位置按_f64_逻辑夹紧() {
        let next = MainView::keyboard_scroll_position_px(
            50.0,
            500.0,
            px(120.0),
            20.0,
            KeyboardScrollCommand::PageDown,
        );
        assert_eq!(next, Some(150.0));

        let top = MainView::keyboard_scroll_position_px(
            50.0,
            500.0,
            px(120.0),
            20.0,
            KeyboardScrollCommand::Top,
        );
        assert_eq!(top, Some(0.0));

        let bottom = MainView::keyboard_scroll_position_px(
            50.0,
            500.0,
            px(120.0),
            20.0,
            KeyboardScrollCommand::Bottom,
        );
        assert_eq!(bottom, Some(500.0));

        assert_eq!(
            MainView::keyboard_scroll_position_px(
                0.0,
                0.0,
                px(120.0),
                20.0,
                KeyboardScrollCommand::PageDown,
            ),
            None
        );
    }

    /// 验证搜索关键字历史按最近使用排序并限制数量。
    ///
    /// 业务意图：
    /// - 搜索窗口没有日志选区时会用最近关键字预填；历史顺序和上限错误会直接影响重复搜索体验。
    /// - 该测试只覆盖纯状态逻辑，不依赖 GPUI 窗口或文本输入系统。
    #[test]
    fn 搜索关键字历史按最近使用排序并限制数量() {
        let mut history = Vec::new();
        for index in 0..12 {
            MainView::remember_search_query_in_history(
                &mut history,
                &format!("key-{index}"),
                SearchMatchMode::Literal,
            );
        }

        assert_eq!(history.len(), SEARCH_QUERY_HISTORY_LIMIT);
        assert_eq!(
            history.first().map(|item| item.query.as_str()),
            Some("key-11")
        );
        assert_eq!(
            history.last().map(|item| item.query.as_str()),
            Some("key-2")
        );

        MainView::remember_search_query_in_history(&mut history, "key-5", SearchMatchMode::Regex);
        assert_eq!(history.len(), SEARCH_QUERY_HISTORY_LIMIT);
        assert_eq!(
            history.first().map(|item| item.query.as_str()),
            Some("key-5")
        );
        assert_eq!(
            history.first().map(|item| item.match_mode),
            Some(SearchMatchMode::Regex)
        );
        assert_eq!(
            history.iter().filter(|item| item.query == "key-5").count(),
            2
        );
        MainView::remember_search_query_in_history(&mut history, "key-5", SearchMatchMode::Regex);
        assert_eq!(
            history
                .iter()
                .filter(|item| item.query == "key-5" && item.match_mode == SearchMatchMode::Regex)
                .count(),
            1
        );

        MainView::remember_search_query_in_history(&mut history, "   ", SearchMatchMode::Literal);
        assert_eq!(history.len(), SEARCH_QUERY_HISTORY_LIMIT);
    }

    /// 验证选择历史关键字会替换搜索框内容并关闭下拉菜单。
    ///
    /// 业务意图：
    /// - 历史下拉菜单选择项只负责恢复关键字，不立即启动搜索；恢复后光标应位于末尾，当前文件计数缓存必须失效。
    #[test]
    fn 搜索历史关键字选择后填入搜索框并关闭菜单() {
        let mut dialog = SearchDialogState {
            query_input: SingleLineTextInputState {
                text: "旧关键字".to_string(),
                selection_range: 0.."旧关键字".len(),
                marked_range: Some(0.."旧".len()),
                horizontal_scroll_px: 0.0,
                selection_drag: None,
            },
            query_history_menu_open: true,
            scope: SearchScope::CurrentFile,
            selected_file_sources: Vec::new(),
            directory_input: SingleLineTextInputState::empty(),
            case_sensitive: false,
            match_mode: SearchMatchMode::Literal,
            current_file_match_count: Some(7),
            current_file_navigation_match: None,
            is_searching: false,
            progress: SearchProgress::default(),
            message: String::new(),
            job_id: 0,
        };

        let history_item =
            SearchQueryHistoryItem::new("error", SearchMatchMode::Regex).expect("历史项应有效");
        MainView::apply_search_history_query(&mut dialog, &history_item);

        assert_eq!(dialog.query_input.text, "error");
        assert_eq!(dialog.match_mode, SearchMatchMode::Regex);
        assert_eq!(
            dialog.query_input.selection_range,
            "error".len().."error".len()
        );
        assert!(dialog.query_input.marked_range.is_none());
        assert!(!dialog.query_history_menu_open);
        assert!(dialog.current_file_match_count.is_none());
        assert_eq!(dialog.message, "已选择历史关键字，按 Enter 或点击搜索");
    }

    /// 验证激活搜索输入框会全选现有关键字。
    ///
    /// 业务意图：
    /// - 用户打开或重新激活搜索框时通常要替换关键字；全选行为必须直接体现在自绘输入框状态中。
    #[test]
    fn 激活搜索输入框会全选关键字并清理组合文本() {
        let mut dialog = SearchDialogState {
            query_input: SingleLineTextInputState {
                text: "error日志".to_string(),
                selection_range: 5..5,
                marked_range: Some(0..5),
                horizontal_scroll_px: 18.0,
                selection_drag: Some(5),
            },
            query_history_menu_open: false,
            scope: SearchScope::CurrentFile,
            selected_file_sources: Vec::new(),
            directory_input: SingleLineTextInputState::empty(),
            case_sensitive: false,
            match_mode: SearchMatchMode::Literal,
            current_file_match_count: None,
            current_file_navigation_match: None,
            is_searching: false,
            progress: SearchProgress::default(),
            message: String::new(),
            job_id: 0,
        };

        MainView::select_all_search_query(&mut dialog);

        assert_eq!(dialog.query_input.selection_range, 0.."error日志".len());
        assert!(dialog.query_input.marked_range.is_none());
        assert_eq!(dialog.query_input.horizontal_scroll_px, 0.0);
        assert!(dialog.query_input.selection_drag.is_none());
    }

    /// 验证切换正则模式会清空依赖旧匹配条件的当前文件计数缓存。
    ///
    /// 业务意图：
    /// - 同一个查询词在普通文本和正则模式下含义可能完全不同，旧计数继续展示会误导用户。
    #[test]
    fn 切换正则模式会清空当前文件计数缓存() {
        let mut dialog = SearchDialogState {
            query_input: SingleLineTextInputState {
                text: "error|warn".to_string(),
                selection_range: 0.."error|warn".len(),
                marked_range: None,
                horizontal_scroll_px: 0.0,
                selection_drag: None,
            },
            query_history_menu_open: true,
            scope: SearchScope::CurrentFile,
            selected_file_sources: Vec::new(),
            directory_input: SingleLineTextInputState::empty(),
            case_sensitive: true,
            match_mode: SearchMatchMode::Literal,
            current_file_match_count: Some(12),
            current_file_navigation_match: None,
            is_searching: false,
            progress: SearchProgress::default(),
            message: String::new(),
            job_id: 0,
        };

        MainView::set_search_dialog_match_mode(&mut dialog, SearchMatchMode::Regex);

        assert_eq!(dialog.match_mode, SearchMatchMode::Regex);
        assert!(dialog.current_file_match_count.is_none());
        assert!(!dialog.query_history_menu_open);
        assert!(dialog.case_sensitive);
    }

    /// 验证停止搜索只取消后台任务，不清空用户输入。
    ///
    /// 业务意图：
    /// - 搜索按钮切换为“停止”后，用户点击应立即让当前任务失效，但搜索窗口仍保留关键字和范围，方便修改后重新搜索。
    #[test]
    fn 停止搜索会保留输入并返回任务编号() {
        let mut dialog = SearchDialogState {
            query_input: SingleLineTextInputState::from_text("Exception".to_string()),
            query_history_menu_open: true,
            scope: SearchScope::CurrentDirectory,
            selected_file_sources: Vec::new(),
            directory_input: SingleLineTextInputState {
                text: "monitorThread".to_string(),
                selection_range: 0.."monitorThread".len(),
                marked_range: None,
                horizontal_scroll_px: 0.0,
                selection_drag: None,
            },
            case_sensitive: true,
            match_mode: SearchMatchMode::Literal,
            current_file_match_count: Some(3),
            current_file_navigation_match: None,
            is_searching: true,
            progress: SearchProgress {
                searched_files: 2,
                total_files: 10,
                matched_lines: 6,
            },
            message: "正在搜索 2/10 个文件，已命中 6 行".to_string(),
            job_id: 42,
        };

        let canceled_job_id = MainView::stop_search_dialog_task(&mut dialog);

        assert_eq!(canceled_job_id, Some(42));
        assert_eq!(dialog.query_input.text, "Exception");
        assert_eq!(dialog.scope, SearchScope::CurrentDirectory);
        assert_eq!(dialog.directory_input.text, "monitorThread");
        assert!(dialog.case_sensitive);
        assert!(!dialog.is_searching);
        assert!(!dialog.query_history_menu_open);
        assert_eq!(dialog.message, "搜索已停止，可修改条件后重新搜索");
    }

    /// 验证“选中文件”搜索范围的中文标签稳定。
    ///
    /// 业务意图：
    /// - 搜索对话框范围按钮和底部结果历史都读取同一个 `label`，这里锁定文案，避免后续改动导致 UI 展示不一致。
    #[test]
    fn 选中文件搜索范围标签为选中文件() {
        assert_eq!(SearchScope::SelectedFiles.label(), "选中文件");
    }

    /// 验证“选中文件”打开预设会写入固定来源快照。
    ///
    /// 业务意图：
    /// - 右键菜单打开搜索窗口时，搜索范围必须固定为当时选中的日志来源；后续左侧树选择变化不能修改当前窗口会话。
    /// - 预设还需要清空当前文件计数和导航缓存，避免跨范围后继续展示旧的当前文件状态。
    #[test]
    fn 选中搜索预设会固定来源快照并切换范围() {
        let first = test_local_file("/tmp/first.log");
        let second = test_local_file("/tmp/second.log");
        let mut external_sources = vec![first.clone()];
        let mut dialog = test_search_dialog_state("error");
        dialog.current_file_match_count = Some(8);
        dialog.current_file_navigation_match = Some(LogSearchMatchHighlight {
            line_index: 3,
            match_range: 0..5,
        });

        MainView::apply_search_dialog_open_preset_to_dialog(
            &mut dialog,
            SearchDialogOpenPreset::SelectedFiles {
                sources: external_sources.clone(),
            },
        );
        external_sources.push(second);

        assert_eq!(dialog.scope, SearchScope::SelectedFiles);
        assert_eq!(dialog.selected_file_sources, vec![first]);
        assert!(dialog.current_file_match_count.is_none());
        assert!(dialog.current_file_navigation_match.is_none());
        assert_eq!(
            dialog.message,
            "已选择 1 个文件，输入关键字后按 Enter 或点击搜索"
        );
    }

    /// 验证重新加载日志会清空“选中文件”搜索快照。
    ///
    /// 业务意图：
    /// - 搜索窗口允许跨重新加载保持打开，但旧目录树的选中文件快照不能继续参与搜索。
    /// - 如果当前范围是“选中文件”，重新加载后必须切回“当前文件”，避免用户在新工作区误搜旧日志。
    #[test]
    fn 重新加载日志会清空选中文件搜索快照并切回当前文件() {
        let mut dialog = test_search_dialog_state("error");
        dialog.scope = SearchScope::SelectedFiles;
        dialog.selected_file_sources = vec![test_local_file("/tmp/old.log")];
        dialog.current_file_match_count = Some(3);
        dialog.current_file_navigation_match = Some(LogSearchMatchHighlight {
            line_index: 1,
            match_range: 0..5,
        });
        dialog.is_searching = true;
        dialog.progress = SearchProgress {
            searched_files: 1,
            total_files: 1,
            matched_lines: 2,
        };

        MainView::reset_search_dialog_for_log_reload(&mut dialog);

        assert_eq!(dialog.scope, SearchScope::CurrentFile);
        assert!(dialog.selected_file_sources.is_empty());
        assert!(dialog.current_file_match_count.is_none());
        assert!(dialog.current_file_navigation_match.is_none());
        assert!(!dialog.is_searching);
        assert_eq!(dialog.progress, SearchProgress::default());
        assert_eq!(dialog.message, "日志已重新加载，请重新打开文件后搜索");
    }

    /// 验证“选中文件”范围禁用当前文件快捷按钮。
    ///
    /// 业务意图：
    /// - “计数 / 上一个 / 下一个”只面向当前文件；选中文件集合搜索下必须禁用，避免被误解为跨文件聚合快捷操作。
    #[test]
    fn 选中文件范围禁用当前文件快捷动作() {
        assert!(MainView::search_scope_allows_current_file_shortcuts(
            SearchScope::CurrentFile
        ));
        assert!(MainView::search_scope_allows_current_file_shortcuts(
            SearchScope::CurrentDirectory
        ));
        assert!(!MainView::search_scope_allows_current_file_shortcuts(
            SearchScope::SelectedFiles
        ));
    }

    /// 验证取消中的搜索记录优先展示取消状态。
    ///
    /// 业务意图：
    /// - 并行目录搜索被停止后，已启动的后台文件任务可能稍后才自然结束；结果面板必须根据取消标记展示“已取消”，不能误显示“搜索中”。
    #[test]
    fn 被取消搜索记录优先显示已取消() {
        let record = SearchHistoryRecord {
            job_id: 7,
            query: "error".to_string(),
            scope: SearchScope::CurrentDirectory,
            directory_target: Some("logs".to_string()),
            case_sensitive: false,
            match_mode: SearchMatchMode::Literal,
            progress: SearchProgress {
                searched_files: 1,
                total_files: 10,
                matched_lines: 2,
            },
            results: Vec::new(),
            result_groups: Vec::new(),
            result_group_indices: HashMap::new(),
            errors: Vec::new(),
            canceled: true,
            expanded: true,
            expanded_file_keys: HashSet::new(),
        };

        assert_eq!(record.state_label(), "已取消");
    }

    /// 验证新搜索启动失败时可以把上一轮搜索记录切换为取消态。
    ///
    /// 业务意图：
    /// - 用户在搜索运行中按 Enter 发起新搜索时，新条件可能因为无效正则或当前文件状态不满足而失败。
    /// - 旧后台任务会被对话框状态失效，因此结果面板记录也必须同步变为“已取消”，不能长期显示“搜索中”。
    #[test]
    fn 搜索启动失败会取消上一轮搜索记录() {
        let mut records = vec![SearchHistoryRecord {
            job_id: 7,
            query: "error".to_string(),
            scope: SearchScope::CurrentDirectory,
            directory_target: Some("logs".to_string()),
            case_sensitive: false,
            match_mode: SearchMatchMode::Literal,
            progress: SearchProgress {
                searched_files: 1,
                total_files: 10,
                matched_lines: 2,
            },
            results: Vec::new(),
            result_groups: Vec::new(),
            result_group_indices: HashMap::new(),
            errors: Vec::new(),
            canceled: false,
            expanded: true,
            expanded_file_keys: HashSet::new(),
        }];

        assert!(MainView::mark_search_record_canceled_in_records(
            &mut records,
            7
        ));

        assert!(records[0].canceled);
        assert_eq!(records[0].state_label(), "已取消");
    }

    /// 验证搜索输入框粘贴会替换当前选区并把光标放到插入文本之后。
    ///
    /// 业务意图：
    /// - 关键字输入框和目录输入框复用同一套粘贴替换逻辑；如果选区替换或光标位置错误，`Ctrl+V` 会破坏用户正在编辑的搜索条件。
    /// - 该测试覆盖纯字符串状态，不依赖系统剪贴板或 GPUI 窗口，避免平台差异造成测试不稳定。
    #[test]
    fn 搜索输入框粘贴替换选区并更新光标() {
        let mut text = "error warning info".to_string();
        let mut selection_range = 6..13;
        let mut marked_range = None;

        MainView::replace_search_text_selection(
            &mut text,
            &mut selection_range,
            &mut marked_range,
            "fatal",
        );

        assert_eq!(text, "error fatal info");
        assert_eq!(selection_range, 11..11);
        assert!(marked_range.is_none());
    }

    /// 验证搜索输入框粘贴优先替换平台组合文本范围。
    ///
    /// 业务意图：
    /// - 中文输入法组合态下用户触发粘贴时，应替换正在组合的临时文本，而不是错误插入到旧光标位置。
    /// - 该边界会同时影响关键字和目录输入框，因此用共享辅助函数做回归保护。
    #[test]
    fn 搜索输入框粘贴优先替换组合文本范围() {
        let mut text = "目录abc路径".to_string();
        let mut selection_range = text.len()..text.len();
        let mut marked_range = Some(6..9);

        MainView::replace_search_text_selection(
            &mut text,
            &mut selection_range,
            &mut marked_range,
            "日志",
        );

        assert_eq!(text, "目录日志路径");
        assert_eq!(selection_range, 12..12);
        assert!(marked_range.is_none());
    }

    /// 验证日志查看器粘贴会覆盖搜索框内已有预填关键字。
    ///
    /// 业务意图：
    /// - 日志正文有选区时，打开搜索窗口会先预填选中文本；如果随后处理日志查看器 `Ctrl+V`，
    ///   必须用剪贴板内容覆盖预填文本，而不是追加到预填文本末尾。
    /// - 该测试不访问系统剪贴板，只锁定状态变更规则，避免平台剪贴板权限影响回归验证。
    #[test]
    fn 日志查看器粘贴会覆盖已有搜索预填文本() {
        let mut dialog = SearchDialogState {
            query_input: SingleLineTextInputState::from_text("日志选中文本".to_string()),
            query_history_menu_open: false,
            scope: SearchScope::CurrentFile,
            selected_file_sources: Vec::new(),
            directory_input: SingleLineTextInputState::empty(),
            case_sensitive: false,
            match_mode: SearchMatchMode::Literal,
            current_file_match_count: Some(3),
            current_file_navigation_match: None,
            is_searching: false,
            progress: SearchProgress::default(),
            message: String::new(),
            job_id: 0,
        };

        MainView::replace_search_query_with_clipboard_text(&mut dialog, "剪贴板文本".to_string());

        assert_eq!(dialog.query_input.text, "剪贴板文本");
        assert_eq!(
            dialog.query_input.selection_range,
            "剪贴板文本".len().."剪贴板文本".len()
        );
        assert!(dialog.query_input.marked_range.is_none());
        assert_eq!(dialog.current_file_match_count, None);
        assert_eq!(dialog.message, "已粘贴剪贴板文本，按 Enter 或点击搜索");
    }

    /// 验证主题配置文本只接受稳定的三种持久化值。
    ///
    /// 业务意图：
    /// - 设置窗口显示中文文案，但配置文件必须使用 `light`、`dark`、`system`，避免 UI 文案调整破坏历史配置。
    /// - 损坏文本、空文本和未知值应回退到默认“跟随系统”，因此解析层返回 `None`。
    #[test]
    fn 主题配置文本解析合法值和损坏值() {
        assert_eq!(
            parse_theme_preference("light\n"),
            Some(ThemePreference::Light)
        );
        assert_eq!(parse_theme_preference("dark"), Some(ThemePreference::Dark));
        assert_eq!(
            parse_theme_preference("system"),
            Some(ThemePreference::System)
        );
        assert_eq!(parse_theme_preference(""), None);
        assert_eq!(parse_theme_preference("unknown"), None);
    }

    /// 验证主题配置文件可以完成写入和读取往返。
    ///
    /// 边界条件：
    /// - 写入函数需要自动创建父目录，保证首次修改主题时配置目录不存在也能保存。
    #[test]
    fn 主题配置可以读写往返() {
        let path = test_theme_preference_file_path("roundtrip").join(THEME_PREFERENCE_FILE_NAME);

        write_theme_preference(&path, ThemePreference::Dark).expect("主题配置应能写入临时目录");
        assert_eq!(read_theme_preference(&path), Some(ThemePreference::Dark));

        write_theme_preference(&path, ThemePreference::System)
            .expect("主题配置应能覆盖写入临时目录");
        assert_eq!(read_theme_preference(&path), Some(ThemePreference::System));

        let _ = fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    /// 验证主题配置缺失、损坏或目录不可用时不会阻断应用启动。
    ///
    /// 业务意图：
    /// - 主题偏好只是界面体验配置，任何文件系统异常都应回退到“跟随系统”，不能影响日志查看主流程。
    #[test]
    fn 主题配置异常会被忽略() {
        let missing_path =
            test_theme_preference_file_path("missing").join(THEME_PREFERENCE_FILE_NAME);
        assert_eq!(read_theme_preference(&missing_path), None);

        let broken_path =
            test_theme_preference_file_path("broken").join(THEME_PREFERENCE_FILE_NAME);
        fs::create_dir_all(broken_path.parent().expect("测试路径应包含父目录"))
            .expect("测试目录应能创建");
        fs::write(&broken_path, "broken-theme").expect("测试损坏配置应能写入");
        assert_eq!(read_theme_preference(&broken_path), None);

        let directory_path = test_theme_preference_file_path("directory");
        fs::create_dir_all(&directory_path).expect("测试目录应能创建");
        assert_eq!(read_theme_preference(&directory_path), None);
        assert!(write_theme_preference(&directory_path, ThemePreference::Light).is_err());

        let _ = fs::remove_file(&broken_path);
        if let Some(parent) = broken_path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
        let _ = fs::remove_dir_all(&directory_path);
    }

    /// 验证日志显示字号配置只接受允许范围内的数值。
    ///
    /// 业务意图：
    /// - 设置页提供 10px 到 20px 的字号调整；配置文件被手工修改时也必须遵守同一边界，避免日志正文不可读或挤破固定行高。
    #[test]
    fn 日志显示字号配置解析合法值和损坏值() {
        assert_eq!(
            parse_log_viewer_font_size_preference("12\n"),
            Some(LOG_VIEWER_DEFAULT_FONT_SIZE)
        );
        assert_eq!(parse_log_viewer_font_size_preference("15.4"), Some(15.0));
        assert_eq!(parse_log_viewer_font_size_preference(""), None);
        assert_eq!(parse_log_viewer_font_size_preference("broken"), None);
        assert_eq!(
            parse_log_viewer_font_size_preference(&(LOG_VIEWER_MIN_FONT_SIZE - 1.0).to_string()),
            None
        );
        assert_eq!(
            parse_log_viewer_font_size_preference(&(LOG_VIEWER_MAX_FONT_SIZE + 1.0).to_string()),
            None
        );
    }

    /// 验证日志显示字号配置可以完成写入和读取往返。
    ///
    /// 边界条件：
    /// - 写入函数需要自动创建父目录；非法字号应返回错误，而不是写入损坏配置。
    #[test]
    fn 日志显示字号配置可以读写往返() {
        let path = test_log_font_size_file_path("roundtrip").join(LOG_VIEWER_FONT_SIZE_FILE_NAME);

        write_log_viewer_font_size_preference(&path, 14.0).expect("日志字号配置应能写入临时目录");
        assert_eq!(read_log_viewer_font_size_preference(&path), Some(14.0));

        assert!(write_log_viewer_font_size_preference(&path, 99.0).is_err());

        let _ = fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    /// 验证日志 minimap 配置默认关闭，并兼容常见布尔文本。
    ///
    /// 业务意图：
    /// - minimap 影响日志打开和滚动性能，首次启动或配置损坏时必须保持关闭。
    /// - 用户手工编辑配置时可能输入 true/false 或 on/off，解析层应稳定接受这些低风险同义值。
    #[test]
    fn 日志_minimap_开关配置解析合法值和损坏值() {
        assert!(!LOG_MINIMAP_DEFAULT_ENABLED);
        assert_eq!(
            parse_log_minimap_enabled_preference("enabled\n"),
            Some(true)
        );
        assert_eq!(parse_log_minimap_enabled_preference("true"), Some(true));
        assert_eq!(parse_log_minimap_enabled_preference("on"), Some(true));
        assert_eq!(
            parse_log_minimap_enabled_preference("disabled"),
            Some(false)
        );
        assert_eq!(parse_log_minimap_enabled_preference("false"), Some(false));
        assert_eq!(parse_log_minimap_enabled_preference("0"), Some(false));
        assert_eq!(parse_log_minimap_enabled_preference(""), None);
        assert_eq!(parse_log_minimap_enabled_preference("broken"), None);
    }

    /// 验证日志 minimap 开关配置可以完成写入和读取往返。
    ///
    /// 边界条件：
    /// - 配置文件缺失或损坏时读取返回 `None`，调用方才会回退默认关闭；读写函数本身不访问真实应用配置目录。
    #[test]
    fn 日志_minimap_开关配置可以读写往返() {
        let path =
            test_log_minimap_enabled_file_path("roundtrip").join(LOG_MINIMAP_ENABLED_FILE_NAME);

        assert_eq!(read_log_minimap_enabled_preference(&path), None);

        write_log_minimap_enabled_preference(&path, true)
            .expect("日志 minimap 开关配置应能写入临时目录");
        assert_eq!(read_log_minimap_enabled_preference(&path), Some(true));

        write_log_minimap_enabled_preference(&path, false)
            .expect("日志 minimap 开关配置应能覆盖写入临时目录");
        assert_eq!(read_log_minimap_enabled_preference(&path), Some(false));

        fs::write(&path, "broken-minimap").expect("测试损坏 minimap 配置应能写入");
        assert_eq!(read_log_minimap_enabled_preference(&path), None);

        let _ = fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    /// 验证线程分析堆栈过滤配置缺失时回退到内置默认堆栈规则。
    ///
    /// 业务意图：
    /// - 首次使用线程分析时应默认过滤 Resin 网络线程堆栈，减少时间线噪声，同时不要求用户先进入设置维护规则。
    #[test]
    fn 线程分析过滤配置缺失返回默认规则() {
        let path =
            test_thread_analysis_filter_file_path("missing").join(THREAD_ANALYSIS_FILTER_FILE_NAME);

        let text = read_thread_analysis_filter_preference(&path);
        assert_eq!(text, DEFAULT_THREAD_ANALYSIS_FILTER_TEXT);
        assert!(text.contains("TcpSocketAcceptThread.run"));
        assert!(text.contains("SocketInputStream.socketRead0"));
    }

    /// 验证线程分析线程名过滤配置缺失时回退到内置默认线程名规则。
    ///
    /// 业务意图：
    /// - 线程名过滤已经独立成单独输入框和配置文件，缺失文件时仍要默认过滤常见 JVM 常驻线程。
    #[test]
    fn 线程分析线程名过滤配置缺失返回默认规则() {
        let path = test_thread_analysis_filter_file_path("name-missing")
            .join(THREAD_ANALYSIS_NAME_FILTER_FILE_NAME);

        let text = read_thread_analysis_name_filter_preference(&path);
        assert_eq!(text, DEFAULT_THREAD_ANALYSIS_NAME_FILTER_TEXT);
        assert!(text.contains("C1 CompilerThread*"));
        assert!(text.contains("C2 CompilerThread*"));
        assert!(text.contains("Service Thread"));
        assert!(text.contains("Attach Listener"));
    }

    /// 验证线程分析过滤配置的空文件会覆盖默认堆栈。
    ///
    /// 业务意图：
    /// - 用户点击“清空”后必须真正禁用默认过滤；否则内置规则会在下次启动时悄悄恢复，导致设置行为不可预测。
    #[test]
    fn 线程分析过滤配置空文件保留为空文本() {
        let path =
            test_thread_analysis_filter_file_path("empty").join(THREAD_ANALYSIS_FILTER_FILE_NAME);

        write_thread_analysis_filter_preference(&path, "").expect("线程过滤配置应能写入空文本");
        assert_eq!(read_thread_analysis_filter_preference(&path), "");

        let _ = fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    /// 验证线程分析过滤配置可以保存多行文本并规范化换行。
    ///
    /// 业务意图：
    /// - 用户会从不同平台粘贴线程堆栈，配置读写必须把 CRLF 统一成 LF，保证后续规则解析稳定。
    #[test]
    fn 线程分析过滤配置多行读写往返并规范化换行() {
        let path = test_thread_analysis_filter_file_path("roundtrip")
            .join(THREAD_ANALYSIS_FILTER_FILE_NAME);

        write_thread_analysis_filter_preference(&path, "\"worker\" #1\r\n  at demo.A.run\r\n")
            .expect("线程过滤配置应能写入临时目录");
        assert_eq!(
            read_thread_analysis_filter_preference(&path),
            "\"worker\" #1\n  at demo.A.run\n"
        );

        let _ = fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    /// 验证线程名过滤配置可以保存多行文本并规范化换行。
    ///
    /// 业务意图：
    /// - 线程名规则在设置页单独输入，读写函数必须和堆栈过滤保持相同 CRLF 规范化行为。
    #[test]
    fn 线程分析线程名过滤配置多行读写往返并规范化换行() {
        let path = test_thread_analysis_filter_file_path("name-roundtrip")
            .join(THREAD_ANALYSIS_NAME_FILTER_FILE_NAME);

        write_thread_analysis_name_filter_preference(&path, "C1 CompilerThread*\r\nService Thread")
            .expect("线程名过滤配置应能写入临时目录");
        assert_eq!(
            read_thread_analysis_name_filter_preference(&path),
            "C1 CompilerThread*\nService Thread"
        );

        let _ = fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    /// 验证快搜关键字配置缺失时返回内置默认关键字。
    ///
    /// 业务意图：
    /// - 首次使用快搜时应默认覆盖常见导入、导出、转换和水印相关排障词，减少用户进入设置后才能使用的前置步骤。
    #[test]
    fn 快搜关键字配置缺失返回默认关键字() {
        let path =
            test_quick_search_keywords_file_path("missing").join(QUICK_SEARCH_KEYWORDS_FILE_NAME);

        assert_eq!(
            read_quick_search_keywords_preference(&path),
            DEFAULT_QUICK_SEARCH_KEYWORDS_TEXT
        );
        assert_eq!(
            parse_quick_search_keywords(&read_quick_search_keywords_preference(&path)),
            vec![
                "excel".to_string(),
                "import".to_string(),
                "export".to_string(),
                "wbi".to_string(),
                "convertFile".to_string(),
                "waterMark".to_string(),
            ]
        );
    }

    /// 验证快搜关键字配置空文件会覆盖默认关键字。
    ///
    /// 业务意图：
    /// - 用户保存空配置表示显式禁用默认快搜关键字，后续启动不能悄悄恢复内置值。
    #[test]
    fn 快搜关键字配置空文件保留为空文本() {
        let path =
            test_quick_search_keywords_file_path("empty").join(QUICK_SEARCH_KEYWORDS_FILE_NAME);

        write_quick_search_keywords_preference(&path, "").expect("快搜关键字配置应能写入空文本");
        assert_eq!(read_quick_search_keywords_preference(&path), "");

        let _ = fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    /// 验证快搜关键字配置读写保持单行文本。
    ///
    /// 业务意图：
    /// - 设置页输入框是单行配置，读写时需要移除平台换行，保证重启后仍按英文逗号解析。
    #[test]
    fn 快搜关键字配置多关键字读写往返并移除换行() {
        let path =
            test_quick_search_keywords_file_path("roundtrip").join(QUICK_SEARCH_KEYWORDS_FILE_NAME);

        write_quick_search_keywords_preference(&path, "ERROR, Exception\r\nTimeout")
            .expect("快搜关键字配置应能写入临时目录");
        assert_eq!(
            read_quick_search_keywords_preference(&path),
            "ERROR, ExceptionTimeout"
        );

        let _ = fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    /// 验证快搜关键字只按英文逗号拆分。
    ///
    /// 业务意图：
    /// - 用户已确认中文逗号不作为分隔符；连续英文逗号和空白项需要被忽略，避免生成空关键字。
    #[test]
    fn 快搜关键字解析只支持英文逗号并忽略空项() {
        assert_eq!(
            parse_quick_search_keywords("a,b,, c"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert_eq!(
            parse_quick_search_keywords("a，b"),
            vec!["a，b".to_string()]
        );
    }

    /// 验证模型配置缺失和损坏时回退空配置。
    ///
    /// 业务意图：
    /// - 模型配置不是日志查看主流程的必要条件，配置文件缺失或 JSON 损坏不能阻断应用启动或设置窗口打开。
    #[test]
    fn 模型配置缺失和损坏返回空配置() {
        let missing_path = test_model_configs_file_path("missing").join(MODEL_CONFIGS_FILE_NAME);
        assert_eq!(
            read_model_configs_preference(&missing_path),
            ModelConfigs::default()
        );

        let broken_path = test_model_configs_file_path("broken").join(MODEL_CONFIGS_FILE_NAME);
        fs::create_dir_all(broken_path.parent().expect("测试路径应包含父目录"))
            .expect("测试目录应能创建");
        fs::write(&broken_path, "{broken json").expect("测试损坏 JSON 应能写入");
        assert_eq!(
            read_model_configs_preference(&broken_path),
            ModelConfigs::default()
        );

        let _ = fs::remove_file(&broken_path);
        if let Some(parent) = broken_path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    /// 验证模型配置 JSON 可以保持多配置和默认 ID 往返。
    ///
    /// 业务意图：
    /// - `model-configs.json` 同时保存列表和默认模型引用，读写必须保持 API Key、Base URL 和默认选择不丢失。
    #[test]
    fn 模型配置可以读写往返() {
        let path = test_model_configs_file_path("roundtrip").join(MODEL_CONFIGS_FILE_NAME);
        let configs = ModelConfigs {
            profiles: vec![
                ModelProfile {
                    id: "openai".to_string(),
                    name: "OpenAI".to_string(),
                    base_url: "https://api.openai.com/v1".to_string(),
                    api_key: "sk-test".to_string(),
                    model: "gpt-4.1-mini".to_string(),
                },
                ModelProfile {
                    id: "local".to_string(),
                    name: "Local".to_string(),
                    base_url: "http://127.0.0.1:11434/v1".to_string(),
                    api_key: String::new(),
                    model: "qwen2.5:7b".to_string(),
                },
            ],
            default_profile_id: Some("local".to_string()),
        };

        write_model_configs_preference(&path, &configs).expect("模型配置应能写入临时目录");
        assert_eq!(read_model_configs_preference(&path), configs);
        let raw = fs::read_to_string(&path).expect("测试配置应能读取原文");
        assert!(raw.contains('\n'), "pretty JSON 应包含换行，方便手工排查");

        let _ = fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    /// 验证悬空默认模型 ID 会被清空。
    ///
    /// 业务意图：
    /// - 删除默认模型或手工编辑配置后，默认 ID 不能继续指向不存在的配置，否则后续智能功能会引用错误档案。
    #[test]
    fn 模型配置悬空默认_id_会清空() {
        let configs = normalize_model_configs(ModelConfigs {
            profiles: vec![ModelProfile {
                id: "exists".to_string(),
                name: "可用配置".to_string(),
                base_url: "https://api.example.com/v1".to_string(),
                api_key: String::new(),
                model: "demo".to_string(),
            }],
            default_profile_id: Some("missing".to_string()),
        });

        assert_eq!(configs.default_profile_id, None);
    }

    /// 验证模型配置表单校验规则。
    ///
    /// 业务意图：
    /// - 保存和测试都必须拒绝空名称、空 Base URL、空模型 ID 和非法协议；API Key 可为空以兼容本地服务。
    #[test]
    fn 模型配置校验必填字段和_url_协议() {
        assert!(
            validate_model_profile_fields("OpenAI", "https://api.openai.com/v1", "gpt").is_ok()
        );
        assert!(validate_model_profile_fields("", "https://api.openai.com/v1", "gpt").is_err());
        assert!(validate_model_profile_fields("OpenAI", "", "gpt").is_err());
        assert!(
            validate_model_profile_fields("OpenAI", "ftp://api.example.com/v1", "gpt").is_err()
        );
        assert!(validate_model_profile_fields("OpenAI", "http://localhost:11434/v1", "").is_err());
    }

    /// 验证模型测试请求构造符合 OpenAI Chat Completions 兼容格式。
    ///
    /// 业务意图：
    /// - 测试按钮会产生真实模型调用，URL、鉴权头和请求体必须稳定，避免用户配置正确但测试请求格式错误。
    #[test]
    fn 模型测试请求构造稳定() {
        assert_eq!(
            model_test_chat_completions_url("https://api.openai.com/v1/").unwrap(),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            model_test_authorization_header(" sk-test "),
            Some("Bearer sk-test".to_string())
        );
        assert_eq!(model_test_authorization_header("   "), None);

        let body = model_test_request_body(" gpt-4.1-mini ");
        assert_eq!(body["model"], "gpt-4.1-mini");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "ping");
        assert_eq!(body["max_tokens"], 1);
        assert_eq!(body["stream"], false);
    }

    /// 验证模型测试响应只接受包含 choices 的 JSON。
    ///
    /// 边界条件：
    /// - HTTP 2xx 但响应不是 Chat Completions 结构时，应提示失败，避免把错误接口误判为可用。
    #[test]
    fn 模型测试响应必须包含_choices() {
        assert!(model_test_response_has_choices(
            &serde_json::json!({ "choices": [] })
        ));
        assert!(!model_test_response_has_choices(
            &serde_json::json!({ "ok": true })
        ));
        assert!(!model_test_response_has_choices(
            &serde_json::json!({ "choices": {} })
        ));
    }

    /// 验证用户强制主题优先于系统外观。
    ///
    /// 业务意图：
    /// - 用户选择明亮或暗色后，系统外观变化不能覆盖该选择，避免设置窗口显示的偏好和实际界面不一致。
    #[test]
    fn 强制主题优先于系统外观() {
        assert_eq!(
            EffectiveTheme::resolve(ThemePreference::Light, WindowAppearance::Dark),
            EffectiveTheme::Light
        );
        assert_eq!(
            EffectiveTheme::resolve(ThemePreference::Dark, WindowAppearance::Light),
            EffectiveTheme::Dark
        );
    }

    /// 验证跟随系统会按 GPUI 窗口外观计算实际主题。
    ///
    /// 边界条件：
    /// - macOS 可能上报 `VibrantLight` 或 `VibrantDark`，这些外观必须分别归入明亮和暗色调色板。
    #[test]
    fn 跟随系统根据窗口外观计算实际主题() {
        assert_eq!(
            EffectiveTheme::resolve(ThemePreference::System, WindowAppearance::Light),
            EffectiveTheme::Light
        );
        assert_eq!(
            EffectiveTheme::resolve(ThemePreference::System, WindowAppearance::VibrantLight),
            EffectiveTheme::Light
        );
        assert_eq!(
            EffectiveTheme::resolve(ThemePreference::System, WindowAppearance::Dark),
            EffectiveTheme::Dark
        );
        assert_eq!(
            EffectiveTheme::resolve(ThemePreference::System, WindowAppearance::VibrantDark),
            EffectiveTheme::Dark
        );
    }

    /// 验证基础调色板避免纯白纯黑的大面积背景。
    ///
    /// 业务意图：
    /// - 程序用于长时间查看日志，大面积纯白或接近纯黑会增加视觉疲劳；调色板应使用更柔和的底色。
    /// - 该测试只锁定“舒适性配置”的边界，不约束布局、字号或信息密度。
    #[test]
    fn 基础调色板避免极端背景并保留状态区分() {
        let light = AppThemePalette::for_theme(EffectiveTheme::Light);
        assert_ne!(light.background, 0xffffff);
        assert_ne!(light.panel, light.background);
        assert_ne!(light.hover, light.panel);
        assert_ne!(light.selected, light.hover);

        let dark = AppThemePalette::for_theme(EffectiveTheme::Dark);
        assert_ne!(dark.background, 0x000000);
        assert_ne!(dark.input, 0x000000);
        assert_ne!(dark.panel, dark.background);
        assert_ne!(dark.hover, dark.panel);
        assert_ne!(dark.selected, dark.hover);
    }

    /// 构造测试用目录树行。
    ///
    /// 业务意图：
    /// - 测试只关心节点层级、类型和来源，统一构造函数可以避免每个用例重复填充无关展示字段。
    fn test_tree_row(
        id: usize,
        depth: usize,
        kind: LogTreeEntryKind,
        has_children: bool,
        source: Option<LogFileSource>,
    ) -> LoadedLogTreeRow {
        LoadedLogTreeRow {
            id,
            depth,
            label: format!("node-{}", id),
            kind,
            has_children,
            meta: None,
            error_message: None,
            source,
        }
    }

    /// 构造测试用压缩包成员来源。
    ///
    /// 边界条件：
    /// - 路径只用于来源相等性判断，不会在测试中访问真实文件系统。
    fn test_archive_member(member_path: &str) -> LogFileSource {
        LogFileSource::ArchiveMember {
            archive_path: PathBuf::from("logs.zip"),
            archive_format: ArchiveFormat::Zip,
            member_path: member_path.to_string(),
        }
    }

    /// 构造测试用本地日志来源。
    ///
    /// 边界条件：
    /// - 测试只比较来源是否被识别为唯一日志，不访问真实磁盘路径。
    fn test_local_file(path: &str) -> LogFileSource {
        LogFileSource::LocalFile {
            path: PathBuf::from(path),
        }
    }

    /// 构造测试用搜索对话框状态。
    ///
    /// 业务意图：
    /// - 多个测试只关心搜索范围、来源快照或高亮副作用，统一构造可以避免重复填充进度、历史菜单和输入框等无关字段。
    /// - 默认范围使用“当前文件”，选中文件来源为空，确保测试必须显式设置自己关心的业务状态。
    fn test_search_dialog_state(query: &str) -> SearchDialogState {
        SearchDialogState {
            query_input: SingleLineTextInputState::from_text(query.to_string()),
            query_history_menu_open: false,
            scope: SearchScope::CurrentFile,
            selected_file_sources: Vec::new(),
            directory_input: SingleLineTextInputState::empty(),
            case_sensitive: false,
            match_mode: SearchMatchMode::Literal,
            current_file_match_count: None,
            current_file_navigation_match: None,
            is_searching: false,
            progress: SearchProgress::default(),
            message: String::new(),
            job_id: 0,
        }
    }

    /// 验证左侧目录树 Shift 多选只覆盖当前可见范围。
    ///
    /// 业务意图：
    /// - 多选基于用户当前看到的虚拟列表顺序；折叠隐藏的节点不应被间接选中并参与右键文件操作。
    #[test]
    fn 左侧树_shift_多选覆盖可见范围() {
        let visible_node_ids = vec![10, 11, 12, 13];
        let mut selected = HashSet::new();
        let mut anchor = None;

        MainView::apply_log_tree_selection_click(
            &mut selected,
            &mut anchor,
            &visible_node_ids,
            11,
            1,
            false,
            false,
        );
        MainView::apply_log_tree_selection_click(
            &mut selected,
            &mut anchor,
            &visible_node_ids,
            13,
            3,
            true,
            false,
        );

        assert_eq!(selected, HashSet::from([11, 12, 13]));
        assert_eq!(anchor, Some(11));
    }

    /// 验证 Ctrl/Command 点击会切换单个节点选中态。
    ///
    /// 业务意图：
    /// - 用户需要从多选集合中增删个别日志文件，不能每次点击都清空已有选择。
    #[test]
    fn 左侧树_ctrl_多选切换单个节点() {
        let visible_node_ids = vec![1, 2, 3];
        let mut selected = HashSet::from([1, 2]);
        let mut anchor = Some(1);

        MainView::apply_log_tree_selection_click(
            &mut selected,
            &mut anchor,
            &visible_node_ids,
            2,
            1,
            false,
            true,
        );
        MainView::apply_log_tree_selection_click(
            &mut selected,
            &mut anchor,
            &visible_node_ids,
            3,
            2,
            false,
            true,
        );

        assert_eq!(selected, HashSet::from([1, 3]));
        assert_eq!(anchor, Some(3));
    }

    /// 验证左侧目录树普通单击会执行打开或展开主动作。
    ///
    /// 业务意图：
    /// - 用户要求日志和目录从双击改为单击触发；该测试锁定文件优先打开、目录展开的判断规则。
    /// - 单文件压缩包会同时表现为压缩包节点和文件来源，因此有来源时必须优先打开，不能误判为展开目录。
    #[test]
    fn 左侧树普通单击触发打开或展开() {
        assert_eq!(
            MainView::log_tree_primary_action_for_click(true, false, false, false, 1),
            LogTreePrimaryClickAction::OpenSource
        );
        assert_eq!(
            MainView::log_tree_primary_action_for_click(false, true, false, false, 1),
            LogTreePrimaryClickAction::ToggleNode
        );
        assert_eq!(
            MainView::log_tree_primary_action_for_click(true, true, false, false, 1),
            LogTreePrimaryClickAction::OpenSource
        );
    }

    /// 验证多选和双击后续事件不会重复执行目录树主动作。
    ///
    /// 业务意图：
    /// - Shift/Ctrl/Command 点击要服务多选，不应顺带打开文件或展开目录。
    /// - 双击会产生第二次鼠标按下事件，如果不忽略会导致目录展开后立刻收起。
    #[test]
    fn 左侧树多选和双击后续事件不触发主动作() {
        assert_eq!(
            MainView::log_tree_primary_action_for_click(true, false, true, false, 1),
            LogTreePrimaryClickAction::None
        );
        assert_eq!(
            MainView::log_tree_primary_action_for_click(false, true, false, true, 1),
            LogTreePrimaryClickAction::None
        );
        assert_eq!(
            MainView::log_tree_primary_action_for_click(false, true, false, false, 2),
            LogTreePrimaryClickAction::None
        );
    }

    /// 验证日志加载脉冲点透明度始终保持可见。
    ///
    /// 业务意图：
    /// - 大日志打开期间加载态需要持续动起来；透明度计算不能返回 0，否则某些相位会让动画看起来停顿。
    /// - 该测试只覆盖纯数学边界，实际动画帧由 GPUI 在渲染层驱动。
    #[test]
    fn 日志加载脉冲透明度保持可见范围() {
        for delta in [0.0, 0.25, 0.5, 0.75, 0.99] {
            let opacity = MainView::loading_dot_opacity(delta, 1.0 / 3.0);
            assert!((0.35..=1.0).contains(&opacity));
        }
    }

    /// 验证另存为路径只保留最终文件名。
    ///
    /// 业务意图：
    /// - 用户要求另存为直接保存到目标目录，不保留本地父目录、压缩包内部目录或嵌套压缩包路径。
    #[test]
    fn 另存为路径只保留最终文件名() {
        let local = LogFileSource::LocalFile {
            path: PathBuf::from("/tmp/a/server.log"),
        };
        let archive = test_archive_member("thread/2026/thread.log");
        let materialized_archive = LogFileSource::MaterializedArchiveMember {
            archive_path: PathBuf::from("/tmp/logs.7z"),
            archive_format: ArchiveFormat::SevenZ,
            member_path: "a/app.log".to_string(),
            temp_path: PathBuf::from("/tmp/LogClinic/sevenz/a/app.log"),
        };
        let nested_archive = LogFileSource::NestedArchiveMember {
            outer_archive_path: PathBuf::from("/tmp/logs.zip"),
            outer_archive_format: ArchiveFormat::Zip,
            archive_member_path: "nested/inner.zip".to_string(),
            nested_archive_format: ArchiveFormat::Zip,
            nested_member_path: "logs/error.log".to_string(),
        };

        assert_eq!(
            MainView::save_relative_path_for_source(&local),
            PathBuf::from("server.log")
        );
        assert_eq!(
            MainView::save_relative_path_for_source(&archive),
            PathBuf::from("thread.log")
        );
        assert_eq!(
            MainView::save_relative_path_for_source(&materialized_archive),
            PathBuf::from("app.log")
        );
        assert_eq!(
            MainView::save_relative_path_for_source(&nested_archive),
            PathBuf::from("error.log")
        );
    }

    /// 验证另存为会在写入前发现目标目录中的同名文件。
    ///
    /// 业务意图：
    /// - 用户要求同名文件必须弹窗确认，因此保存管线在后台复制前需要先计算冲突目标路径。
    #[test]
    fn 另存为会检测目标目录同名文件() {
        let root = test_save_as_directory("conflict-detect");
        let source_dir = root.join("source");
        let target_dir = root.join("target");
        fs::create_dir_all(&source_dir).expect("测试源目录应能创建");
        fs::create_dir_all(&target_dir).expect("测试目标目录应能创建");
        let source_path = source_dir.join("server.log");
        let target_path = target_dir.join("server.log");
        fs::write(&source_path, "new").expect("测试源文件应能写入");
        fs::write(&target_path, "old").expect("测试目标文件应能写入");

        let source = LogFileSource::LocalFile { path: source_path };
        let conflicts = MainView::save_target_conflicts(&[source], &target_dir);

        assert_eq!(conflicts, vec![target_path]);

        let _ = fs::remove_dir_all(&root);
    }

    /// 验证另存为同名文件时跳过和覆盖策略。
    ///
    /// 业务意图：
    /// - 弹窗中的“跳过”必须保留目标文件原内容，“覆盖”必须用源文件内容替换目标文件。
    #[test]
    fn 另存为同名文件支持跳过和覆盖策略() {
        let root = test_save_as_directory("conflict-policy");
        let source_dir = root.join("source");
        let target_dir = root.join("target");
        fs::create_dir_all(&source_dir).expect("测试源目录应能创建");
        fs::create_dir_all(&target_dir).expect("测试目标目录应能创建");
        let source_path = source_dir.join("server.log");
        let target_path = target_dir.join("server.log");
        fs::write(&source_path, "new-content").expect("测试源文件应能写入");
        fs::write(&target_path, "old-content").expect("测试目标文件应能写入");
        let source = LogFileSource::LocalFile {
            path: source_path.clone(),
        };

        let skipped = MainView::save_log_sources_to_directory(
            std::slice::from_ref(&source),
            &target_dir,
            SaveConflictPolicy::SkipExisting,
        );
        assert_eq!(skipped.saved_count, 0);
        assert_eq!(skipped.skipped_count, 1);
        assert_eq!(skipped.failed_count, 0);
        assert_eq!(
            fs::read_to_string(&target_path).expect("跳过后目标文件仍应可读"),
            "old-content"
        );

        let overwritten = MainView::save_log_sources_to_directory(
            &[source],
            &target_dir,
            SaveConflictPolicy::OverwriteExisting,
        );
        assert_eq!(overwritten.saved_count, 1);
        assert_eq!(overwritten.skipped_count, 0);
        assert_eq!(overwritten.failed_count, 0);
        assert_eq!(
            fs::read_to_string(&target_path).expect("覆盖后目标文件仍应可读"),
            "new-content"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// 验证 Java thread dump 会解析出快照和线程状态。
    ///
    /// 业务意图：
    /// - 线程分析窗口依赖解析出的快照矩阵；线程名和状态识别失败会直接导致时间线为空。
    #[test]
    fn java_thread_dump_解析快照和线程状态() {
        let lines = vec![
            "线程日志打印时间：2026-05-07 11:01:10".to_string(),
            "Full thread dump Java HotSpot(TM) 64-Bit Server VM:".to_string(),
            "\"pool-1-thread-1\" #1 prio=5".to_string(),
            "   java.lang.Thread.State: RUNNABLE".to_string(),
            "\"worker\" #2 prio=5".to_string(),
            "   java.lang.Thread.State: WAITING (parking)".to_string(),
            "线程日志打印时间：2026-05-07 11:02:10".to_string(),
            "Full thread dump Java HotSpot(TM) 64-Bit Server VM:".to_string(),
            "\"pool-1-thread-1\" #1 prio=5".to_string(),
            "   java.lang.Thread.State: RUNNABLE".to_string(),
            "\"worker\" #2 prio=5".to_string(),
            "   java.lang.Thread.State: WAITING (parking)".to_string(),
        ];

        let source = LogFileSource::LocalFile {
            path: PathBuf::from("thread.log"),
        };
        let snapshots = parse_thread_dump_snapshots(&lines, "thread.log", 0, &source);
        let analysis = build_thread_analysis_data(1, 0, snapshots, &[]);

        assert_eq!(analysis.snapshots.len(), 2);
        assert_eq!(analysis.snapshots[0].label, "2026-05-07 11:01:10");
        assert_eq!(analysis.thread_names, vec!["pool-1-thread-1", "worker"]);
        let first_cell = analysis.matrix[0][0]
            .as_ref()
            .expect("第一个线程应形成可点击时间线色块");
        assert_eq!(first_cell.state, ThreadStateKind::Runnable);
        assert_eq!(first_cell.thread_id.as_deref(), Some("#1"));
        assert_eq!(first_cell.line_index, 2);
        assert_eq!(first_cell.preview_lines.len(), 2);
        assert_eq!(first_cell.stack_lines.len(), 2);
        assert_eq!(
            analysis.matrix[1][0].as_ref().map(|cell| cell.state),
            Some(ThreadStateKind::Waiting)
        );
        assert_eq!(
            ThreadAnalysisWindowView::visible_thread_indexes_for_state_kinds(
                &analysis,
                &ThreadAnalysisWindowView::default_visible_state_kinds(),
            ),
            vec![0]
        );
    }

    /// 验证线程分析过滤规则按空行拆分，并执行连续片段匹配。
    ///
    /// 业务意图：
    /// - 设置页允许用户直接粘贴多段线程堆栈；解析规则必须忽略多余空白，同时避免只凭单行误过滤其它线程。
    #[test]
    fn 线程分析过滤规则按空行拆分并连续匹配() {
        let rules = MainView::parse_thread_analysis_filter_rules(
            "\"worker\" #1\r\n  at demo.A.run\r\n\r\n\"timer\" #2\n  at demo.Timer.sleep",
        );
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].kind, ThreadAnalysisFilterRuleKind::StackLines);
        assert_eq!(
            rules[0].lines,
            vec!["\"worker\" #1".to_string(), "at demo.A.run".to_string()]
        );

        let stack = vec![
            "\"worker\" #1".to_string(),
            "   java.lang.Thread.State: RUNNABLE".to_string(),
            "  at demo.A.run".to_string(),
        ];
        assert!(!thread_stack_matches_filter_rule(&stack, &rules[0]));

        let consecutive_stack = vec![
            "\"worker\" #1".to_string(),
            "  at demo.A.run".to_string(),
            "  at demo.B.run".to_string(),
        ];
        assert!(thread_stack_matches_filter_rule(
            &consecutive_stack,
            &rules[0]
        ));
    }

    /// 验证堆栈过滤输入框中的单行规则不会被误解析为线程名通配。
    ///
    /// 业务意图：
    /// - 线程名过滤已经拆成独立输入框；堆栈过滤输入框里即使只有一行线程头或方法片段，也必须按堆栈连续片段匹配。
    /// - 这能避免用户只粘贴线程头时规则变成 `ThreadNamePattern` 后被堆栈匹配逻辑忽略。
    #[test]
    fn 线程分析单行堆栈过滤仍保持堆栈规则() {
        let rules = MainView::parse_thread_analysis_filter_rules("\"worker\" #1 prio=5");

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].kind, ThreadAnalysisFilterRuleKind::StackLines);
        assert_eq!(rules[0].lines, vec!["\"worker\" #1 prio=5".to_string()]);
        assert!(thread_stack_matches_filter_rule(
            &[
                "\"worker\" #1 prio=5".to_string(),
                "java.lang.Thread.State: RUNNABLE".to_string(),
            ],
            &rules[0],
        ));
        assert!(!thread_name_matches_filter_rule("worker", &rules[0]));
    }

    /// 验证线程名通配过滤规则支持精确和星号匹配。
    ///
    /// 业务意图：
    /// - 用户可以在设置页直接配置 `C2 CompilerThread*` 这类 JVM 常驻线程名模式，不需要粘贴完整线程堆栈。
    #[test]
    fn 线程分析过滤规则支持线程名通配匹配() {
        let rules = MainView::parse_thread_analysis_name_filter_rules(
            "C2 CompilerThread*\nService Thread, Attach Listener",
        );

        assert_eq!(rules.len(), 3);
        assert_eq!(
            rules[0].kind,
            ThreadAnalysisFilterRuleKind::ThreadNamePattern
        );
        assert!(thread_name_matches_filter_rule(
            "C2 CompilerThread7",
            &rules[0]
        ));
        assert!(!thread_name_matches_filter_rule(
            "C1 CompilerThread7",
            &rules[0]
        ));
        assert!(thread_name_matches_filter_rule("Service Thread", &rules[1]));
        assert!(thread_name_matches_filter_rule(
            "Attach Listener",
            &rules[2]
        ));
        assert!(wildcard_pattern_matches_text(
            "pool-*-thread-*",
            "pool-2-thread-16"
        ));
        assert!(!wildcard_pattern_matches_text(
            "pool-*-worker",
            "pool-2-thread-16"
        ));
    }

    /// 验证线程分析过滤输入区宽度估算会随最长行增长。
    ///
    /// 业务意图：
    /// - 设置页线程名和堆栈输入框都要求在内容超出可视范围时显示横向滚动条；布局阶段需要根据最长行估算子元素宽度。
    /// - 制表符按四列估算，避免含 tab 的堆栈行被低估后右侧内容无法滚动查看。
    #[test]
    fn 线程分析过滤输入区宽度估算随长行增长() {
        let short_width = MainView::estimated_thread_analysis_filter_text_width("short");
        let long_width = MainView::estimated_thread_analysis_filter_text_width(
            "short\nvery-long-thread-name-with-many-columns\tend",
        );

        assert!(long_width > short_width);
    }

    /// 验证内置默认过滤规则会解析为独立线程名规则和两条堆栈规则。
    ///
    /// 业务意图：
    /// - 默认线程名配置和默认堆栈配置分开保存，但分析时会合并成同一组规则；这里锁定合并后的规则类型顺序。
    #[test]
    fn 线程分析默认过滤规则解析为线程名和堆栈规则() {
        let mut rules = MainView::parse_thread_analysis_name_filter_rules(
            DEFAULT_THREAD_ANALYSIS_NAME_FILTER_TEXT,
        );
        rules.extend(MainView::parse_thread_analysis_filter_rules(
            DEFAULT_THREAD_ANALYSIS_FILTER_TEXT,
        ));

        assert_eq!(rules.len(), 6);
        assert_eq!(
            rules[0].kind,
            ThreadAnalysisFilterRuleKind::ThreadNamePattern
        );
        assert_eq!(rules[0].lines[0], "C1 CompilerThread*");
        assert_eq!(rules[1].lines[0], "C2 CompilerThread*");
        assert_eq!(rules[2].lines[0], "Service Thread");
        assert_eq!(rules[3].lines[0], "Attach Listener");
        assert_eq!(rules[4].kind, ThreadAnalysisFilterRuleKind::StackLines);
        assert_eq!(rules[4].lines[0], "java.lang.Thread.State: RUNNABLE");
        assert!(
            rules[4]
                .lines
                .iter()
                .any(|line| line.contains("PlainSocketImpl.socketAccept"))
        );
        assert_eq!(rules[5].kind, ThreadAnalysisFilterRuleKind::StackLines);
        assert!(
            rules[5]
                .lines
                .iter()
                .any(|line| line.contains("SocketInputStream.socketRead0"))
        );
    }

    /// 验证线程分析会把命中过滤堆栈的线程从矩阵中移除。
    ///
    /// 业务意图：
    /// - 无效线程过滤必须发生在线程名聚合和矩阵构建之前，否则被过滤线程仍会占据纵轴或空色块。
    #[test]
    fn 线程分析过滤命中堆栈后移除线程() {
        let lines = vec![
            "Full thread dump Java HotSpot(TM) 64-Bit Server VM:".to_string(),
            "\"noise-thread\" #1 prio=5".to_string(),
            "   java.lang.Thread.State: RUNNABLE".to_string(),
            "        at demo.Noise.loop(Noise.java:10)".to_string(),
            "\"business-thread\" #2 prio=5".to_string(),
            "   java.lang.Thread.State: RUNNABLE".to_string(),
            "        at demo.Business.run(Business.java:20)".to_string(),
            "Full thread dump Java HotSpot(TM) 64-Bit Server VM:".to_string(),
            "\"business-thread\" #2 prio=5".to_string(),
            "   java.lang.Thread.State: RUNNABLE".to_string(),
            "        at demo.Business.run(Business.java:20)".to_string(),
        ];
        let source = LogFileSource::LocalFile {
            path: PathBuf::from("thread.log"),
        };
        let snapshots = parse_thread_dump_snapshots(&lines, "thread.log", 0, &source);
        let rules = MainView::parse_thread_analysis_filter_rules(
            "\"noise-thread\" #1 prio=5\njava.lang.Thread.State: RUNNABLE\nat demo.Noise.loop(Noise.java:10)",
        );

        let analysis = build_thread_analysis_data(1, 0, snapshots, &rules);

        assert_eq!(analysis.thread_names, vec!["business-thread"]);
        assert_eq!(analysis.matrix.len(), 1);
        assert!(analysis.summary.contains("过滤 1 个线程"));
    }

    /// 验证线程分析会把命中线程名通配规则的线程从矩阵中移除。
    ///
    /// 业务意图：
    /// - JVM 编译线程、Service Thread 和 Attach Listener 这类线程通常只靠线程名即可识别，过滤必须发生在线程名聚合之前。
    #[test]
    fn 线程分析过滤命中线程名通配后移除线程() {
        let lines = vec![
            "Full thread dump Java HotSpot(TM) 64-Bit Server VM:".to_string(),
            "\"C2 CompilerThread7\" #1 prio=9".to_string(),
            "   java.lang.Thread.State: RUNNABLE".to_string(),
            "\"business-thread\" #2 prio=5".to_string(),
            "   java.lang.Thread.State: RUNNABLE".to_string(),
            "        at demo.Business.run(Business.java:20)".to_string(),
            "Full thread dump Java HotSpot(TM) 64-Bit Server VM:".to_string(),
            "\"business-thread\" #2 prio=5".to_string(),
            "   java.lang.Thread.State: RUNNABLE".to_string(),
            "        at demo.Business.run(Business.java:20)".to_string(),
        ];
        let source = LogFileSource::LocalFile {
            path: PathBuf::from("thread.log"),
        };
        let snapshots = parse_thread_dump_snapshots(&lines, "thread.log", 0, &source);
        let rules = MainView::parse_thread_analysis_name_filter_rules("C2 CompilerThread*");

        let analysis = build_thread_analysis_data(1, 0, snapshots, &rules);

        assert_eq!(analysis.thread_names, vec!["business-thread"]);
        assert_eq!(analysis.matrix.len(), 1);
        assert!(analysis.summary.contains("过滤 1 个线程"));
    }

    /// 验证线程分析全部线程被过滤时仍保留快照统计。
    ///
    /// 业务意图：
    /// - 用户可能临时过滤掉所有噪声线程；分析窗口应展示 0 个线程而不是误报没有识别到快照。
    #[test]
    fn 线程分析全部线程过滤后保留快照() {
        let lines = vec![
            "Full thread dump Java HotSpot(TM) 64-Bit Server VM:".to_string(),
            "\"noise-thread\" #1 prio=5".to_string(),
            "   java.lang.Thread.State: RUNNABLE".to_string(),
        ];
        let source = LogFileSource::LocalFile {
            path: PathBuf::from("thread.log"),
        };
        let snapshots = parse_thread_dump_snapshots(&lines, "thread.log", 0, &source);
        let rules = MainView::parse_thread_analysis_filter_rules(
            "\"noise-thread\" #1 prio=5\njava.lang.Thread.State: RUNNABLE",
        );

        let analysis = build_thread_analysis_data(1, 0, snapshots, &rules);

        assert_eq!(analysis.snapshots.len(), 1);
        assert!(analysis.thread_names.is_empty());
        assert!(analysis.matrix.is_empty());
        assert!(analysis.summary.contains("1 个快照"));
        assert!(analysis.summary.contains("过滤 1 个线程"));
    }

    /// 验证当前日志正文可以按线程分析同一套规则过滤线程片段。
    ///
    /// 业务意图：
    /// - 工具条“过滤线程”按钮不打开分析窗口，而是直接裁剪当前正文；被裁剪的线程必须同时包含用户配置过滤和默认单次线程过滤。
    /// - 下一个快照的时间戳必须保留，否则用户过滤后无法判断剩余线程属于哪个 thread dump 时间点。
    #[test]
    fn 当前线程日志正文按分析规则过滤线程片段并保留快照边界() {
        let lines = vec![
            "2026-05-07 11:01:10".to_string(),
            "Full thread dump Java HotSpot(TM) 64-Bit Server VM:".to_string(),
            "\"repeat-thread\" #1 prio=5".to_string(),
            "   java.lang.Thread.State: RUNNABLE".to_string(),
            "        at demo.Business.run(Business.java:20)".to_string(),
            "\"noise-thread\" #2 prio=5".to_string(),
            "   java.lang.Thread.State: RUNNABLE".to_string(),
            "        at demo.Noise.loop(Noise.java:10)".to_string(),
            "2026-05-07 11:02:10".to_string(),
            "Full thread dump Java HotSpot(TM) 64-Bit Server VM:".to_string(),
            "\"repeat-thread\" #1 prio=5".to_string(),
            "   java.lang.Thread.State: WAITING (parking)".to_string(),
            "        at demo.Business.wait(Business.java:30)".to_string(),
            "\"once-thread\" #3 prio=5".to_string(),
            "   java.lang.Thread.State: RUNNABLE".to_string(),
            "        at demo.Once.run(Once.java:40)".to_string(),
        ];
        let source = LogFileSource::LocalFile {
            path: PathBuf::from("thread.log"),
        };
        let rules = MainView::parse_thread_analysis_name_filter_rules("noise-*");

        let result =
            filter_thread_dump_lines_with_analysis_rules(&lines, "thread.log", &source, &rules)
                .expect("Java thread dump 应生成正文过滤结果");

        assert_eq!(result.snapshot_count, 2);
        assert_eq!(result.hidden_thread_count, 2);
        assert_eq!(result.retained_thread_count, 2);
        assert!(
            result
                .filtered_lines
                .iter()
                .any(|line| line == "2026-05-07 11:02:10"),
            "过滤上一条线程时不能误删下一个快照时间戳"
        );
        assert!(
            result
                .filtered_lines
                .iter()
                .all(|line| !line.contains("noise-thread") && !line.contains("once-thread"))
        );
        assert_eq!(
            result
                .filtered_lines
                .iter()
                .filter(|line| line.contains("\"repeat-thread\""))
                .count(),
            2
        );
    }

    /// 验证线程过滤按钮可用性使用真实 Java thread dump 解析结果判断。
    ///
    /// 业务意图：
    /// - 普通日志中可能包含单词 `thread` 或 Java 异常堆栈，但没有完整 thread dump 时不应展示过滤按钮。
    #[test]
    fn 线程日志识别要求存在可解析快照和线程状态() {
        let source = LogFileSource::LocalFile {
            path: PathBuf::from("thread.log"),
        };
        let thread_lines = vec![
            "Full thread dump Java HotSpot(TM) 64-Bit Server VM:".to_string(),
            "\"worker\" #1 prio=5".to_string(),
            "   java.lang.Thread.State: RUNNABLE".to_string(),
        ];
        let normal_lines = vec![
            "2026-05-07 INFO thread pool started".to_string(),
            "at demo.Worker.run(Worker.java:10)".to_string(),
        ];

        assert!(has_java_thread_dump_snapshots(
            &thread_lines,
            "thread.log",
            &source
        ));
        assert!(!has_java_thread_dump_snapshots(
            &normal_lines,
            "app.log",
            &source
        ));
    }

    /// 验证线程分析默认隐藏在选中日志中只出现一次的线程。
    ///
    /// 业务意图：
    /// - 单次出现的线程通常是短暂任务或噪声；默认隐藏可以减少时间线纵轴数量，突出重复出现的线程。
    /// - 该规则不再依赖线程是否跨文件，只要同一线程在选中日志快照中出现超过一次就保留。
    #[test]
    fn 线程分析默认隐藏只出现一次的线程() {
        let sample = |name: &str, state: ThreadStateKind| ThreadStateSample {
            name: name.to_string(),
            thread_id: None,
            state,
            line_index: 0,
            preview_lines: vec![format!("\"{name}\"")],
            stack_lines: vec![format!("\"{name}\"")],
        };
        let source = LogFileSource::LocalFile {
            path: PathBuf::from("thread.log"),
        };
        let snapshots = vec![
            ThreadSnapshot {
                label: "第一个快照".to_string(),
                source_index: 0,
                source: source.clone(),
                threads: vec![
                    sample("repeat-thread", ThreadStateKind::Runnable),
                    sample("once-thread", ThreadStateKind::Runnable),
                ],
            },
            ThreadSnapshot {
                label: "第二个快照".to_string(),
                source_index: 0,
                source,
                threads: vec![sample("repeat-thread", ThreadStateKind::Waiting)],
            },
        ];

        let analysis = build_thread_analysis_data(1, 0, snapshots, &[]);

        assert_eq!(analysis.thread_names, vec!["repeat-thread"]);
        assert_eq!(analysis.matrix.len(), 1);
        assert!(analysis.summary.contains("过滤 1 个线程"));
    }

    /// 验证线程分析结果按线程命中次数从高到低排序。
    ///
    /// 业务意图：
    /// - 高频出现的线程更可能是排障入口，结果列表应把这类线程放在上方，而不是只按首次出现顺序排列。
    /// - 命中次数相同时仍保持首次出现顺序，避免相同数据在重复解析时排序抖动。
    #[test]
    fn 线程分析结果按命中次数降序排序() {
        let sample = |name: &str, state: ThreadStateKind| ThreadStateSample {
            name: name.to_string(),
            thread_id: None,
            state,
            line_index: 0,
            preview_lines: vec![format!("\"{name}\"")],
            stack_lines: vec![format!("\"{name}\"")],
        };
        let source = LogFileSource::LocalFile {
            path: PathBuf::from("thread.log"),
        };
        let snapshots = vec![
            ThreadSnapshot {
                label: "第一个快照".to_string(),
                source_index: 0,
                source: source.clone(),
                threads: vec![
                    sample("warm-thread", ThreadStateKind::Runnable),
                    sample("hot-thread", ThreadStateKind::Runnable),
                ],
            },
            ThreadSnapshot {
                label: "第二个快照".to_string(),
                source_index: 0,
                source: source.clone(),
                threads: vec![
                    sample("warm-thread", ThreadStateKind::Waiting),
                    sample("hot-thread", ThreadStateKind::Runnable),
                ],
            },
            ThreadSnapshot {
                label: "第三个快照".to_string(),
                source_index: 0,
                source,
                threads: vec![sample("hot-thread", ThreadStateKind::Blocked)],
            },
        ];

        let analysis = build_thread_analysis_data(1, 0, snapshots, &[]);

        assert_eq!(analysis.thread_names, vec!["hot-thread", "warm-thread"]);
        assert_eq!(
            analysis.matrix[0][2]
                .as_ref()
                .map(|cell| cell.thread_name.as_str()),
            Some("hot-thread")
        );
    }

    /// 验证线程分析窗口按当前可见状态的命中次数排序。
    ///
    /// 业务意图：
    /// - 数据矩阵会按所有状态的总命中次数排序，但窗口默认只显示 RUNNABLE 状态。
    /// - 当用户只看 RUNNABLE 时，行顺序应按绿色色块数量从高到低排列，隐藏的 WAITING/BLOCKED 命中不能把低频可见线程顶到前面。
    /// - 当前状态下只有一个可见命中的线程也应隐藏，避免单次 RUNNABLE 噪声继续出现在默认结果里。
    #[test]
    fn 线程分析可见行按当前状态命中次数降序排序() {
        let sample = |name: &str, state: ThreadStateKind| ThreadStateSample {
            name: name.to_string(),
            thread_id: None,
            state,
            line_index: 0,
            preview_lines: vec![format!("\"{name}\"")],
            stack_lines: vec![format!("\"{name}\"")],
        };
        let source = LogFileSource::LocalFile {
            path: PathBuf::from("thread.log"),
        };
        let snapshots = vec![
            ThreadSnapshot {
                label: "第一个快照".to_string(),
                source_index: 0,
                source: source.clone(),
                threads: vec![
                    sample("mostly-waiting", ThreadStateKind::Waiting),
                    sample("hot-runnable", ThreadStateKind::Runnable),
                ],
            },
            ThreadSnapshot {
                label: "第二个快照".to_string(),
                source_index: 0,
                source: source.clone(),
                threads: vec![
                    sample("mostly-waiting", ThreadStateKind::Waiting),
                    sample("hot-runnable", ThreadStateKind::Runnable),
                    sample("warm-runnable", ThreadStateKind::Runnable),
                ],
            },
            ThreadSnapshot {
                label: "第三个快照".to_string(),
                source_index: 0,
                source,
                threads: vec![
                    sample("mostly-waiting", ThreadStateKind::Runnable),
                    sample("warm-runnable", ThreadStateKind::Waiting),
                ],
            },
        ];

        let analysis = build_thread_analysis_data(1, 0, snapshots, &[]);

        assert_eq!(
            analysis.thread_names,
            vec!["mostly-waiting", "hot-runnable", "warm-runnable"]
        );
        let visible_thread_names =
            ThreadAnalysisWindowView::visible_thread_indexes_for_state_kinds(
                &analysis,
                &ThreadAnalysisWindowView::default_visible_state_kinds(),
            )
            .into_iter()
            .map(|thread_index| analysis.thread_names[thread_index].as_str())
            .collect::<Vec<_>>();

        assert_eq!(visible_thread_names, vec!["hot-runnable"]);
    }

    /// 验证线程分析进度比例和文案在边界条件下稳定。
    ///
    /// 业务意图：
    /// - 解析窗口进度条直接消费该进度快照；空文件、完成态和百分比都不能出现除零或越界。
    #[test]
    fn 线程分析进度比例和文案稳定() {
        let empty_progress = ThreadAnalysisProgress::new(0);
        assert_eq!(empty_progress.ratio(), 1.0);

        let mut progress = ThreadAnalysisProgress::new(4);
        progress.processed_files = 2;
        progress.parsed_snapshots = 3;
        progress.parsed_threads = 12;
        progress.current_file = Some("thread.log".to_string());

        assert_eq!(progress.ratio(), 0.5);
        assert!(progress.label().contains("2 / 4 文件"));
        assert!(progress.message().contains("thread.log"));
    }

    /// 验证多文件线程分析默认隐藏只在单个日志中出现的线程。
    ///
    /// 业务意图：
    /// - 用户希望默认关注跨线程日志持续出现的线程，单文件独有线程会增加纵轴噪声，应默认过滤掉。
    #[test]
    fn 多文件线程分析默认隐藏单日志独有线程() {
        let snapshots = vec![
            ThreadSnapshot {
                label: "2026-05-07 11:00:00".to_string(),
                source_index: 0,
                source: LogFileSource::LocalFile {
                    path: PathBuf::from("a.log"),
                },
                threads: vec![
                    ThreadStateSample {
                        name: "shared-thread".to_string(),
                        thread_id: Some("#1".to_string()),
                        state: ThreadStateKind::Runnable,
                        line_index: 0,
                        preview_lines: vec!["\"shared-thread\" #1".to_string()],
                        stack_lines: vec!["\"shared-thread\" #1".to_string()],
                    },
                    ThreadStateSample {
                        name: "only-a".to_string(),
                        thread_id: Some("#2".to_string()),
                        state: ThreadStateKind::Waiting,
                        line_index: 1,
                        preview_lines: vec!["\"only-a\" #2".to_string()],
                        stack_lines: vec!["\"only-a\" #2".to_string()],
                    },
                ],
            },
            ThreadSnapshot {
                label: "2026-05-07 11:01:00".to_string(),
                source_index: 1,
                source: LogFileSource::LocalFile {
                    path: PathBuf::from("b.log"),
                },
                threads: vec![
                    ThreadStateSample {
                        name: "shared-thread".to_string(),
                        thread_id: Some("#1".to_string()),
                        state: ThreadStateKind::Blocked,
                        line_index: 0,
                        preview_lines: vec!["\"shared-thread\" #1".to_string()],
                        stack_lines: vec!["\"shared-thread\" #1".to_string()],
                    },
                    ThreadStateSample {
                        name: "only-b".to_string(),
                        thread_id: Some("#3".to_string()),
                        state: ThreadStateKind::Runnable,
                        line_index: 1,
                        preview_lines: vec!["\"only-b\" #3".to_string()],
                        stack_lines: vec!["\"only-b\" #3".to_string()],
                    },
                ],
            },
        ];

        let analysis = build_thread_analysis_data(2, 0, snapshots, &[]);

        assert_eq!(analysis.thread_names, vec!["shared-thread"]);
        assert_eq!(
            analysis.matrix[0][0].as_ref().map(|cell| cell.state),
            Some(ThreadStateKind::Runnable)
        );
        assert_eq!(
            analysis.matrix[0][1].as_ref().map(|cell| cell.state),
            Some(ThreadStateKind::Blocked)
        );
    }

    /// 验证多文件线程分析不要求线程在相邻日志中连续出现。
    ///
    /// 业务意图：
    /// - 用户要求去除“连续”限制；只要线程出现在多个选中的线程日志文件中，就应显示在分析结果里。
    /// - 中间某个选中文件缺少该线程时，时间线对应单元格保持为空，但不应把整条线程过滤掉。
    #[test]
    fn 多文件线程分析重复线程不要求相邻文件连续出现() {
        let sample = |name: &str, state: ThreadStateKind| ThreadStateSample {
            name: name.to_string(),
            thread_id: None,
            state,
            line_index: 0,
            preview_lines: vec![format!("\"{name}\"")],
            stack_lines: vec![format!("\"{name}\"")],
        };
        let source = |name: &str| LogFileSource::LocalFile {
            path: PathBuf::from(name),
        };
        let snapshots = vec![
            ThreadSnapshot {
                label: "第一个文件".to_string(),
                source_index: 0,
                source: source("a.log"),
                threads: vec![sample("repeat-thread", ThreadStateKind::Runnable)],
            },
            ThreadSnapshot {
                label: "第二个文件".to_string(),
                source_index: 1,
                source: source("b.log"),
                threads: vec![sample("only-middle", ThreadStateKind::Waiting)],
            },
            ThreadSnapshot {
                label: "第三个文件".to_string(),
                source_index: 2,
                source: source("c.log"),
                threads: vec![sample("repeat-thread", ThreadStateKind::Blocked)],
            },
        ];

        let analysis = build_thread_analysis_data(3, 0, snapshots, &[]);

        assert_eq!(analysis.thread_names, vec!["repeat-thread"]);
        assert_eq!(
            analysis.matrix[0][0].as_ref().map(|cell| cell.state),
            Some(ThreadStateKind::Runnable)
        );
        assert!(
            analysis.matrix[0][1].is_none(),
            "中间文件缺少该线程时应保留空单元格，而不是过滤整条线程"
        );
        assert_eq!(
            analysis.matrix[0][2].as_ref().map(|cell| cell.state),
            Some(ThreadStateKind::Blocked)
        );
    }

    /// 验证线程分析色块单击即可打开堆栈详情。
    ///
    /// 业务意图：
    /// - 线程色块点击不再自动跳转主日志窗口；左键单击只负责打开堆栈详情窗口。
    /// - 双击会产生第二次鼠标按下事件；继续允许多击更新详情窗口，保证用户沿用旧双击习惯时不会失效。
    #[test]
    fn 线程分析色块单击即可打开堆栈详情() {
        assert!(!ThreadAnalysisWindowView::timeline_cell_click_should_open_stack_window(0));
        assert!(ThreadAnalysisWindowView::timeline_cell_click_should_open_stack_window(1));
        assert!(ThreadAnalysisWindowView::timeline_cell_click_should_open_stack_window(2));
    }

    /// 验证线程堆栈详情窗口只在当前线程的堆栈样本之间切换。
    ///
    /// 业务意图：
    /// - 点击某个线程色块后，左右按钮应沿同一线程在选中日志中的快照顺序切换，不能跳到其它线程。
    /// - 详情窗口展示完整堆栈，应优先使用解析阶段保存的 `stack_lines`。
    #[test]
    fn 线程堆栈详情按当前线程收集可切换堆栈() {
        let source = LogFileSource::LocalFile {
            path: PathBuf::from("thread.log"),
        };
        let sample = |name: &str, line_index: usize| ThreadStateSample {
            name: name.to_string(),
            thread_id: Some(format!("#{line_index}")),
            state: ThreadStateKind::Runnable,
            line_index,
            preview_lines: vec![format!("\"{name}\" #{line_index}")],
            stack_lines: vec![
                format!("\"{name}\" #{line_index}"),
                format!("        at demo.Worker.run({line_index})"),
            ],
        };
        let snapshots = vec![
            ThreadSnapshot {
                label: "第一个快照".to_string(),
                source_index: 0,
                source: source.clone(),
                threads: vec![sample("repeat-thread", 10), sample("other-thread", 11)],
            },
            ThreadSnapshot {
                label: "第二个快照".to_string(),
                source_index: 0,
                source: source.clone(),
                threads: vec![sample("repeat-thread", 20)],
            },
            ThreadSnapshot {
                label: "第三个快照".to_string(),
                source_index: 0,
                source,
                threads: vec![sample("repeat-thread", 30)],
            },
        ];
        let analysis = build_thread_analysis_data(1, 0, snapshots, &[]);
        let clicked_cell = analysis.matrix[0][1]
            .as_ref()
            .expect("第二个快照的重复线程应形成可点击色块")
            .clone();

        let (stacks, active_index) =
            ThreadAnalysisWindowView::thread_stack_cells_for_clicked_cell(&analysis, &clicked_cell);

        assert_eq!(stacks.len(), 3);
        assert_eq!(active_index, 1);
        assert!(
            stacks
                .iter()
                .all(|cell| cell.thread_name == "repeat-thread")
        );
        assert_eq!(
            ThreadStackWindowView::thread_stack_lines_for_cell(&clicked_cell)[1],
            "        at demo.Worker.run(20)"
        );
        assert_eq!(
            ThreadStackWindowView::clamped_active_index(&stacks, usize::MAX),
            2
        );
    }

    /// 验证线程堆栈详情按命中线程的日志文件数计算行出现率。
    ///
    /// 业务意图：
    /// - 详情窗口每行后的小百分比用于区分共性栈帧和少数文件才出现的差异栈帧。
    /// - 分母应是命中当前线程的不同日志来源数；同一来源内动态线程号和 `0x...` 地址变化不应降低语义相同行的出现率。
    #[test]
    fn 线程堆栈详情行出现率按命中文件数计算() {
        let stack =
            |file_name: &str, thread_id: &str, lock_address: &str, extra_line: Option<&str>| {
                let mut stack_lines = vec![
                    format!("\"Thread-342\" {thread_id} daemon tid={lock_address} runnable"),
                    "   java.lang.Thread.State: RUNNABLE".to_string(),
                    "        at demo.Worker.run(Worker.java:20)".to_string(),
                    format!("        - locked <{lock_address}> (a java.lang.Object)"),
                ];
                if let Some(extra_line) = extra_line {
                    stack_lines.push(extra_line.to_string());
                }
                std::sync::Arc::new(ThreadTimelineCell {
                    state: ThreadStateKind::Runnable,
                    time_label: file_name.to_string(),
                    thread_name: "Thread-342".to_string(),
                    thread_id: Some(thread_id.to_string()),
                    source: LogFileSource::LocalFile {
                        path: PathBuf::from(file_name),
                    },
                    line_index: 0,
                    preview_lines: stack_lines.iter().take(5).cloned().collect(),
                    stack_lines,
                })
            };
        let stacks = vec![
            stack(
                "a.log",
                "#1",
                "0x000000001111",
                Some("        at demo.OnlyA.run(OnlyA.java:1)"),
            ),
            stack("b.log", "#2", "0x000000002222", None),
            stack("c.log", "#3", "0x000000003333", None),
        ];

        let display_lines =
            ThreadStackWindowView::thread_stack_display_lines_for_active_stack(&stacks, 0);

        assert_eq!(display_lines[0].presence_percent, 100);
        assert_eq!(display_lines[2].presence_percent, 100);
        assert_eq!(display_lines[3].presence_percent, 100);
        assert_eq!(display_lines[4].presence_percent, 33);
        assert_eq!(
            ThreadStackWindowView::normalized_stack_line_presence_key(
                "        - locked <0x00000000ffff> (a java.lang.Object)",
                "Thread-342",
            ),
            "- locked <0x*> (a java.lang.Object)"
        );
    }

    /// 验证线程堆栈详情会为长行估算横向内容宽度。
    ///
    /// 业务意图：
    /// - 详情窗口使用虚拟列表渲染堆栈，如果最长行暂时不在可视范围，仍需要提前撑出横向滚动范围，保证横向滚动条稳定显示。
    /// - 制表符按四列展开，避免包含 tab 的堆栈行低估宽度后右侧内容无法通过滚动条查看。
    #[test]
    fn 线程堆栈详情长行会撑出横向滚动范围() {
        let short_lines = vec!["at a.B.run(B.java:1)".to_string()];
        let long_lines = vec![
            "at a.B.run(B.java:1)".to_string(),
            "        at oracle.jdbc.driver.T4CConnection.doCommit(T4CConnection.java:961)"
                .to_string(),
        ];

        assert_eq!(
            ThreadStackWindowView::stack_line_display_columns("ab\tc"),
            5
        );
        assert!(
            ThreadStackWindowView::estimated_stack_content_width(&long_lines)
                > ThreadStackWindowView::estimated_stack_content_width(&short_lines)
        );
    }

    /// 验证线程堆栈详情选区复制按真实文本行截取。
    ///
    /// 业务意图：
    /// - 详情窗口每行还会显示出现率标签，但复制应只包含原始堆栈文本，不能把 UI 辅助百分比混入诊断内容。
    /// - 跨行选择需要保留换行，并按字符列截取中文内容，避免 UTF-8 边界错误。
    #[test]
    fn 线程堆栈详情选区复制只包含原始堆栈文本() {
        let lines = vec![
            "\"线程-A\" runnable".to_string(),
            "    at demo.Worker.run(Worker.java:1)".to_string(),
            "    at demo.End.run(End.java:2)".to_string(),
        ];
        let selection = LogTextSelection {
            anchor: LogTextPosition {
                line_index: 0,
                column: 1,
            },
            focus: LogTextPosition {
                line_index: 1,
                column: 11,
            },
        };

        assert_eq!(
            ThreadStackWindowView::selected_stack_text_from_lines(&lines, &selection),
            Some("线程-A\" runnable\n    at demo".to_string())
        );
    }

    /// 验证线程堆栈详情全选范围覆盖当前全部正文行。
    ///
    /// 业务意图：
    /// - `Ctrl/Cmd+A` 是复制完整线程堆栈的高频入口，范围必须从第一行第 0 列覆盖到最后一行末尾。
    /// - 空堆栈行集合不应生成选择，避免窗口空状态下快捷键制造无意义选区。
    #[test]
    fn 线程堆栈详情全选覆盖全部正文行() {
        let lines = vec!["first".to_string(), "".to_string(), "最后".to_string()];
        let selection = ThreadStackWindowView::full_stack_selection_for_lines(&lines)
            .expect("非空堆栈应能构造全选范围");

        assert_eq!(
            selection.anchor,
            LogTextPosition {
                line_index: 0,
                column: 0,
            }
        );
        assert_eq!(
            selection.focus,
            LogTextPosition {
                line_index: 2,
                column: 2,
            }
        );
        assert!(ThreadStackWindowView::full_stack_selection_for_lines(&[]).is_none());
    }

    /// 验证线程分析详情选中色不会和任一状态色冲突。
    ///
    /// 业务意图：
    /// - 点击打开详情后的色块会覆盖原状态色；如果强调色和某个状态色相同，用户无法区分“当前详情目标”和“线程状态”。
    /// - 明暗主题下 OTHER 状态颜色不同，因此两个主题都需要覆盖。
    #[test]
    fn 线程分析详情选中色不与状态色冲突() {
        let states = [
            ThreadStateKind::Runnable,
            ThreadStateKind::Blocked,
            ThreadStateKind::Waiting,
            ThreadStateKind::TimedWaiting,
            ThreadStateKind::New,
            ThreadStateKind::Terminated,
            ThreadStateKind::Other,
        ];

        for theme in [EffectiveTheme::Light, EffectiveTheme::Dark] {
            for state in states {
                assert_ne!(
                    ThreadAnalysisWindowView::timeline_cell_fill_color(state, true, theme),
                    ThreadAnalysisWindowView::timeline_cell_fill_color(state, false, theme),
                    "详情选中色不能和状态 {:?} 在 {:?} 主题下的颜色相同",
                    state,
                    theme
                );
            }
        }
    }

    /// 验证线程分析线程名列只扩展到线程名内容所需宽度。
    ///
    /// 业务意图：
    /// - 线程日志分析结果中快照数量较少时，右侧时间线只占用少量宽度；此时长线程名不应仍被固定 260px 截断。
    /// - 线程名列只需要显示完整线程名，不应把时间线右侧所有空白都吞掉，否则用户会误以为还有额外内容列。
    #[test]
    fn 线程分析线程名列只扩展到内容宽度() {
        let short_required_width =
            ThreadAnalysisWindowView::thread_analysis_name_column_required_width("SubThread");
        let short_width =
            ThreadAnalysisWindowView::thread_analysis_name_column_width_for_required_width(
                short_required_width,
            );
        assert_eq!(short_width, THREAD_ANALYSIS_NAME_COLUMN_WIDTH);

        let long_name = "CACHE_REINIT_weaver.hrm.resource.ResourceComInfo_1779295169625";
        let long_required_width =
            ThreadAnalysisWindowView::thread_analysis_name_column_required_width(long_name);
        let expanded_width =
            ThreadAnalysisWindowView::thread_analysis_name_column_width_for_required_width(
                long_required_width,
            );
        assert_eq!(expanded_width, long_required_width);
        assert!(expanded_width > THREAD_ANALYSIS_NAME_COLUMN_WIDTH);

        let viewport_remaining_width = 1200.0 - 13.0 * THREAD_ANALYSIS_SNAPSHOT_COLUMN_WIDTH;
        assert!(
            expanded_width < viewport_remaining_width,
            "线程名列不应为了填满容器而扩展到右侧剩余空白"
        );
    }

    /// 验证线程分析气泡在窗口右下角会自动改为向左上方弹出。
    ///
    /// 业务意图：
    /// - 用户悬浮在靠近窗口边缘的状态色块时，气泡不能被窗口裁切，否则关键线程 ID 和预览日志不可见。
    #[test]
    fn 线程分析气泡靠近边缘时反向弹出() {
        let (x, y) = ThreadAnalysisWindowView::thread_analysis_popup_origin(
            1000.0,
            680.0,
            THREAD_ANALYSIS_WINDOW_WIDTH,
            THREAD_ANALYSIS_WINDOW_HEIGHT,
        );

        assert!(f32::from(x) < 1000.0);
        assert!(f32::from(y) < 680.0);
        assert!(
            f32::from(x) + THREAD_ANALYSIS_POPUP_WIDTH + THREAD_ANALYSIS_POPUP_MARGIN
                <= THREAD_ANALYSIS_WINDOW_WIDTH
        );
        assert!(
            f32::from(y) + THREAD_ANALYSIS_POPUP_ESTIMATED_HEIGHT + THREAD_ANALYSIS_POPUP_MARGIN
                <= THREAD_ANALYSIS_WINDOW_HEIGHT
        );
    }

    /// 验证线程分析气泡在普通位置默认向右下方弹出。
    ///
    /// 业务意图：
    /// - 非边缘区域保留靠近悬浮点的默认方向，让用户能直接把气泡和当前悬浮的色块关联起来。
    #[test]
    fn 线程分析气泡普通位置向右下弹出() {
        let (x, y) = ThreadAnalysisWindowView::thread_analysis_popup_origin(
            120.0,
            120.0,
            THREAD_ANALYSIS_WINDOW_WIDTH,
            THREAD_ANALYSIS_WINDOW_HEIGHT,
        );

        assert_eq!(f32::from(x), 120.0 + THREAD_ANALYSIS_POPUP_OFFSET);
        assert_eq!(f32::from(y), 120.0 + THREAD_ANALYSIS_POPUP_OFFSET);
    }

    /// 验证日志选区的字节范围不会截断中文字符。
    ///
    /// 业务意图：
    /// - 只读复制和选区高亮都依赖 `selected_byte_range_for_line`，如果该函数返回非 UTF-8 边界会在渲染或复制时崩溃。
    #[test]
    fn 日志选区范围保持_utf8_边界() {
        let line = "abc中文def";
        let selection = LogTextSelection {
            anchor: LogTextPosition {
                line_index: 0,
                column: 3,
            },
            focus: LogTextPosition {
                line_index: 0,
                column: 5,
            },
        };

        let range = MainView::selected_byte_range_for_line(&selection, 0, line)
            .expect("中文选区应返回有效范围");
        assert_eq!(&line[range], "中文");
    }

    /// 验证反向拖选时仍能按文档顺序生成选区范围。
    ///
    /// 业务意图：
    /// - 用户可以从右往左拖选，复制和高亮必须使用规范化后的范围，而不是假设鼠标按下位置永远在前。
    #[test]
    fn 日志反向选区会规范化() {
        let line = "abcdef";
        let selection = LogTextSelection {
            anchor: LogTextPosition {
                line_index: 0,
                column: 5,
            },
            focus: LogTextPosition {
                line_index: 0,
                column: 2,
            },
        };

        let range = MainView::selected_byte_range_for_line(&selection, 0, line)
            .expect("反向选区应返回有效范围");
        assert_eq!(&line[range], "cde");
    }

    /// 验证行首空格不会影响选区列到真实文本的映射。
    ///
    /// 业务意图：
    /// - Java 堆栈、XML 缩进和 properties 续行经常包含行首空格；用户按视觉位置选择正文时，
    ///   高亮和复制必须保留这些缩进后的真实文本列。
    #[test]
    fn 日志选区支持行首空格() {
        let line = "        at java.lang.Thread.run(Thread.java:748)";
        let selection = LogTextSelection {
            anchor: LogTextPosition {
                line_index: 0,
                column: 11,
            },
            focus: LogTextPosition {
                line_index: 0,
                column: 20,
            },
        };

        let range = MainView::selected_byte_range_for_line(&selection, 0, line)
            .expect("带行首空格的选区应返回有效范围");
        assert_eq!(&line[range], "java.lang");
    }

    /// 验证双击日志正文时会选中当前日志 token。
    ///
    /// 业务意图：
    /// - 日志中的 Java 包名和方法名会被 `.`、`(`、`)` 等符号串联；双击时这些符号都应作为边界。
    /// - 选区只包含两个符号之间的当前单词，避免把整段调用链误选为一句。
    #[test]
    fn 双击日志正文会选中当前_token() {
        let line = "        at java.lang.Thread.run(Thread.java:748)";
        let token_start = line.find("Thread").expect("测试行应包含 Thread");
        let selection = MainView::word_selection_for_position(
            0,
            line,
            LogTextPosition {
                line_index: 0,
                column: line[..token_start].chars().count() + 2,
            },
        )
        .expect("双击类名中间应选中完整 token");

        let range = MainView::selected_byte_range_for_line(&selection, 0, line)
            .expect("双击 token 选区应返回有效范围");
        assert_eq!(&line[range], "Thread");
    }

    /// 验证 Java 堆栈中的点号和括号会截断双击选词。
    ///
    /// 业务意图：
    /// - 用户在堆栈行中双击 `AESCipher` 或 `engineDoFinal` 时，目标通常是复制当前类名或方法名。
    /// - 点号和括号属于符号边界，不能把 `com.sun...` 整段包名或 `(AESCipher.java:491)` 一起选中。
    #[test]
    fn 双击_java_堆栈只选两个符号之间的单词() {
        let line = "    at com.sun.crypto.provider.AESCipher.engineDoFinal(AESCipher.java:491)";
        let token_start = line.find("AESCipher").expect("测试行应包含 AESCipher");
        let selection = MainView::word_selection_for_position(
            0,
            line,
            LogTextPosition {
                line_index: 0,
                column: line[..token_start].chars().count() + 2,
            },
        )
        .expect("双击类名中间应选中类名");

        let range = MainView::selected_byte_range_for_line(&selection, 0, line)
            .expect("Java 堆栈选区应返回有效范围");
        assert_eq!(&line[range], "AESCipher");
    }

    /// 验证双击命中单词右侧符号时会回退到左侧单词。
    ///
    /// 边界条件：
    /// - GPUI 文本命中测试在双击单词右边缘时可能返回后一个 `.` 的列；这里需要选中左侧单词，而不是空选。
    #[test]
    fn 双击命中右侧点号会回退选中左侧单词() {
        let line = "AESCipher.engineDoFinal";
        let selection = MainView::word_selection_for_position(
            0,
            line,
            LogTextPosition {
                line_index: 0,
                column: "AESCipher".chars().count(),
            },
        )
        .expect("双击单词右边缘命中点号时应回退选中左侧单词");

        let range = MainView::selected_byte_range_for_line(&selection, 0, line)
            .expect("右边界回退选区应返回有效范围");
        assert_eq!(&line[range], "AESCipher");
    }

    /// 验证双击在结构分隔符上不会误选相邻内容。
    ///
    /// 边界条件：
    /// - `=`、括号和逗号等字符在日志中用于分隔字段；双击这些位置时退化为空选择更符合用户预期。
    #[test]
    fn 双击结构分隔符不会误选_token() {
        let line = "status=OK";

        assert!(
            MainView::word_selection_for_position(
                0,
                line,
                LogTextPosition {
                    line_index: 0,
                    column: 6,
                },
            )
            .is_none()
        );
    }

    /// 验证三连击日志正文时会选中整行。
    ///
    /// 业务意图：
    /// - 排障时经常需要复制当前完整日志行，三连击应保留行首缩进和行尾内容，不能只复制可见 token。
    #[test]
    fn 三连击日志正文会选中整行() {
        let line = "  ERROR failed to start  ";
        let selection = MainView::line_selection_for_line(0, line);

        let range = MainView::selected_byte_range_for_line(&selection, 0, line)
            .expect("三连击整行选区应返回有效范围");
        assert_eq!(&line[range], line);
    }

    /// 验证 GPUI shaping 返回的字节下标可以安全转换为字符列。
    ///
    /// 边界条件：
    /// - 即使外部传入的字节下标落在中文字符中间，也必须回退到前一个字符边界，避免选区状态保存非法列。
    #[test]
    fn 字节下标转换字符列会回退到_utf8_边界() {
        let text = "  中abc";
        assert_eq!(MainView::char_column_for_byte_index(text, 2), 2);
        assert_eq!(MainView::char_column_for_byte_index(text, 3), 2);
        assert_eq!(MainView::char_column_for_byte_index(text, 5), 3);
    }

    /// 验证日志正文显示层会按固定 4 列 tab stop 展开制表符。
    ///
    /// 业务意图：
    /// - 用户反馈用 tab 分隔的日志列在当前查看器里贴在一起，显示层必须模拟常见编辑器的 tab stop 行为。
    /// - 这里验证不是简单替换成固定 4 个空格，而是根据当前显示列补齐到下一个 4 列边界。
    #[test]
    fn 日志显示层会按四列展开制表符() {
        let expanded = MainView::expanded_log_line_for_display("a\tb\tc");

        assert_eq!(expanded.text, "a   b   c");
    }

    /// 验证 tab 展开后原始高亮范围会同步映射到显示文本范围。
    ///
    /// 业务意图：
    /// - 搜索命中、语法高亮和选区高亮都基于原始日志文本计算；tab 展开后必须平移范围，否则高亮会落在错误字符上。
    #[test]
    fn 日志高亮范围会映射到制表符展开后的文本() {
        let expanded = MainView::expanded_log_line_for_display("a\tERROR");
        let highlights =
            MainView::map_log_highlights_to_display(vec![(2..7, Default::default())], &expanded);

        assert_eq!(expanded.text, "a   ERROR");
        assert_eq!(highlights[0].0, 4..9);
    }

    /// 验证显示文本命中位置可以回到原始日志字节位置。
    ///
    /// 业务意图：
    /// - 鼠标选择发生在展开后的可见文本上，但复制和选区状态必须指向原始日志字符列，才能保留真实 `\t`。
    #[test]
    fn 制表符展开后的命中位置会映射回原始文本() {
        let expanded = MainView::expanded_log_line_for_display("a\tb");

        assert_eq!(
            MainView::original_byte_index_for_display_byte(&expanded, 0),
            0
        );
        assert_eq!(
            MainView::original_byte_index_for_display_byte(&expanded, 2),
            1
        );
        assert_eq!(
            MainView::original_byte_index_for_display_byte(&expanded, 4),
            2
        );
    }

    /// 验证搜索结果预览会去掉命中行前后空白。
    ///
    /// 业务意图：
    /// - 结果面板展示的是摘要预览，不应让缩进或行尾空白占据首屏空间；但高亮范围必须同步平移，仍准确标记命中词。
    #[test]
    fn 搜索结果预览去除前后空白并平移高亮范围() {
        let line = "    ERROR failed to start   ";
        let match_range = 4..9;

        let (preview, range) = MainView::search_result_preview_text_and_range(line, match_range);

        assert_eq!(preview, "ERROR failed to start");
        assert_eq!(range, 0..5);
        assert_eq!(&preview[range], "ERROR");
    }

    /// 验证搜索结果预览裁剪 Unicode 空白时仍保持 UTF-8 边界。
    ///
    /// 边界条件：
    /// - 日志可能包含全角空格或不间断空格，展示裁剪不能把中文命中范围切坏。
    #[test]
    fn 搜索结果预览支持_unicode_空白() {
        let line = "\u{3000}\u{3000}启动成功\u{00a0}";
        let match_range = 6..12;

        let (preview, range) = MainView::search_result_preview_text_and_range(line, match_range);

        assert_eq!(preview, "启动成功");
        assert_eq!(&preview[range], "启动");
    }

    /// 验证手动编码选择后选择器展示用户选择而不是自动检测结果。
    ///
    /// 业务意图：
    /// - 用户切换编码后需要立刻从工具条确认当前选择；即使测试文档本身可被 UTF-8 自动识别，
    ///   手动选择 GBK 时按钮也应显示 GBK，避免交互反馈看起来像选择无效。
    #[test]
    fn 手动编码选择器优先展示用户选择() {
        let document = decode_log_bytes(b"hello", EncodingChoice::Auto, "access.log")
            .expect("UTF-8 测试内容应能自动解码");
        let tab = OpenLogTab {
            id: 1,
            source: LogFileSource::LocalFile {
                path: PathBuf::from("access.log"),
            },
            source_key: "local:access.log".to_string(),
            title: "access.log".to_string(),
            encoding_choice: EncodingChoice::Manual(LogTextEncoding::Gbk),
            thread_filter_available: false,
            thread_filter_original_document: None,
            thread_filter_summary: None,
            raw_bytes: Some(Arc::new(b"hello".to_vec())),
            state: LogTabState::Ready {
                document: Box::new(LogTabDocument::InMemory(document)),
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
        };

        assert_eq!(MainView::log_tab_encoding_selector_label(&tab), "GBK");
    }

    /// 验证分页日志把超大滚动位置映射为视口内小坐标。
    ///
    /// 业务意图：
    /// - 用户反馈滚到三千多万行后行号出现间隔、正文重叠，本质是完整列表的 `f32` 像素坐标过大。
    /// - 新分页渲染只保留首个真实行号和小于单行高度的偏移，确保交给布局系统的坐标始终很小。
    #[test]
    fn 分页日志深处滚动只产生视口内小偏移() {
        let target_line = 33_142_000usize;
        let row_height = f64::from(px(LOG_VIEWER_ROW_HEIGHT));
        let scroll_top = target_line as f64 * row_height + 7.25;

        let (first_line, fractional_top) =
            MainView::paged_log_visible_start(scroll_top, target_line + 1_000);

        assert_eq!(first_line, target_line);
        assert!((7.0..7.5).contains(&fractional_top));
        assert!(
            fractional_top < LOG_VIEWER_ROW_HEIGHT,
            "分页渲染只能把单行内偏移交给 GPUI，不能再传递完整深度坐标"
        );
    }

    /// 验证分页日志跳转目标行会夹在合法滚动范围内。
    ///
    /// 边界条件：
    /// - 搜索结果可能指向文件末尾附近；滚动位置必须限制在最大可滚动距离内，否则最后一屏会出现空白。
    #[test]
    fn 分页日志跳转行号会限制在最大滚动范围内() {
        let line_count = 50_000_000usize;
        let viewport_height = px(880.0);
        let max_scroll = MainView::paged_log_vertical_max_scroll_px(line_count, viewport_height);

        let scroll_top = MainView::paged_log_scroll_top_for_line(
            line_count + 10_000,
            line_count,
            viewport_height,
        );

        assert_eq!(scroll_top, max_scroll);
    }

    /// 验证 minimap 点击坐标会按局部窗口映射到合法行号。
    ///
    /// 业务意图：
    /// - 右侧 minimap 当前只显示正文视口上下文，点击坐标应落到当前局部窗口中的真实日志行，而不是整份文件百分比。
    /// - 坐标越界时仍需要夹紧，避免用户点击右侧栏边缘时产生非法行号。
    #[test]
    fn 日志_minimap_点击坐标会夹紧到合法行号() {
        let scroll_info = LogMinimapScrollInfo {
            scroll_top_px: 120_000.0,
            max_scroll_px: 500_000.0,
            viewport_width_px: 1280.0,
            viewport_height_px: 720.0,
        };
        let layout = MainView::log_minimap_layout_for_scroll(
            20_000,
            px(LOG_MINIMAP_WIDTH),
            px(720.0),
            scroll_info,
        )
        .expect("有效日志和视口尺寸应生成 minimap 布局");

        assert_eq!(
            MainView::log_minimap_target_line_from_y(px(10.0), layout, 0),
            None
        );
        assert_eq!(
            MainView::log_minimap_target_line_from_y(px(80.0), layout, 1),
            Some(0)
        );

        let top_line = MainView::log_minimap_target_line_from_y(px(-20.0), layout, 20_000)
            .expect("越界顶部坐标应夹紧到局部窗口起点");
        assert!(
            (top_line as f64 - layout.window_start_line_float).abs() < 1.0,
            "顶部越界点击应夹紧到当前局部窗口起点，而不是整份文件开头"
        );

        let bottom_line = MainView::log_minimap_target_line_from_y(px(900.0), layout, 20_000)
            .expect("越界底部坐标应夹紧到局部窗口末端");
        assert!(
            bottom_line >= layout.window_start_line,
            "底部越界点击仍应落在当前局部窗口内"
        );
    }

    /// 验证 minimap 点击会把局部窗口中的目标行滚到正文中部。
    ///
    /// 业务意图：
    /// - 点击 minimap 的语义是“跳到鼠标下这段日志”，不是点击滚动条轨道百分比。
    /// - 该测试锁定局部窗口点击换算，避免后续优化拖动逻辑时重新退化为全文百分比跳转。
    #[test]
    fn 日志_minimap_点击按局部行滚到正文中部() {
        let line_count = 20_000;
        let scroll_info = LogMinimapScrollInfo {
            scroll_top_px: 120_000.0,
            max_scroll_px: 500_000.0,
            viewport_width_px: 1280.0,
            viewport_height_px: 720.0,
        };
        let layout = MainView::log_minimap_layout_for_scroll(
            line_count,
            px(LOG_MINIMAP_WIDTH),
            px(720.0),
            scroll_info,
        )
        .expect("有效日志和视口尺寸应生成 minimap 布局");
        let click_y = layout.viewport_block_top + layout.viewport_block_height + px(48.0);
        let target_line = MainView::log_minimap_target_line_from_y(click_y, layout, line_count)
            .expect("点击局部窗口内位置应得到目标行");
        let scroll_top =
            MainView::log_minimap_scroll_top_for_click(click_y, layout, scroll_info, line_count)
                .expect("点击局部窗口内位置应得到正文滚动偏移");
        let row_height = f64::from(px(LOG_VIEWER_ROW_HEIGHT));
        let visible_lines = scroll_info.viewport_height_px / row_height;
        let centered_line = scroll_top / row_height + visible_lines / 2.0;

        assert!(
            (centered_line - target_line as f64).abs() <= 1.0,
            "点击后的正文视口中部应接近 minimap 鼠标下的目标行"
        );
    }

    /// 验证分页日志不会显示 minimap。
    ///
    /// 业务意图：
    /// - 分页模式用于保护超大日志的内存和滚动性能，右侧 minimap 会在滚动时增加额外读取与高亮计算，因此必须从显示入口关闭。
    /// - 内存日志仍允许显示 minimap，避免该性能保护误伤普通日志的右侧预览体验。
    #[test]
    fn 日志_minimap_分页模式不显示以避免性能问题() {
        let in_memory_document =
            decode_log_bytes(b"first\nsecond\nthird", EncodingChoice::Auto, "memory.log")
                .expect("测试内存日志应能按 UTF-8 解码");
        assert!(
            MainView::log_minimap_document_allows_render(&LogTabDocument::InMemory(
                in_memory_document
            )),
            "普通内存日志仍允许在存在纵向溢出时显示 minimap"
        );

        let path = env::temp_dir().join(format!(
            "logclinic3-paged-minimap-disabled-test-{}.log",
            std::process::id()
        ));
        fs::write(&path, b"first\nsecond\nthird").expect("测试分页日志临时文件应能写入");
        let byte_len = fs::metadata(&path)
            .expect("测试分页日志临时文件应能读取元数据")
            .len();
        let paged_document = crate::log_document::PagedLogDocument::open(
            MaterializedLogSource {
                original_source: LogFileSource::LocalFile { path: path.clone() },
                temp_path: path.clone(),
                byte_len,
            },
            EncodingChoice::Auto,
            "paged.log",
        )
        .expect("测试分页日志应能建立行索引");
        let paged_tab_document = LogTabDocument::Paged(paged_document);

        assert!(
            !MainView::log_minimap_document_allows_render(&paged_tab_document),
            "分页日志必须关闭 minimap，避免滚动时产生额外分页读取和高亮成本"
        );

        drop(paged_tab_document);
        let _ = fs::remove_file(path);
    }

    /// 验证 minimap 对超大日志只按视口高度采样。
    ///
    /// 业务意图：
    /// - 千万级日志不能因为右侧预览栏回到逐行绘制；采样数量必须受 minimap 高度控制，并覆盖文件首尾位置。
    #[test]
    fn 日志_minimap_超大日志采样数量受视口高度限制() {
        let viewport_height = px(900.0);
        let indices = MainView::log_minimap_sample_line_indices(50_000_000, viewport_height);

        assert!(!indices.is_empty());
        assert!(indices.len() <= MainView::log_minimap_sample_capacity(viewport_height));
        assert_eq!(indices.first().copied(), Some(0));
        assert_eq!(indices.last().copied(), Some(49_999_999));

        let line_height = MainView::log_minimap_sample_line_height(indices.len(), viewport_height);
        assert!(
            line_height <= f32::from(viewport_height) / indices.len() as f32,
            "缩略线高度必须小于等于采样间距，避免右侧预览行重叠"
        );
    }

    /// 验证 minimap 视口块保持真实正文窗口宽高比。
    ///
    /// 业务意图：
    /// - VS Code 风格 minimap 的灰色视口块应像正文窗口的缩小版，而不是按全文比例变成极薄滑块。
    #[test]
    fn 日志_minimap_视口块保持正文宽高比() {
        let scroll_info = LogMinimapScrollInfo {
            scroll_top_px: 12_000.0,
            max_scroll_px: 1_000_000.0,
            viewport_width_px: 1280.0,
            viewport_height_px: 720.0,
        };

        let block_height = MainView::log_minimap_viewport_block_height(
            px(LOG_MINIMAP_WIDTH),
            px(720.0),
            scroll_info,
        );
        let expected_height = LOG_MINIMAP_WIDTH * scroll_info.viewport_height_px as f32
            / scroll_info.viewport_width_px as f32;

        assert!(
            (f32::from(block_height) - expected_height).abs() < 0.01,
            "110px 宽 minimap 对应 1280x720 正文窗口时，视口块高度应保持正文宽高比"
        );
    }

    /// 验证 minimap 只绘制当前视口附近的局部窗口。
    ///
    /// 业务意图：
    /// - 大日志不能再被压缩成一整张全文件缩略图；当前可见行应落在局部窗口的视口块位置，上下保留有限上下文。
    #[test]
    fn 日志_minimap_局部窗口不覆盖整份超大日志() {
        let line_count = 50_000_000usize;
        let scroll_info = LogMinimapScrollInfo {
            scroll_top_px: 3_600_000.0,
            max_scroll_px: 1_200_000_000.0,
            viewport_width_px: 1200.0,
            viewport_height_px: 900.0,
        };

        let layout = MainView::log_minimap_layout_for_scroll(
            line_count,
            px(LOG_MINIMAP_WIDTH),
            px(900.0),
            scroll_info,
        )
        .expect("有效尺寸和行数必须生成 minimap 局部窗口");
        let current_top_line = scroll_info.scroll_top_px / f64::from(px(LOG_VIEWER_ROW_HEIGHT));
        let current_top_y =
            (current_top_line - layout.window_start_line_float) * layout.line_height_px as f64;

        assert!(
            layout.window_line_count < 1_000,
            "局部窗口只应覆盖当前视口上下文，不能覆盖千万级全文"
        );
        assert!(
            layout.window_start_line > 0
                && layout.window_start_line + layout.window_line_count < line_count,
            "中段滚动时局部窗口应位于文件内部，而不是从首尾强行显示整份日志"
        );
        assert!(
            (current_top_y - f64::from(layout.viewport_block_top)).abs() < 0.5,
            "正文当前顶部行应对齐到 minimap 视口块顶部"
        );
    }

    /// 验证 minimap 静态文本层使用量化缓存窗口。
    ///
    /// 业务意图：
    /// - 滚轮或拖动滚动条时，正文顶部行会连续变化；minimap 不能因为每一行变化都重建离屏位图。
    /// - 小幅滚动应复用同一个量化起点，跨过足够距离后才切换到新的静态文本层。
    #[test]
    fn 日志_minimap_缓存窗口小幅滚动复用量化起点() {
        let first_start = MainView::log_minimap_quantized_window_start_line(10_000);
        let nearby_start = MainView::log_minimap_quantized_window_start_line(10_010);
        let far_start = MainView::log_minimap_quantized_window_start_line(10_480);

        assert_eq!(
            first_start, nearby_start,
            "十行以内的小幅滚动应只移动图片偏移，不应重建 minimap 静态层"
        );
        assert!(
            far_start > first_start,
            "跨过量化块后才允许进入新的 minimap 静态层缓存窗口"
        );

        let base_scroll_info = LogMinimapScrollInfo {
            scroll_top_px: 120_000.0,
            max_scroll_px: 500_000.0,
            viewport_width_px: 1280.0,
            viewport_height_px: 720.0,
        };
        let nearby_scroll_info = LogMinimapScrollInfo {
            scroll_top_px: 120_000.0 + f64::from(px(LOG_VIEWER_ROW_HEIGHT)) * 10.0,
            ..base_scroll_info
        };
        let base_layout = MainView::log_minimap_layout_for_scroll(
            20_000,
            px(LOG_MINIMAP_WIDTH),
            px(720.0),
            base_scroll_info,
        )
        .expect("有效日志和视口尺寸应生成 minimap 布局");
        let nearby_layout = MainView::log_minimap_layout_for_scroll(
            20_000,
            px(LOG_MINIMAP_WIDTH),
            px(720.0),
            nearby_scroll_info,
        )
        .expect("有效日志和视口尺寸应生成 minimap 布局");

        assert_eq!(
            base_layout.window_start_line, nearby_layout.window_start_line,
            "同一量化块内的小幅滚动应复用同一个缓存起点"
        );
        assert_eq!(
            base_layout.window_line_count, nearby_layout.window_line_count,
            "同一量化块内的小幅滚动不应改变缓存窗口行数，否则 cache key 会逐行失效"
        );
    }

    /// 构造测试用 minimap 缓存键。
    ///
    /// 业务意图：
    /// - 多个 minimap 缓存测试只关心 key 的相等性和窗口起点变化，集中构造可以避免字段新增时测试样板分散失效。
    fn test_log_minimap_cache_key() -> LogMinimapCacheKey {
        LogMinimapCacheKey {
            source_key: "memory.log".to_string(),
            line_count: 20_000,
            viewport_width_px: 1280,
            viewport_height_px: 720,
            window_start_line: 9_600,
            window_line_count: 480,
            bucket_count: 480,
            image_width_px: 110,
            image_height_px: 960,
            scale_factor_milli: 1000,
            palette_signature: 42,
            syntax_theme: SyntaxTheme::Light,
            paged: false,
        }
    }

    /// 验证 minimap 静态层后台构建请求会去重。
    ///
    /// 业务意图：
    /// - 滚动跨过量化块时 UI 线程应继续绘制上一张静态图，并且只为最新局部窗口派发一次后台任务。
    /// - 如果当前图片或 pending 任务已经覆盖请求 key，再次进入 canvas prepare 不应重复生成位图，避免滚动时后台任务堆积。
    #[test]
    fn 日志_minimap_静态层后台构建请求会去重() {
        let current_key = test_log_minimap_cache_key();
        let mut next_key = current_key.clone();
        next_key.window_start_line = 9_792;

        assert!(
            !MainView::log_minimap_should_request_static_layer_build(
                Some(&current_key),
                None,
                &current_key,
            ),
            "已有静态图片匹配当前 key 时不应派发后台构建"
        );
        assert!(
            !MainView::log_minimap_should_request_static_layer_build(
                Some(&current_key),
                Some(&next_key),
                &next_key,
            ),
            "同一个新 key 已有后台任务时不应重复派发"
        );
        assert!(
            !MainView::log_minimap_should_request_static_layer_build(
                Some(&current_key),
                Some(&next_key),
                &current_key,
            ),
            "即使用户滚回已命中的静态图，也不能忘记仍在后台运行的旧任务，否则下一次远距离拖动会重新堆积构建任务"
        );
        let mut far_key = current_key.clone();
        far_key.window_start_line = 10_176;
        assert!(
            !MainView::log_minimap_should_request_static_layer_build(
                Some(&current_key),
                Some(&next_key),
                &far_key,
            ),
            "已有任意后台任务时不应继续派发新任务，避免快速拖动堆积大量过期静态层构建"
        );
        assert!(
            MainView::log_minimap_should_request_static_layer_build(
                Some(&current_key),
                None,
                &next_key,
            ),
            "跨到新量化窗口且没有 pending 任务时，应派发后台构建并继续保留旧图"
        );
    }

    /// 验证 minimap 缓存清理会取出静态图片。
    ///
    /// 业务意图：
    /// - 关闭 tab、重新加载和切换编码时，`RenderImage` 需要交给 GPUI `drop_image` 释放 atlas 纹理；清理辅助函数必须先把图片从缓存中取出。
    /// - 该测试不依赖真实窗口，只锁定缓存所有权转移，避免后续重构再次只删除 HashMap 条目而遗漏纹理释放。
    #[test]
    fn 日志_minimap_缓存清理会取出静态图片() {
        let buffer = image::RgbaImage::new(1, 1);
        let image = Arc::new(gpui::RenderImage::new(smallvec::SmallVec::from_elem(
            image::Frame::new(buffer),
            1,
        )));
        let key = test_log_minimap_cache_key();
        let mut cache = LogMinimapRenderCache {
            key: key.clone(),
            static_image_key: Some(key),
            pending_key: None,
            pending_generation: 0,
            static_image: Some(image.clone()),
            image_logical_width_px: 1.0,
            image_logical_height_px: 1.0,
        };

        let taken = MainView::log_minimap_take_static_image(&mut cache)
            .expect("带静态图片的 minimap 缓存应能取出待释放图片");
        assert!(
            Arc::ptr_eq(&taken, &image),
            "取出的图片必须是缓存里原来的 RenderImage，才能释放已上传的同一张 atlas 纹理"
        );
        assert!(
            cache.static_image.is_none(),
            "图片取出后缓存内不应继续持有 Arc，避免后续重复释放或误以为仍有可绘制静态层"
        );
    }

    /// 验证 minimap 过期静态层在后台刷新期间不会被平移到可视区外。
    ///
    /// 业务意图：
    /// - 用户拖动滚动条跨越很大距离时，新静态图由后台异步生成；旧静态图如果按真实行距继续平移，会完全离开右侧栏并造成空白。
    /// - 过期图只作为临时纹理使用，因此可以夹紧在可视区内；当 key 精确匹配时仍必须保留真实偏移，避免正常滚动错位。
    #[test]
    fn 日志_minimap_过期静态层偏移会夹紧避免拖动空白() {
        let far_below_offset =
            MainView::log_minimap_static_layer_y_offset(10_000.0, 1_000, 1.0, 800.0, 600.0, false);
        assert_eq!(
            far_below_offset, -200.0,
            "旧静态图理论上已经在可视区上方很远时，应夹紧到底部仍覆盖右侧栏"
        );

        let far_above_offset =
            MainView::log_minimap_static_layer_y_offset(1_000.0, 10_000, 1.0, 800.0, 600.0, false);
        assert_eq!(
            far_above_offset, 0.0,
            "旧静态图理论上已经在可视区下方很远时，应贴顶保留临时纹理"
        );

        let exact_offset =
            MainView::log_minimap_static_layer_y_offset(10_000.0, 1_000, 1.0, 800.0, 600.0, true);
        assert_eq!(
            exact_offset, -9_000.0,
            "静态图 key 精确匹配时不能夹紧，否则正常滚动会和真实行号错位"
        );
    }

    /// 验证 minimap 离屏位图按设备缩放生成物理像素。
    ///
    /// 业务意图：
    /// - Retina 和 Windows 缩放屏上，如果静态层仍按 1x 生成，GPUI 拉伸后会模糊；物理像素尺寸必须随 scale factor 增长。
    #[test]
    fn 日志_minimap_静态位图尺寸跟随设备缩放() {
        let scroll_info = LogMinimapScrollInfo {
            scroll_top_px: 120_000.0,
            max_scroll_px: 500_000.0,
            viewport_width_px: 1280.0,
            viewport_height_px: 720.0,
        };
        let layout = MainView::log_minimap_layout_for_scroll(
            20_000,
            px(LOG_MINIMAP_WIDTH),
            px(720.0),
            scroll_info,
        )
        .expect("有效日志和视口尺寸应生成 minimap 布局");

        let one_x = MainView::log_minimap_static_image_size(px(LOG_MINIMAP_WIDTH), layout, 1.0);
        let two_x = MainView::log_minimap_static_image_size(px(LOG_MINIMAP_WIDTH), layout, 2.0);

        assert_eq!(two_x.0, one_x.0 * 2, "2x 屏幕的位图宽度应翻倍");
        assert_eq!(two_x.1, one_x.1 * 2, "2x 屏幕的位图高度应翻倍");
    }

    /// 验证 minimap bucket 在千万级日志中不会溢出且覆盖首尾。
    ///
    /// 业务意图：
    /// - 优化后的 minimap 使用固定数量 bucket 缓存静态内容层，行号换算必须能处理超大分页日志。
    /// - 最后一段范围需要夹紧到真实末尾，避免预览底部点击、绘制或聚合时访问不存在的行。
    #[test]
    fn 日志_minimap_bucket_千万级行数范围稳定() {
        let line_count = 50_000_000usize;
        let bucket_count = 520usize;

        let first = MainView::log_minimap_bucket_line_range(line_count, bucket_count, 0);
        let middle =
            MainView::log_minimap_bucket_line_range(line_count, bucket_count, bucket_count / 2);
        let last =
            MainView::log_minimap_bucket_line_range(line_count, bucket_count, bucket_count - 1);

        assert_eq!(first.0, 0, "第一个 bucket 必须从文件首行开始");
        assert!(
            first.1 > first.0 && middle.1 > middle.0 && last.1 > last.0,
            "每个 bucket 都必须覆盖至少一行，避免空范围导致预览缺块"
        );
        assert_eq!(
            last.1, line_count,
            "最后一个 bucket 必须夹紧到真实行数，不能越界"
        );
    }

    /// 验证分页日志的 minimap 轮廓只使用行索引字节长度。
    ///
    /// 业务意图：
    /// - 大日志分页模式不能为了右侧预览随机读取正文；字节长度轮廓必须可单独生成并按最大分析列数夹紧。
    #[test]
    fn 日志_minimap_分页轮廓按字节长度夹紧() {
        let empty_line = MainView::log_minimap_segments_for_byte_len(0);
        let long_line = MainView::log_minimap_segments_for_byte_len(50_000);

        assert_eq!(empty_line.len(), 1, "空行也需要保留最小可见轮廓");
        assert_eq!(empty_line[0].column_len, 1);
        assert!(
            long_line[0].column_len <= 160,
            "超长行必须按 minimap 最大分析列数夹紧，避免宽度计算溢出"
        );
    }

    /// 验证横向滚动条不会重复扣减右侧 minimap 宽度。
    ///
    /// 业务意图：
    /// - minimap 和纵向滚动条已经作为正文内容区的并列 flex 子项存在，横向滚动条的视口测量天然排除了右侧栏。
    /// - 如果这里继续传入右侧预留宽度，滑块轨道会被二次扣短，导致横向拖动比例和视觉长度都偏小。
    #[test]
    fn 日志_横向滚动条不重复扣减并列_minimap() {
        assert_eq!(
            MainView::log_horizontal_scrollbar_right_reserved_width_for_current_layout(),
            0.0,
            "当前并列布局下，横向滚动条不应额外扣除右侧 minimap 宽度"
        );
    }

    /// 验证右侧纵向滚动条槽可以脱离 minimap 独立显示。
    ///
    /// 业务意图：
    /// - 分页日志为了性能不显示 minimap，但仍需要保留右侧纵向滚动条作为精确拖动入口。
    /// - 普通内存日志显示 minimap 时，首帧即使滚动条指标未回填也保留槽位；短日志则不显示无意义空槽。
    #[test]
    fn 日志_右侧纵向滚动条槽支持分页独立显示() {
        assert!(
            MainView::log_vertical_scrollbar_gutter_should_render(true, false),
            "分页日志禁用 minimap 时，只要存在纵向滚动条指标仍应显示滚动条槽"
        );
        assert!(
            MainView::log_vertical_scrollbar_gutter_should_render(false, true),
            "minimap 可见但滚动条指标首帧暂缺时，应保留槽位避免布局抖动"
        );
        assert!(
            !MainView::log_vertical_scrollbar_gutter_should_render(false, false),
            "短日志既没有滚动条也没有 minimap 时，不应显示右侧空槽"
        );
    }

    /// 验证 minimap 迷你文本使用单条轮廓并识别错误行。
    ///
    /// 业务意图：
    /// - 小文件预览需要保留缩进和行长轮廓，但不能把表格列按空白拆成大量竖向条纹；错误日志仍需要在预览栏中有更强视觉信号。
    #[test]
    fn 日志_minimap_迷你文本保留片段和错误色调() {
        let segments = MainView::log_minimap_segments_for_text("INFO  worker-1  ERROR failed");

        assert_eq!(segments.len(), 1, "每个采样行最多绘制一条连续轮廓线");
        assert!(segments[0].column_len > 10, "连续轮廓应保留行长变化");
        assert_eq!(
            format!(
                "{:?}",
                MainView::log_minimap_tone_for_line("2026-05-20 ERROR failed")
            ),
            "Error"
        );
    }

    /// 验证 minimap 使用真实显示文本并复用正文高亮。
    ///
    /// 业务意图：
    /// - 右侧预览不能只画行长横线；它需要拿到正文同源的显示文本和高亮范围，后续绘制阶段才能画出真实字符纹理。
    #[test]
    fn 日志_minimap_真实字符复用正文高亮() {
        let (display_text, highlights) = MainView::log_minimap_display_text_and_highlights(
            "2026-05-20 04:37:04 ERROR failed",
            crate::highlighting::HighlightMode::Log,
            None,
            SyntaxTheme::Light,
        );

        assert!(
            display_text.starts_with("2026-05-20"),
            "minimap 应保留真实日志字符，而不是只保留长度轮廓"
        );
        assert!(
            highlights.iter().any(|(_, style)| style.color.is_some()),
            "minimap 高亮应复用正文语法高亮颜色"
        );
    }

    /// 验证 minimap 微缩字符图集能区分文本和空白。
    ///
    /// 业务意图：
    /// - 静态文本层不再调用平台字体排版，而是使用内置点阵字符；空格必须保持透明，真实字符必须产生像素纹理。
    #[test]
    fn 日志_minimap_微缩字符图集区分文本和空白() {
        let digit = MainView::log_minimap_micro_glyph_rows('8');
        let letter = MainView::log_minimap_micro_glyph_rows('a');
        let upper_letter = MainView::log_minimap_micro_glyph_rows('A');
        let whitespace = MainView::log_minimap_micro_glyph_rows(' ');
        let non_ascii = MainView::log_minimap_micro_glyph_rows('中');

        assert!(
            digit.iter().any(|row| *row != 0),
            "数字字符应在 minimap 字符图集中产生像素"
        );
        assert_eq!(
            letter, upper_letter,
            "小写字母复用大写点阵，保证缩略图稳定且无需维护两套字形"
        );
        assert!(
            whitespace.iter().all(|row| *row == 0),
            "空格必须保持透明，只推进列位置"
        );
        assert!(
            non_ascii.iter().any(|row| *row != 0),
            "中文等非 ASCII 字符应显示占位纹理，避免整段日志在 minimap 中消失"
        );
    }

    /// 验证 minimap 拖动换算支持极大的分页滚动范围。
    ///
    /// 边界条件：
    /// - 超大日志的最大滚动距离可能远超 `f32` 精度稳定区，拖动换算必须保留 `f64` 范围并夹紧到合法上下界。
    #[test]
    fn 日志_minimap_拖动换算支持超大滚动距离() {
        let max_scroll = 1_500_000_000.0_f64;
        let scroll_top = MainView::log_minimap_scroll_top_for_drag(
            px(500.0),
            px(10.0),
            px(900.0),
            px(18.0),
            max_scroll,
        );

        assert!(scroll_top > 0.0);
        assert!(scroll_top <= max_scroll);
        assert_eq!(
            MainView::log_minimap_scroll_top_for_drag(
                px(-100.0),
                px(10.0),
                px(900.0),
                px(18.0),
                max_scroll,
            ),
            0.0
        );
    }

    /// 验证 minimap 只绘制当前 tab 来源对应的搜索命中。
    ///
    /// 业务意图：
    /// - 底部搜索结果面板会保留多文件、多轮搜索历史；右侧预览栏不能把其它文件的命中行画到当前日志上。
    #[test]
    fn 日志_minimap_搜索标记只使用当前来源() {
        let current_source = test_local_file("/tmp/current.log");
        let other_source = test_local_file("/tmp/other.log");
        let current_key = current_source.stable_key();
        let other_key = other_source.stable_key();
        let record = SearchHistoryRecord {
            job_id: 1,
            query: "error".to_string(),
            scope: SearchScope::CurrentDirectory,
            directory_target: Some("/tmp".to_string()),
            case_sensitive: false,
            match_mode: SearchMatchMode::Literal,
            progress: SearchProgress::default(),
            results: vec![
                SearchResultItem {
                    source: current_source,
                    source_key: current_key.clone(),
                    file_name: "current.log".to_string(),
                    location: "/tmp".to_string(),
                    line_index: 12,
                    line_text: "ERROR current".to_string(),
                    match_range: 0..5,
                },
                SearchResultItem {
                    source: other_source,
                    source_key: other_key,
                    file_name: "other.log".to_string(),
                    location: "/tmp".to_string(),
                    line_index: 88,
                    line_text: "ERROR other".to_string(),
                    match_range: 0..5,
                },
            ],
            result_groups: Vec::new(),
            result_group_indices: HashMap::new(),
            errors: Vec::new(),
            canceled: false,
            expanded: true,
            expanded_file_keys: HashSet::new(),
        };

        let markers =
            MainView::log_minimap_search_marker_lines_for_records(&current_key, Some(3), &[record]);

        assert_eq!(markers, vec![3, 12]);
    }

    /// 验证千万级行号列不会被旧的窄上限裁切。
    ///
    /// 业务意图：
    /// - 用户截图中的 33325038 行属于 8 位行号；如果行号列仍被 64px 上限截断，最左侧数字会显示不全。
    #[test]
    fn 千万级日志行号列宽度能容纳完整数字() {
        let width = MainView::log_viewer_line_number_width(33_325_038);

        assert!(
            width > 64.0,
            "8 位行号需要突破旧的 64px 上限，否则左侧高位会被裁切"
        );
    }

    /// 验证日志正文右键另存为按菜单绑定 tab 查找来源。
    ///
    /// 业务意图：
    /// - 正文右键菜单打开后不应依赖当前激活 tab；另存为必须使用菜单绑定 tab 的来源，否则会出现点击菜单无反应或保存错文件。
    #[test]
    fn 日志正文右键另存为按绑定_tab_查找来源() {
        let first_source = LogFileSource::LocalFile {
            path: PathBuf::from("first.log"),
        };
        let second_source = LogFileSource::LocalFile {
            path: PathBuf::from("second.log"),
        };
        let tabs = vec![
            OpenLogTab {
                id: 10,
                source: first_source.clone(),
                source_key: "local:first.log".to_string(),
                title: "first.log".to_string(),
                encoding_choice: EncodingChoice::Auto,
                thread_filter_available: false,
                thread_filter_original_document: None,
                thread_filter_summary: None,
                raw_bytes: None,
                state: LogTabState::Loading {
                    message: "测试加载中".to_string(),
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
            },
            OpenLogTab {
                id: 20,
                source: second_source.clone(),
                source_key: "local:second.log".to_string(),
                title: "second.log".to_string(),
                encoding_choice: EncodingChoice::Auto,
                thread_filter_available: false,
                thread_filter_original_document: None,
                thread_filter_summary: None,
                raw_bytes: None,
                state: LogTabState::Loading {
                    message: "测试加载中".to_string(),
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
            },
        ];

        assert_eq!(
            MainView::log_viewer_save_source_for_tab_from_tabs(&tabs, 20),
            Some(second_source)
        );
        assert_eq!(
            MainView::log_viewer_save_source_for_tab_from_tabs(&tabs, 99),
            None
        );
    }

    /// 验证搜索结果定位同时保留整行高亮和关键字片段高亮。
    ///
    /// 业务意图：
    /// - 任意日志正文搜索入口都应通过整行黄色背景提示“当前定位行”，并通过暖橙色片段背景提示“命中关键字列”。
    /// - 左侧文件树搜索只影响文件名关键字，不能误把日志正文搜索结果的整行定位反馈清掉。
    #[test]
    fn 搜索结果定位会同时设置行高亮和片段高亮() {
        let source = test_local_file("/tmp/search.log");
        let mut tab = OpenLogTab {
            id: 30,
            source,
            source_key: "local:search.log".to_string(),
            title: "search.log".to_string(),
            encoding_choice: EncodingChoice::Auto,
            thread_filter_available: false,
            thread_filter_original_document: None,
            thread_filter_summary: None,
            raw_bytes: None,
            state: LogTabState::Loading {
                message: "测试加载中".to_string(),
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
        };

        MainView::apply_search_result_highlight_to_tab(&mut tab, 12, 4..9);

        assert_eq!(tab.pending_scroll_to_line, Some(12));
        assert_eq!(tab.highlighted_search_line, Some(12));
        assert_eq!(
            tab.highlighted_search_match,
            Some(LogSearchMatchHighlight {
                line_index: 12,
                match_range: 4..9,
            })
        );
    }

    /// 验证搜索关键字片段在明暗主题下都保持可读，且和整行定位背景区分开。
    ///
    /// 业务意图：
    /// - 日志正文搜索现在统一保留整行背景和关键字片段背景；两层颜色必须不同，用户才能同时识别命中行和命中列。
    /// - 亮色主题保留原有暖橙色背景；暗色主题需要额外覆盖前景色，避免绿色语法高亮文本落在搜索底色上看不清。
    #[test]
    fn 日志搜索关键字片段在明暗主题下都保持可读() {
        let light_palette = AppThemePalette::for_theme(EffectiveTheme::Light);
        let highlight = LogSearchMatchHighlight {
            line_index: 3,
            match_range: 7..14,
        };
        let (range, light_style) = MainView::log_search_match_highlight_for_line(
            Some(&highlight),
            3,
            "prefix keyword suffix",
            light_palette,
        )
        .expect("当前行存在搜索命中时应返回关键字片段高亮");

        assert_eq!(range, 7..14);
        assert_eq!(light_style.background_color, Some(rgb(0xf2a65a).into()));
        assert_eq!(light_style.color, None);
        assert_ne!(
            light_style.background_color,
            Some(rgb(light_palette.search_highlight).into())
        );
        assert_eq!(light_style.font_weight, Some(gpui::FontWeight::SEMIBOLD));

        let dark_palette = AppThemePalette::for_theme(EffectiveTheme::Dark);
        let (_, dark_style) = MainView::log_search_match_highlight_for_line(
            Some(&highlight),
            3,
            "prefix keyword suffix",
            dark_palette,
        )
        .expect("暗色主题当前行存在搜索命中时也应返回关键字片段高亮");
        assert_eq!(dark_style.background_color, Some(rgb(0x1d4ed8).into()));
        assert_eq!(dark_style.color, Some(rgb(0xf8fafc).into()));
        assert_ne!(
            dark_style.background_color,
            Some(rgb(dark_palette.search_highlight).into())
        );
        assert_eq!(dark_style.font_weight, Some(gpui::FontWeight::SEMIBOLD));
    }

    /// 验证暗色搜索关键字前景色不会和日志语法色混合。
    ///
    /// 业务意图：
    /// - GPUI 合并重叠高亮时会混合前景色；如果搜索关键字范围内仍保留绿色日志语法色，最终文字会变成低对比混合色。
    /// - 暗色搜索关键字带显式前景色时，需要先清掉命中范围内的语法前景色，让关键字文字保持稳定可读。
    #[test]
    fn 暗色日志搜索关键字前景色会覆盖语法色() {
        let palette = AppThemePalette::for_theme(EffectiveTheme::Dark);
        let (_, search_style) = MainView::log_search_match_highlight_for_line(
            Some(&LogSearchMatchHighlight {
                line_index: 0,
                match_range: 2..6,
            }),
            0,
            "xxkeyword",
            palette,
        )
        .expect("测试行存在搜索命中时应返回关键字片段高亮");

        let highlights = MainView::combine_log_highlights_with_search_match(
            vec![(
                0..10,
                gpui::HighlightStyle {
                    color: Some(rgb(0x5ee787).into()),
                    ..Default::default()
                },
            )],
            2..6,
            search_style,
        );

        let selected_keyword = highlights
            .iter()
            .find(|(range, _)| *range == (2..6))
            .expect("搜索关键字和语法色重叠部分应被单独拆分");
        assert_eq!(selected_keyword.1.color, search_style.color);
        assert_eq!(
            selected_keyword.1.background_color,
            search_style.background_color
        );
    }

    /// 验证鼠标选区覆盖搜索关键字背景，并在暗色主题下覆盖为可读文字颜色。
    ///
    /// 业务意图：
    /// - 搜索命中关键字本身有暖橙色背景，鼠标选区有蓝色背景；当两者重叠时，用户更需要看到“这段文本已被选中”。
    /// - 选区覆盖背景时仍应保留搜索关键字的粗体等文字样式；暗色主题额外覆盖前景色，避免语法高亮色和选区背景对比不足。
    #[test]
    fn 日志选区覆盖搜索关键字背景并保持暗色可读() {
        let palette = AppThemePalette::for_theme(EffectiveTheme::Dark);
        let search_background: gpui::Hsla = rgb(0xf2a65a).into();
        let selection_style = MainView::log_text_selection_highlight_style(palette);
        let selection_background = selection_style.background_color;
        let highlights = MainView::combine_log_highlights_with_selection(
            vec![(
                4..10,
                gpui::HighlightStyle {
                    background_color: Some(search_background),
                    font_weight: Some(gpui::FontWeight::SEMIBOLD),
                    ..Default::default()
                },
            )],
            6..8,
            palette,
        );

        let selected_keyword = highlights
            .iter()
            .find(|(range, _)| *range == (6..8))
            .expect("搜索关键字和鼠标选区重叠部分应被单独拆分");
        assert_eq!(selected_keyword.1.background_color, selection_background);
        assert_eq!(selected_keyword.1.color, selection_style.color);
        assert_eq!(
            selected_keyword.1.font_weight,
            Some(gpui::FontWeight::SEMIBOLD)
        );
        assert!(
            highlights.iter().any(|(range, style)| *range == (4..6)
                && style.background_color == Some(search_background)),
            "选区外的搜索关键字背景应保持不变"
        );
    }

    /// 验证搜索条件变化只清理当前激活 tab 的片段高亮。
    ///
    /// 业务意图：
    /// - 用户修改搜索框关键字后，当前正文中的旧关键字高亮必须立即失效，避免“未找到”时仍显示旧命中。
    /// - 其它 tab 的高亮属于各自历史定位上下文，切换过去前不应被当前 tab 的输入动作误清。
    #[test]
    fn 搜索条件变化只清理当前_tab_片段高亮() {
        let first_source = LogFileSource::LocalFile {
            path: PathBuf::from("first.log"),
        };
        let second_source = LogFileSource::LocalFile {
            path: PathBuf::from("second.log"),
        };
        let mut tabs = vec![
            OpenLogTab {
                id: 10,
                source: first_source,
                source_key: "local:first.log".to_string(),
                title: "first.log".to_string(),
                encoding_choice: EncodingChoice::Auto,
                thread_filter_available: false,
                thread_filter_original_document: None,
                thread_filter_summary: None,
                raw_bytes: None,
                state: LogTabState::Loading {
                    message: "测试加载中".to_string(),
                },
                scroll_handle: UniformListScrollHandle::new(),
                paged_viewport_handle: ScrollHandle::new(),
                paged_scroll: PagedLogScrollState::default(),
                pending_scroll_to_line: None,
                highlighted_search_line: None,
                highlighted_search_match: Some(LogSearchMatchHighlight {
                    line_index: 1,
                    match_range: 0..5,
                }),
                marked_lines: BTreeSet::new(),
                last_marker_jump_line: None,
                text_selection: None,
                selection_drag_anchor: None,
            },
            OpenLogTab {
                id: 20,
                source: second_source,
                source_key: "local:second.log".to_string(),
                title: "second.log".to_string(),
                encoding_choice: EncodingChoice::Auto,
                thread_filter_available: false,
                thread_filter_original_document: None,
                thread_filter_summary: None,
                raw_bytes: None,
                state: LogTabState::Loading {
                    message: "测试加载中".to_string(),
                },
                scroll_handle: UniformListScrollHandle::new(),
                paged_viewport_handle: ScrollHandle::new(),
                paged_scroll: PagedLogScrollState::default(),
                pending_scroll_to_line: None,
                highlighted_search_line: None,
                highlighted_search_match: Some(LogSearchMatchHighlight {
                    line_index: 2,
                    match_range: 6..11,
                }),
                marked_lines: BTreeSet::new(),
                last_marker_jump_line: None,
                text_selection: None,
                selection_drag_anchor: None,
            },
        ];

        MainView::clear_log_tab_search_match_highlight_for_active(&mut tabs, Some(10));

        assert!(tabs[0].highlighted_search_match.is_none());
        assert_eq!(
            tabs[1].highlighted_search_match,
            Some(LogSearchMatchHighlight {
                line_index: 2,
                match_range: 6..11,
            })
        );
    }

    /// 验证单文件压缩包会返回内部成员来源，供 UI 点击压缩包根节点时直接打开。
    #[test]
    fn 单文件压缩包返回唯一成员来源() {
        let source = test_archive_member("access.log");
        let tree = LoadedLogTree {
            summary: "2 个节点".to_string(),
            rows: vec![
                test_tree_row(0, 0, LogTreeEntryKind::Archive, true, None),
                test_tree_row(1, 1, LogTreeEntryKind::File, false, Some(source.clone())),
            ],
            error_count: 0,
            temporary_paths: Vec::new(),
        };
        let state = LoadedLogTreeState::new(tree);

        assert_eq!(state.single_file_source_for_archive(0), Some(source));
    }

    /// 验证整棵加载树只有一个日志时会记录唯一来源，供主界面隐藏左侧树并自动打开。
    ///
    /// 业务意图：
    /// - 普通单文件和只含一个日志的目录都应走单日志浏览模式，避免左侧树占用空间。
    #[test]
    fn 单日志加载结果返回唯一来源() {
        let source = test_local_file("/tmp/app.log");
        let tree = LoadedLogTree {
            summary: "1 个节点".to_string(),
            rows: vec![test_tree_row(
                0,
                0,
                LogTreeEntryKind::File,
                false,
                Some(source.clone()),
            )],
            error_count: 0,
            temporary_paths: Vec::new(),
        };
        let state = LoadedLogTreeState::new(tree);

        assert_eq!(state.single_log_source(), Some(source));
    }

    /// 验证多日志或存在扫描错误时不会隐藏左侧树。
    ///
    /// 业务意图：
    /// - 多文件目录需要用户从左侧树中选择；存在错误时也要展示错误节点，不能被自动打开流程遮蔽。
    #[test]
    fn 多日志或错误加载结果不返回唯一来源() {
        let multi_tree = LoadedLogTree {
            summary: "2 个节点".to_string(),
            rows: vec![
                test_tree_row(
                    0,
                    0,
                    LogTreeEntryKind::File,
                    false,
                    Some(test_local_file("/tmp/a.log")),
                ),
                test_tree_row(
                    1,
                    0,
                    LogTreeEntryKind::File,
                    false,
                    Some(test_local_file("/tmp/b.log")),
                ),
            ],
            error_count: 0,
            temporary_paths: Vec::new(),
        };
        let error_tree = LoadedLogTree {
            summary: "2 个节点，1 个错误".to_string(),
            rows: vec![
                test_tree_row(
                    0,
                    0,
                    LogTreeEntryKind::File,
                    false,
                    Some(test_local_file("/tmp/a.log")),
                ),
                test_tree_row(1, 0, LogTreeEntryKind::Error, false, None),
            ],
            error_count: 1,
            temporary_paths: Vec::new(),
        };

        assert_eq!(
            LoadedLogTreeState::new(multi_tree).single_log_source(),
            None
        );
        assert_eq!(
            LoadedLogTreeState::new(error_tree).single_log_source(),
            None
        );
    }

    /// 验证多文件压缩包不会被绑定到单个成员，仍应保留展开/收起目录树的行为。
    #[test]
    fn 多文件压缩包不返回直接打开来源() {
        let tree = LoadedLogTree {
            summary: "3 个节点".to_string(),
            rows: vec![
                test_tree_row(0, 0, LogTreeEntryKind::Archive, true, None),
                test_tree_row(
                    1,
                    1,
                    LogTreeEntryKind::File,
                    false,
                    Some(test_archive_member("access.log")),
                ),
                test_tree_row(
                    2,
                    1,
                    LogTreeEntryKind::File,
                    false,
                    Some(test_archive_member("error.log")),
                ),
            ],
            error_count: 0,
            temporary_paths: Vec::new(),
        };
        let state = LoadedLogTreeState::new(tree);

        assert_eq!(state.single_file_source_for_archive(0), None);
    }

    /// 验证左侧树选中文件来源按加载树顺序过滤。
    ///
    /// 业务意图：
    /// - “选中搜索”与另存为、线程分析共用同一套来源收集规则，目录和错误节点不能进入搜索范围。
    /// - 用户多选顺序可能与树顺序不同，但后台搜索应按加载树顺序稳定执行，方便结果面板排序可预测。
    #[test]
    fn 左侧树选中文件来源按树顺序过滤不可读节点() {
        let first = test_local_file("/tmp/first.log");
        let second = test_local_file("/tmp/second.log");
        let tree = LoadedLogTree {
            summary: "5 个节点".to_string(),
            rows: vec![
                test_tree_row(0, 0, LogTreeEntryKind::Directory, true, None),
                test_tree_row(1, 1, LogTreeEntryKind::File, false, Some(first.clone())),
                test_tree_row(2, 1, LogTreeEntryKind::Directory, true, None),
                test_tree_row(3, 2, LogTreeEntryKind::File, false, Some(second.clone())),
                test_tree_row(4, 1, LogTreeEntryKind::Error, false, None),
            ],
            error_count: 1,
            temporary_paths: Vec::new(),
        };
        let state = LoadedLogTreeState::new(tree);
        let mut selected = HashSet::new();
        selected.insert(4);
        selected.insert(3);
        selected.insert(2);
        selected.insert(1);

        assert_eq!(
            state.file_sources_for_node_ids(&selected),
            vec![first, second]
        );
    }

    /// 验证单文件压缩包根节点可作为“选中搜索”来源。
    ///
    /// 业务意图：
    /// - 左侧树中只有一个成员的压缩包会支持点击根节点直接打开；右键“选中搜索”也应使用同一来源映射。
    /// - 多文件压缩包仍不会映射，避免用户选择压缩包根节点时误以为搜索了全部成员。
    #[test]
    fn 左侧树选中文件来源支持单文件压缩包节点() {
        let member = test_archive_member("access.log");
        let tree = LoadedLogTree {
            summary: "2 个节点".to_string(),
            rows: vec![
                test_tree_row(10, 0, LogTreeEntryKind::Archive, true, None),
                test_tree_row(11, 1, LogTreeEntryKind::File, false, Some(member.clone())),
            ],
            error_count: 0,
            temporary_paths: Vec::new(),
        };
        let state = LoadedLogTreeState::new(tree);
        let mut selected = HashSet::new();
        selected.insert(10);

        assert_eq!(state.file_sources_for_node_ids(&selected), vec![member]);
    }

    /// 验证单文件压缩包根节点和子文件同时选中时不会重复返回来源。
    ///
    /// 业务意图：
    /// - 单文件压缩包根节点会映射到唯一成员来源；用户 Shift 多选时可能同时包含根节点和子文件节点。
    /// - “选中搜索”必须只搜索一次该成员，否则结果面板会重复展示同一个日志的命中。
    #[test]
    fn 左侧树选中文件来源会去重单文件压缩包根节点和子文件() {
        let member = test_archive_member("access.log");
        let tree = LoadedLogTree {
            summary: "2 个节点".to_string(),
            rows: vec![
                test_tree_row(10, 0, LogTreeEntryKind::Archive, true, None),
                test_tree_row(11, 1, LogTreeEntryKind::File, false, Some(member.clone())),
            ],
            error_count: 0,
            temporary_paths: Vec::new(),
        };
        let state = LoadedLogTreeState::new(tree);
        let mut selected = HashSet::new();
        selected.insert(10);
        selected.insert(11);

        assert_eq!(state.file_sources_for_node_ids(&selected), vec![member]);
    }

    /// 验证左侧树菜单来源为空时使用右键落点兜底。
    ///
    /// 业务意图：
    /// - 如果用户没有形成有效多选，但右键落在可读文件上，“选中搜索”仍应搜索该文件。
    /// - 一旦已有有效选中文件，就必须保留完整多选集合，不能被右键落点覆盖。
    #[test]
    fn 左侧树菜单来源为空时使用右键兜底来源() {
        let fallback = test_local_file("/tmp/fallback.log");
        let selected = test_local_file("/tmp/selected.log");

        assert_eq!(
            MainView::log_tree_menu_action_sources(Vec::new(), Some(fallback.clone())),
            vec![fallback.clone()]
        );
        assert_eq!(
            MainView::log_tree_menu_action_sources(vec![selected.clone()], Some(fallback)),
            vec![selected]
        );
    }

    /// 验证插件日志树菜单只收集选中目录的直接子文件。
    ///
    /// 业务意图：
    /// - 插件框架第一版只把用户明确选择或目录直接子文件作为上下文发送给第三方插件，不递归扩大到深层目录。
    /// - 这样可以避免用户右键上层目录时意外把大量日志元数据发送给插件。
    #[test]
    fn 插件日志树菜单目录只收集直接子文件() {
        let direct = test_local_file("/tmp/direct.log");
        let nested = test_local_file("/tmp/nested.log");
        let tree = LoadedLogTree {
            summary: "4 个节点".to_string(),
            rows: vec![
                test_tree_row(0, 0, LogTreeEntryKind::Directory, true, None),
                test_tree_row(1, 1, LogTreeEntryKind::File, false, Some(direct)),
                test_tree_row(2, 1, LogTreeEntryKind::Directory, true, None),
                test_tree_row(3, 2, LogTreeEntryKind::File, false, Some(nested)),
            ],
            error_count: 0,
            temporary_paths: Vec::new(),
        };
        let state = LoadedLogTreeState::new(tree);
        let selected = HashSet::from([0]);
        let files = state.plugin_log_files_for_menu(&selected, 0, None);

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].display_name, "direct.log");
    }

    /// 验证插件菜单启用态只做存在性判断且目录直接子文件可启用。
    ///
    /// 业务意图：
    /// - 大目录右键菜单渲染阶段不能提前构造完整插件文件列表，只要发现第一个直接子文件即可启用插件菜单项。
    /// - 该测试锁定启用态规则与实际收集规则的一致性，避免后续改回“渲染时完整收集”。
    #[test]
    fn 插件日志树菜单启用态目录直接子文件为真() {
        let direct = test_local_file("/tmp/direct.log");
        let tree = LoadedLogTree {
            summary: "2 个节点".to_string(),
            rows: vec![
                test_tree_row(0, 0, LogTreeEntryKind::Directory, true, None),
                test_tree_row(1, 1, LogTreeEntryKind::File, false, Some(direct)),
            ],
            error_count: 0,
            temporary_paths: Vec::new(),
        };
        let state = LoadedLogTreeState::new(tree);
        let selected = HashSet::from([0]);

        assert!(state.has_plugin_log_file_for_menu(&selected, 0, None));
    }

    /// 验证普通文件菜单启用态不会把目录子文件当作显式选中文件。
    ///
    /// 业务意图：
    /// - “另存为”“选中搜索”“线程日志分析”保持只处理显式文件来源的旧语义。
    /// - 优化右键菜单性能时不能为了短路把普通目录错误识别为普通文件操作可用。
    #[test]
    fn 普通文件菜单启用态选中目录不会因子文件误启用() {
        let direct = test_local_file("/tmp/direct.log");
        let tree = LoadedLogTree {
            summary: "2 个节点".to_string(),
            rows: vec![
                test_tree_row(0, 0, LogTreeEntryKind::Directory, true, None),
                test_tree_row(1, 1, LogTreeEntryKind::File, false, Some(direct)),
            ],
            error_count: 0,
            temporary_paths: Vec::new(),
        };
        let state = LoadedLogTreeState::new(tree);
        let selected = HashSet::from([0]);

        assert!(!state.has_file_source_for_node_ids(&selected));
    }

    /// 验证插件日志树菜单混选时按树顺序去重。
    ///
    /// 业务意图：
    /// - 用户同时选中目录和该目录下的文件时，插件只应收到一次文件元数据，并且顺序与左侧加载树一致。
    #[test]
    fn 插件日志树菜单混选文件和目录按树顺序去重() {
        let first = test_local_file("/tmp/first.log");
        let second = test_local_file("/tmp/second.log");
        let tree = LoadedLogTree {
            summary: "3 个节点".to_string(),
            rows: vec![
                test_tree_row(0, 0, LogTreeEntryKind::Directory, true, None),
                test_tree_row(1, 1, LogTreeEntryKind::File, false, Some(first)),
                test_tree_row(2, 1, LogTreeEntryKind::File, false, Some(second)),
            ],
            error_count: 0,
            temporary_paths: Vec::new(),
        };
        let state = LoadedLogTreeState::new(tree);
        let selected = HashSet::from([0, 2]);
        let files = state.plugin_log_files_for_menu(&selected, 0, None);
        let names = files
            .into_iter()
            .map(|file| file.display_name)
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["first.log", "second.log"]);
    }

    /// 验证插件日志树菜单在当前选择无候选时使用右键落点兜底。
    ///
    /// 业务意图：
    /// - 右键未选中行或选择集中只有错误节点时，插件菜单仍应能对右键落点文件执行。
    #[test]
    fn 插件日志树菜单选择无候选时使用右键兜底文件() {
        let fallback = test_local_file("/tmp/fallback.log");
        let tree = LoadedLogTree {
            summary: "2 个节点".to_string(),
            rows: vec![
                test_tree_row(0, 0, LogTreeEntryKind::Error, false, None),
                test_tree_row(1, 0, LogTreeEntryKind::File, false, Some(fallback.clone())),
            ],
            error_count: 1,
            temporary_paths: Vec::new(),
        };
        let state = LoadedLogTreeState::new(tree);
        let selected = HashSet::from([0]);
        let files = state.plugin_log_files_for_menu(&selected, 1, Some(&fallback));

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].display_name, "fallback.log");
    }

    /// 验证空文件名搜索保持普通展开/折叠可见行规则。
    ///
    /// 业务意图：
    /// - 清空搜索后必须回到用户原本的目录树展开状态，不能把搜索期间的临时路径展开固化到 UI 状态。
    #[test]
    fn 文件树空搜索保持普通可见行规则() {
        let tree = LoadedLogTree {
            summary: "3 个节点".to_string(),
            rows: vec![
                test_tree_row(0, 0, LogTreeEntryKind::Directory, true, None),
                test_tree_row(1, 1, LogTreeEntryKind::Directory, true, None),
                {
                    let mut row = test_tree_row(
                        2,
                        2,
                        LogTreeEntryKind::File,
                        false,
                        Some(test_local_file("/tmp/nested/app.log")),
                    );
                    row.label = "app.log".to_string();
                    row
                },
            ],
            error_count: 0,
            temporary_paths: Vec::new(),
        };
        let mut state = LoadedLogTreeState::new(tree);
        state.expanded_node_ids.clear();
        state.rebuild_visible_rows();

        let visible_before_search: Vec<_> = state.visible_rows.iter().map(|row| row.id).collect();
        let matches = state.rebuild_visible_rows_for_file_name_search("   ");
        let visible_after_search: Vec<_> = state.visible_rows.iter().map(|row| row.id).collect();

        assert!(matches.is_empty());
        assert_eq!(visible_before_search, vec![0]);
        assert_eq!(visible_after_search, visible_before_search);
    }

    /// 验证文件名搜索只把可打开文件作为命中，目录名称只作为上下文展示。
    ///
    /// 业务意图：
    /// - 左侧搜索用于定位具体日志文件；目录、压缩包和错误节点不应因为名称匹配而进入命中计数。
    #[test]
    fn 文件树搜索只命中文件节点并展示父级上下文() {
        let mut root = test_tree_row(0, 0, LogTreeEntryKind::Directory, true, None);
        root.label = "error-root".to_string();
        let mut nested = test_tree_row(1, 1, LogTreeEntryKind::Directory, true, None);
        nested.label = "service".to_string();
        let mut matched_file = test_tree_row(
            2,
            2,
            LogTreeEntryKind::File,
            false,
            Some(test_local_file("/tmp/service/error.log")),
        );
        matched_file.label = "error.log".to_string();
        let mut other_file = test_tree_row(
            3,
            2,
            LogTreeEntryKind::File,
            false,
            Some(test_local_file("/tmp/service/access.log")),
        );
        other_file.label = "access.log".to_string();
        let mut directory_only_match =
            test_tree_row(4, 1, LogTreeEntryKind::Directory, false, None);
        directory_only_match.label = "error-directory".to_string();
        let mut unopened_file_match = test_tree_row(5, 1, LogTreeEntryKind::File, false, None);
        unopened_file_match.label = "error-without-source.log".to_string();
        let tree = LoadedLogTree {
            summary: "6 个节点".to_string(),
            rows: vec![
                root,
                nested,
                matched_file,
                other_file,
                directory_only_match,
                unopened_file_match,
            ],
            error_count: 0,
            temporary_paths: Vec::new(),
        };
        let mut state = LoadedLogTreeState::new(tree);

        let matches = state.rebuild_visible_rows_for_file_name_search("ERROR");
        let visible_ids: Vec<_> = state.visible_rows.iter().map(|row| row.id).collect();

        assert_eq!(matches, vec![2]);
        assert_eq!(visible_ids, vec![0, 1, 2]);
    }

    /// 验证清空文件名搜索后恢复用户原有展开状态。
    ///
    /// 业务意图：
    /// - 搜索过滤需要临时显示命中路径，但用户手动收起的目录不能因为搜索结束而被意外展开。
    #[test]
    fn 文件树搜索清空后恢复原展开状态() {
        let mut root = test_tree_row(0, 0, LogTreeEntryKind::Directory, true, None);
        root.label = "root".to_string();
        let mut nested = test_tree_row(1, 1, LogTreeEntryKind::Directory, true, None);
        nested.label = "nested".to_string();
        let mut matched_file = test_tree_row(
            2,
            2,
            LogTreeEntryKind::File,
            false,
            Some(test_local_file("/tmp/nested/error.log")),
        );
        matched_file.label = "error.log".to_string();
        let tree = LoadedLogTree {
            summary: "3 个节点".to_string(),
            rows: vec![root, nested, matched_file],
            error_count: 0,
            temporary_paths: Vec::new(),
        };
        let mut state = LoadedLogTreeState::new(tree);
        state.expanded_node_ids.clear();
        state.rebuild_visible_rows();
        assert_eq!(
            state
                .visible_rows
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            vec![0]
        );

        let matches = state.rebuild_visible_rows_for_file_name_search("error");
        assert_eq!(matches, vec![2]);
        assert_eq!(
            state
                .visible_rows
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );

        let cleared_matches = state.rebuild_visible_rows_for_file_name_search("");
        assert!(cleared_matches.is_empty());
        assert_eq!(
            state
                .visible_rows
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            vec![0]
        );
    }

    /// 验证文件树搜索命中导航采用循环定位。
    ///
    /// 业务意图：
    /// - 上一个/下一个按钮在多命中之间循环，用户不需要在首尾额外判断方向。
    #[test]
    fn 文件树搜索命中导航循环定位() {
        assert_eq!(
            LoadedLogTreeState::next_file_name_search_match_index(
                None,
                3,
                LogTreeSearchNavigation::Next,
            ),
            Some(0)
        );
        assert_eq!(
            LoadedLogTreeState::next_file_name_search_match_index(
                Some(2),
                3,
                LogTreeSearchNavigation::Next,
            ),
            Some(0)
        );
        assert_eq!(
            LoadedLogTreeState::next_file_name_search_match_index(
                Some(0),
                3,
                LogTreeSearchNavigation::Previous,
            ),
            Some(2)
        );
        assert_eq!(
            LoadedLogTreeState::next_file_name_search_match_index(
                Some(4),
                3,
                LogTreeSearchNavigation::Previous,
            ),
            Some(0)
        );
        assert_eq!(
            LoadedLogTreeState::next_file_name_search_match_index(
                None,
                0,
                LogTreeSearchNavigation::Next,
            ),
            None
        );
    }

    /// 验证文件树搜索关键字高亮范围使用原始文件名下标。
    ///
    /// 业务意图：
    /// - 左侧树搜索结果需要只高亮文件名里的关键字，不应再把整行作为搜索命中背景。
    /// - 搜索匹配不区分大小写，多次出现时每个命中片段都应能被渲染层单独高亮。
    #[test]
    fn 文件树搜索关键字高亮范围支持大小写和多次命中() {
        let ranges =
            LoadedLogTreeState::file_name_search_match_ranges("Error.ERROR.log", " error ");

        assert_eq!(ranges, vec![0..5, 6..11]);
    }

    /// 验证文件树搜索关键字高亮范围不会切断中文字符。
    ///
    /// 业务意图：
    /// - 文件名可能包含中文，传给 `StyledText` 的高亮范围必须保持 UTF-8 边界，否则渲染会 panic 或高亮错位。
    #[test]
    fn 文件树搜索关键字高亮范围支持中文文件名() {
        let label = "服务日志.log";
        let expected_start = label.find("日志").expect("测试文件名必须包含日志关键字");
        let expected_end = expected_start + "日志".len();

        let ranges = LoadedLogTreeState::file_name_search_match_ranges(label, "日志");

        assert_eq!(ranges, vec![expected_start..expected_end]);
    }

    /// 验证平台传入的 file URL 会解析成本地路径。
    ///
    /// 业务意图：
    /// - macOS 把文件拖到应用图标时通常给 `on_open_urls` 传 `file://` URL，必须能还原空格和中文路径。
    #[test]
    fn open_url_文件路径会被解析为本地路径() {
        let paths = log_source_paths_from_open_urls(vec![
            "file:///Users/test/日志%20目录/app.log".to_string(),
            "logclinic://ignored".to_string(),
        ]);

        assert_eq!(paths, vec![PathBuf::from("/Users/test/日志 目录/app.log")]);
    }

    /// 验证裸路径也能作为平台 open-url 输入兼容处理。
    ///
    /// 业务意图：
    /// - 不同平台和打包方式可能直接传普通路径；兼容裸路径可以减少启动入口差异。
    #[test]
    fn open_url_裸路径会被保留() {
        let paths = log_source_paths_from_open_urls(vec!["/tmp/app.log".to_string()]);

        assert_eq!(paths, vec![PathBuf::from("/tmp/app.log")]);
    }
}
