// 应用层纯状态测试。
//
// 业务意图：
// - 该文件从历史 `app.rs` 底部拆出，保持测试仍在 `app` 模块作用域内，避免第一阶段重构改变私有辅助函数测试方式。
// - 后续当功能域升级为真正子模块时，再把测试逐步迁移到对应模块内。

use super::*;

#[cfg(test)]
mod tests {
    //! 主界面纯状态逻辑测试。
    //!
    //! 业务意图：
    //! - GPUI 渲染交互主要依赖手动验收，但目录树状态这类纯数据规则可以通过单元测试锁定。
    //! - 本模块只验证不需要窗口系统的行为，避免测试环境依赖 macOS 或 Windows 图形能力。

    use crate::log_loader::ArchiveFormat;

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
        assert_eq!(MainView::clamp_search_text_range("a中b", 4..0), 0..4);
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

    /// 验证搜索关键字历史按最近使用排序并限制数量。
    ///
    /// 业务意图：
    /// - 搜索窗口没有日志选区时会用最近关键字预填；历史顺序和上限错误会直接影响重复搜索体验。
    /// - 该测试只覆盖纯状态逻辑，不依赖 GPUI 窗口或文本输入系统。
    #[test]
    fn 搜索关键字历史按最近使用排序并限制数量() {
        let mut history = Vec::new();
        for index in 0..12 {
            MainView::remember_search_query_in_history(&mut history, &format!("key-{index}"));
        }

        assert_eq!(history.len(), SEARCH_QUERY_HISTORY_LIMIT);
        assert_eq!(history.first().map(String::as_str), Some("key-11"));
        assert_eq!(history.last().map(String::as_str), Some("key-2"));

        MainView::remember_search_query_in_history(&mut history, "key-5");
        assert_eq!(history.len(), SEARCH_QUERY_HISTORY_LIMIT);
        assert_eq!(history.first().map(String::as_str), Some("key-5"));
        assert_eq!(history.iter().filter(|query| *query == "key-5").count(), 1);

        MainView::remember_search_query_in_history(&mut history, "   ");
        assert_eq!(history.len(), SEARCH_QUERY_HISTORY_LIMIT);
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
            query: "日志选中文本".to_string(),
            selection_range: "日志选中文本".len().."日志选中文本".len(),
            marked_range: None,
            scope: SearchScope::CurrentFile,
            directory_target: String::new(),
            directory_selection_range: 0..0,
            directory_marked_range: None,
            case_sensitive: false,
            current_file_match_count: Some(3),
            is_searching: false,
            progress: SearchProgress::default(),
            message: String::new(),
            job_id: 0,
        };

        MainView::replace_search_query_with_clipboard_text(&mut dialog, "剪贴板文本".to_string());

        assert_eq!(dialog.query, "剪贴板文本");
        assert_eq!(
            dialog.selection_range,
            "剪贴板文本".len().."剪贴板文本".len()
        );
        assert!(dialog.marked_range.is_none());
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
        ];

        let source = LogFileSource::LocalFile {
            path: PathBuf::from("thread.log"),
        };
        let snapshots = MainView::parse_thread_dump_snapshots(&lines, "thread.log", 0, &source);
        let analysis = MainView::build_thread_analysis_data(1, 0, snapshots);

        assert_eq!(analysis.snapshots.len(), 1);
        assert_eq!(analysis.snapshots[0].label, "2026-05-07 11:01:10");
        assert_eq!(analysis.thread_names, vec!["pool-1-thread-1", "worker"]);
        let first_cell = analysis.matrix[0][0]
            .as_ref()
            .expect("第一个线程应形成可点击时间线色块");
        assert_eq!(first_cell.state, ThreadStateKind::Runnable);
        assert_eq!(first_cell.thread_id.as_deref(), Some("#1"));
        assert_eq!(first_cell.line_index, 2);
        assert_eq!(first_cell.preview_lines.len(), 2);
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
                    },
                    ThreadStateSample {
                        name: "only-a".to_string(),
                        thread_id: Some("#2".to_string()),
                        state: ThreadStateKind::Waiting,
                        line_index: 1,
                        preview_lines: vec!["\"only-a\" #2".to_string()],
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
                    },
                    ThreadStateSample {
                        name: "only-b".to_string(),
                        thread_id: Some("#3".to_string()),
                        state: ThreadStateKind::Runnable,
                        line_index: 1,
                        preview_lines: vec!["\"only-b\" #3".to_string()],
                    },
                ],
            },
        ];

        let analysis = MainView::build_thread_analysis_data(2, 0, snapshots);

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

    /// 验证线程分析气泡在窗口右下角会自动改为向左上方弹出。
    ///
    /// 业务意图：
    /// - 用户点击靠近窗口边缘的状态色块时，气泡不能被窗口裁切，否则关键线程 ID 和预览日志不可见。
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
    /// - 非边缘区域保留靠近点击点的默认方向，让用户能直接把气泡和刚点击的色块关联起来。
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
            raw_bytes: Some(Arc::new(b"hello".to_vec())),
            state: LogTabState::Ready {
                document: LogTabDocument::InMemory(document),
            },
            scroll_handle: UniformListScrollHandle::new(),
            pending_scroll_to_line: None,
            highlighted_search_line: None,
            text_selection: None,
            selection_drag_anchor: None,
        };

        assert_eq!(MainView::log_tab_encoding_selector_label(&tab), "GBK");
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
                raw_bytes: None,
                state: LogTabState::Loading {
                    message: "测试加载中".to_string(),
                },
                scroll_handle: UniformListScrollHandle::new(),
                pending_scroll_to_line: None,
                highlighted_search_line: None,
                text_selection: None,
                selection_drag_anchor: None,
            },
            OpenLogTab {
                id: 20,
                source: second_source.clone(),
                source_key: "local:second.log".to_string(),
                title: "second.log".to_string(),
                encoding_choice: EncodingChoice::Auto,
                raw_bytes: None,
                state: LogTabState::Loading {
                    message: "测试加载中".to_string(),
                },
                scroll_handle: UniformListScrollHandle::new(),
                pending_scroll_to_line: None,
                highlighted_search_line: None,
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
