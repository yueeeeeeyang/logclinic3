//! 系统右键菜单集成功能域。
//!
//! 业务意图：
//! - LogClinic 需要在 Windows 资源管理器和 macOS Finder 中作为“用 LogClinic 打开”的候选应用出现。
//! - 该模块集中平台副作用，UI 只调用查询、注册和卸载入口，不直接理解注册表、LaunchServices 或 `.app` 包结构。
//!
//! 跨平台约束：
//! - Windows 写入 `HKCU\Software\Classes`，只影响当前用户，不需要管理员权限，也不修改系统级文件关联。
//! - macOS 通过 LaunchServices 注册当前 `.app` bundle；Finder 顶层右键扩展不在本次范围内，入口显示在“打开方式”菜单中。
//! - 非目标平台返回不可用状态，避免后续开发期在 Linux 上误执行平台命令。

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(any(test, target_os = "macos"))]
use std::fs;

/// LogClinic 在 Windows 系统菜单中的中文展示文案。
#[cfg(target_os = "windows")]
const SHELL_MENU_TEXT: &str = "用 LogClinic 打开";

/// macOS 系统自带 LaunchServices 注册工具路径。
const MACOS_LSREGISTER_PATH: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

/// macOS `Info.plist` 中用于标记新版 Finder“打开方式”UTI 声明的类型名。
#[cfg(any(test, target_os = "macos"))]
const MACOS_SUPPORTED_DOCUMENTS_TYPE_NAME: &str = "LogClinic Supported Documents";

/// macOS `Info.plist` 中用于标记扩展名兜底声明的类型名。
#[cfg(any(test, target_os = "macos"))]
const MACOS_EXTENSION_FALLBACKS_TYPE_NAME: &str = "LogClinic Extension Fallbacks";

/// Windows 当前用户“所有文件”右键菜单根键。
#[cfg(target_os = "windows")]
const WINDOWS_SHELL_MENU_KEY: &str = r"HKCU\Software\Classes\*\shell\LogClinic";

/// Windows 当前用户“所有文件”右键菜单命令键。
#[cfg(target_os = "windows")]
const WINDOWS_SHELL_COMMAND_KEY: &str = r"HKCU\Software\Classes\*\shell\LogClinic\command";

/// 系统右键集成的当前状态。
///
/// 业务意图：
/// - 设置页需要展示“已注册 / 未注册 / 当前环境不可用”三类互斥状态。
/// - 状态携带中文说明，避免 UI 再拼接平台细节。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ShellIntegrationStatus {
    /// 当前平台入口已注册。
    Registered {
        /// 面向用户的中文状态说明。
        message: String,
    },
    /// 当前平台入口未注册。
    NotRegistered {
        /// 面向用户的中文状态说明。
        message: String,
    },
    /// 当前环境无法执行注册，例如 macOS 开发期不是 `.app` 包内运行。
    Unavailable {
        /// 面向用户的中文状态说明。
        message: String,
    },
}

impl ShellIntegrationStatus {
    /// 返回状态是否表示已注册。
    pub(crate) fn is_registered(&self) -> bool {
        matches!(self, Self::Registered { .. })
    }

    /// 返回状态是否允许尝试注册或卸载。
    pub(crate) fn is_available(&self) -> bool {
        !matches!(self, Self::Unavailable { .. })
    }

    /// 返回可直接展示在设置页中的中文状态文本。
    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Registered { message }
            | Self::NotRegistered { message }
            | Self::Unavailable { message } => message,
        }
    }
}

/// 查询当前平台的右键菜单注册状态。
///
/// 边界条件：
/// - 查询失败不会 panic，而是返回不可用或未注册状态；设置页可以继续允许用户点击注册重试。
pub(crate) fn query_shell_integration_status() -> ShellIntegrationStatus {
    query_platform_shell_integration_status()
}

/// 注册当前平台的右键菜单入口。
///
/// 错误处理：
/// - 失败时返回中文错误，UI 将其展示在通用设置页，避免用户只能从 stderr 排查权限或平台问题。
pub(crate) fn register_shell_integration() -> Result<ShellIntegrationStatus, String> {
    register_platform_shell_integration()
}

/// 卸载当前平台的右键菜单入口。
///
/// 错误处理：
/// - 卸载不存在的入口按成功处理并返回未注册状态，避免重复点击卸载变成无意义错误。
pub(crate) fn unregister_shell_integration() -> Result<ShellIntegrationStatus, String> {
    unregister_platform_shell_integration()
}

#[cfg(target_os = "windows")]
fn query_platform_shell_integration_status() -> ShellIntegrationStatus {
    let exe_path = match env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            return ShellIntegrationStatus::Unavailable {
                message: format!("无法定位当前 LogClinic.exe：{error}"),
            };
        }
    };

    match windows_registered_for_executable(&exe_path) {
        Ok(true) => ShellIntegrationStatus::Registered {
            message: "已注册：资源管理器右键所有文件可用 LogClinic 打开".to_string(),
        },
        Ok(false) => ShellIntegrationStatus::NotRegistered {
            message: "未注册：资源管理器右键菜单中不会显示 LogClinic".to_string(),
        },
        Err(error) => ShellIntegrationStatus::Unavailable { message: error },
    }
}

#[cfg(target_os = "windows")]
fn register_platform_shell_integration() -> Result<ShellIntegrationStatus, String> {
    let exe_path =
        env::current_exe().map_err(|error| format!("无法定位当前 LogClinic.exe：{error}"))?;
    let entries = windows_registry_entries_for_executable(&exe_path);

    run_reg_command(&[
        "add",
        WINDOWS_SHELL_MENU_KEY,
        "/ve",
        "/d",
        SHELL_MENU_TEXT,
        "/f",
    ])?;
    run_reg_command(&[
        "add",
        WINDOWS_SHELL_MENU_KEY,
        "/v",
        "Icon",
        "/d",
        &entries.icon_value,
        "/f",
    ])?;
    run_reg_command(&[
        "add",
        WINDOWS_SHELL_COMMAND_KEY,
        "/ve",
        "/d",
        &entries.command_value,
        "/f",
    ])?;

    Ok(ShellIntegrationStatus::Registered {
        message: "已注册：资源管理器右键所有文件可用 LogClinic 打开".to_string(),
    })
}

#[cfg(target_os = "windows")]
fn unregister_platform_shell_integration() -> Result<ShellIntegrationStatus, String> {
    if !windows_registry_key_exists(WINDOWS_SHELL_MENU_KEY)? {
        return Ok(ShellIntegrationStatus::NotRegistered {
            message: "已卸载：资源管理器右键菜单中不会显示 LogClinic".to_string(),
        });
    }

    let output = Command::new("reg")
        .args(["delete", WINDOWS_SHELL_MENU_KEY, "/f"])
        .output()
        .map_err(|error| format!("无法启动 reg.exe 删除右键菜单：{error}"))?;
    if !output.status.success() {
        // 删除和查询之间存在极短竞态：如果用户或系统清理工具刚好移除了 key，二次查询确认不存在即可按卸载成功处理。
        if let Ok(false) = windows_registry_key_exists(WINDOWS_SHELL_MENU_KEY) {
            return Ok(ShellIntegrationStatus::NotRegistered {
                message: "已卸载：资源管理器右键菜单中不会显示 LogClinic".to_string(),
            });
        }
        return Err(command_failure_message("reg delete", &output));
    }

    Ok(ShellIntegrationStatus::NotRegistered {
        message: "已卸载：资源管理器右键菜单中不会显示 LogClinic".to_string(),
    })
}

#[cfg(target_os = "macos")]
fn query_platform_shell_integration_status() -> ShellIntegrationStatus {
    let bundle_path = match current_macos_app_bundle() {
        Ok(path) => path,
        Err(message) => {
            return ShellIntegrationStatus::Unavailable { message };
        }
    };
    match macos_bundle_declares_open_with_support(&bundle_path) {
        Ok(true) => {}
        Ok(false) => {
            return ShellIntegrationStatus::NotRegistered {
                message:
                    "当前 LogClinic.app 缺少 Finder“打开方式”文件类型声明，请重新打包后再注册。"
                        .to_string(),
            };
        }
        Err(message) => return ShellIntegrationStatus::Unavailable { message },
    }

    match macos_bundle_registered(&bundle_path) {
        Ok(true) => ShellIntegrationStatus::Registered {
            message: "已注册：Finder 右键“打开方式”中可选择 LogClinic".to_string(),
        },
        Ok(false) => ShellIntegrationStatus::NotRegistered {
            message: "未注册：Finder 右键“打开方式”中可能不会显示 LogClinic".to_string(),
        },
        Err(error) => ShellIntegrationStatus::Unavailable { message: error },
    }
}

#[cfg(target_os = "macos")]
fn register_platform_shell_integration() -> Result<ShellIntegrationStatus, String> {
    let bundle_path = current_macos_app_bundle()?;
    if !macos_bundle_declares_open_with_support(&bundle_path)? {
        return Err(
            "当前 LogClinic.app 缺少 Finder“打开方式”文件类型声明，请用最新打包脚本重新生成 .app 后再注册。"
                .to_string(),
        );
    }
    run_macos_lsregister(macos_lsregister_register_args(&bundle_path))?;
    Ok(ShellIntegrationStatus::Registered {
        message: "已注册：Finder 右键“打开方式”中可选择 LogClinic".to_string(),
    })
}

#[cfg(target_os = "macos")]
fn unregister_platform_shell_integration() -> Result<ShellIntegrationStatus, String> {
    let bundle_path = current_macos_app_bundle()?;
    run_macos_lsregister(macos_lsregister_unregister_args(&bundle_path))?;
    Ok(ShellIntegrationStatus::NotRegistered {
        message: "已卸载：Finder 右键“打开方式”中不再主动注册 LogClinic".to_string(),
    })
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn query_platform_shell_integration_status() -> ShellIntegrationStatus {
    ShellIntegrationStatus::Unavailable {
        message: "当前平台暂不支持系统右键菜单注册".to_string(),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn register_platform_shell_integration() -> Result<ShellIntegrationStatus, String> {
    Err("当前平台暂不支持系统右键菜单注册".to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn unregister_platform_shell_integration() -> Result<ShellIntegrationStatus, String> {
    Err("当前平台暂不支持系统右键菜单注册".to_string())
}

/// Windows 右键菜单注册表值集合。
///
/// 业务意图：
/// - 把待写入注册表的字符串集中构造，便于测试覆盖路径包含空格时的命令引用规则。
#[cfg(any(test, target_os = "windows"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WindowsRegistryEntries {
    /// `Icon` 值，让资源管理器菜单显示 LogClinic 可执行文件图标。
    pub(crate) icon_value: String,
    /// `command` 默认值，`%1` 由资源管理器替换为用户右键的文件路径。
    pub(crate) command_value: String,
}

/// 构造 Windows 右键菜单注册表值。
///
/// 边界条件：
/// - 可执行文件路径必须加双引号，文件占位符 `%1` 也必须加双引号，才能支持空格、中文和括号等常见路径字符。
#[cfg(any(test, target_os = "windows"))]
pub(crate) fn windows_registry_entries_for_executable(exe_path: &Path) -> WindowsRegistryEntries {
    let exe = exe_path.display();
    WindowsRegistryEntries {
        icon_value: format!("{exe},0"),
        command_value: format!("\"{exe}\" \"%1\""),
    }
}

#[cfg(target_os = "windows")]
fn windows_registered_for_executable(exe_path: &Path) -> Result<bool, String> {
    let output = Command::new("reg")
        .args(["query", WINDOWS_SHELL_COMMAND_KEY, "/ve"])
        .output()
        .map_err(|error| format!("无法启动 reg.exe 查询右键菜单：{error}"))?;
    if !output.status.success() {
        return Ok(false);
    }

    let expected_command = windows_registry_entries_for_executable(exe_path).command_value;
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.contains(&expected_command))
}

/// 查询 Windows 注册表 key 是否存在。
///
/// 业务意图：
/// - 卸载右键菜单时只需要知道 key 是否存在，不能解析 `reg.exe` 的本地化输出，否则中文 Windows 的控制台编码会影响判断。
///
/// 边界条件：
/// - `reg query` 退出成功视为存在，退出失败视为不存在；无法启动 `reg.exe` 才返回可展示错误。
#[cfg(target_os = "windows")]
fn windows_registry_key_exists(key: &str) -> Result<bool, String> {
    let output = Command::new("reg")
        .args(["query", key])
        .output()
        .map_err(|error| format!("无法启动 reg.exe 查询右键菜单：{error}"))?;
    Ok(output.status.success())
}

#[cfg(target_os = "windows")]
fn run_reg_command(args: &[&str]) -> Result<(), String> {
    let output = Command::new("reg")
        .args(args)
        .output()
        .map_err(|error| format!("无法启动 reg.exe 写入右键菜单：{error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_failure_message("reg", &output))
    }
}

#[cfg(target_os = "macos")]
fn current_macos_app_bundle() -> Result<PathBuf, String> {
    let exe_path =
        env::current_exe().map_err(|error| format!("无法定位当前应用可执行文件：{error}"))?;
    for ancestor in exe_path.ancestors() {
        if ancestor
            .extension()
            .is_some_and(|extension| extension == "app")
        {
            let info_plist = ancestor.join("Contents").join("Info.plist");
            if info_plist.is_file() {
                return Ok(ancestor.to_path_buf());
            }
        }
    }

    Err("当前程序不在 LogClinic.app 内，右键菜单注册需要使用打包后的 macOS 应用。".to_string())
}

/// 构造 macOS 注册命令参数。
///
/// 业务意图：
/// - `-f` 强制刷新 LaunchServices 中同一路径的旧声明，适合用户覆盖安装或替换版本后重新注册。
#[cfg(any(test, target_os = "macos"))]
pub(crate) fn macos_lsregister_register_args(bundle_path: &Path) -> Vec<String> {
    vec!["-f".to_string(), bundle_path.to_string_lossy().into_owned()]
}

/// 构造 macOS 卸载命令参数。
#[cfg(any(test, target_os = "macos"))]
pub(crate) fn macos_lsregister_unregister_args(bundle_path: &Path) -> Vec<String> {
    vec!["-u".to_string(), bundle_path.to_string_lossy().into_owned()]
}

#[cfg(target_os = "macos")]
fn run_macos_lsregister(args: Vec<String>) -> Result<(), String> {
    let output = Command::new(MACOS_LSREGISTER_PATH)
        .args(args)
        .output()
        .map_err(|error| format!("无法启动 LaunchServices 注册工具：{error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_failure_message("lsregister", &output))
    }
}

#[cfg(target_os = "macos")]
fn macos_bundle_registered(bundle_path: &Path) -> Result<bool, String> {
    let output = Command::new(MACOS_LSREGISTER_PATH)
        .args(["-dump"])
        .output()
        .map_err(|error| format!("无法查询 LaunchServices 注册状态：{error}"))?;
    if !output.status.success() {
        return Err(command_failure_message("lsregister -dump", &output));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(macos_lsregister_dump_contains_bundle_path(
        stdout.as_ref(),
        bundle_path,
    ))
}

/// 检查当前 `.app` 是否包含 Finder“打开方式”所需的文档类型声明。
///
/// 业务意图：
/// - LaunchServices 中存在应用路径并不等于 Finder 会把它列入某个文件的“打开方式”候选。
/// - 设置页必须同时确认应用包声明了新版 UTI 匹配和扩展名兜底，否则会出现“已注册但菜单没有”的误导状态。
///
/// 边界条件：
/// - 打包脚本生成的是 XML plist，这里只做轻量文本校验；读取失败按不可用错误展示，让用户知道当前应用包不完整。
#[cfg(any(test, target_os = "macos"))]
fn macos_bundle_declares_open_with_support(bundle_path: &Path) -> Result<bool, String> {
    let info_plist_path = bundle_path.join("Contents").join("Info.plist");
    let info_plist = fs::read_to_string(&info_plist_path).map_err(|error| {
        format!(
            "无法读取当前应用的 Info.plist（{}）：{error}",
            info_plist_path.display()
        )
    })?;
    Ok(macos_info_plist_declares_open_with_support(&info_plist))
}

/// 判断 `Info.plist` 文本是否包含新版 Finder“打开方式”声明。
///
/// 业务意图：
/// - `public.data` 过于宽泛，实际 Finder 菜单可能不会展示；新版声明必须包含文本、配置、压缩包常用 UTI 和扩展名兜底。
#[cfg(any(test, target_os = "macos"))]
fn macos_info_plist_declares_open_with_support(info_plist: &str) -> bool {
    info_plist.contains("<key>CFBundleDocumentTypes</key>")
        && info_plist.contains(MACOS_SUPPORTED_DOCUMENTS_TYPE_NAME)
        && info_plist.contains(MACOS_EXTENSION_FALLBACKS_TYPE_NAME)
        && info_plist.contains("<string>public.text</string>")
        && info_plist.contains("<string>public.zip-archive</string>")
        && info_plist.contains("<string>public.item</string>")
        && info_plist.contains("<key>CFBundleTypeExtensions</key>")
        && info_plist.contains("<string>*</string>")
        && info_plist.contains("<string>log</string>")
        && info_plist.contains("<string>zip</string>")
}

/// 判断 LaunchServices dump 是否包含当前运行的 `.app` 路径。
///
/// 业务意图：
/// - 用户机器上可能同时存在多份同 bundle identifier 的 LogClinic.app；设置页状态必须反映当前包路径，而不是任意旧副本。
#[cfg(any(test, target_os = "macos"))]
fn macos_lsregister_dump_contains_bundle_path(dump: &str, bundle_path: &Path) -> bool {
    let bundle_text = bundle_path.to_string_lossy();
    dump.contains(bundle_text.as_ref())
}

/// 构造外部平台命令失败时的中文错误。
///
/// 边界条件：
/// - macOS/Windows 命令可能把错误写到 stdout 或 stderr；这里合并后裁剪，避免设置页被长输出撑破。
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn command_failure_message(command: &str, output: &std::process::Output) -> String {
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let normalized = combined.replace(['\r', '\n'], " ");
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return format!("{command} 执行失败，退出码 {:?}", output.status.code());
    }
    let snippet: String = trimmed.chars().take(180).collect();
    format!("{command} 执行失败：{snippet}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 Windows 注册命令会同时引用可执行文件和右键文件占位符。
    ///
    /// 业务意图：
    /// - 资源管理器把带空格路径传入 `%1`，缺少双引号会导致 LogClinic 收到截断路径。
    #[test]
    fn windows_注册表命令会引用路径和文件占位符() {
        let entries = windows_registry_entries_for_executable(Path::new(
            r"C:\Program Files\LogClinic\LogClinic.exe",
        ));

        assert_eq!(
            entries.command_value,
            r#""C:\Program Files\LogClinic\LogClinic.exe" "%1""#
        );
        assert_eq!(
            entries.icon_value,
            r"C:\Program Files\LogClinic\LogClinic.exe,0"
        );
    }

    /// 验证 macOS 注册和卸载命令参数固定可预测。
    ///
    /// 业务意图：
    /// - 设置页只负责触发注册/卸载，命令参数必须由平台模块统一构造，避免 UI 层硬编码 LaunchServices 细节。
    #[test]
    fn macos_lsregister_参数固定包含应用包路径() {
        let bundle_path = Path::new("/Applications/LogClinic.app");

        assert_eq!(
            macos_lsregister_register_args(bundle_path),
            vec!["-f".to_string(), "/Applications/LogClinic.app".to_string()]
        );
        assert_eq!(
            macos_lsregister_unregister_args(bundle_path),
            vec!["-u".to_string(), "/Applications/LogClinic.app".to_string()]
        );
    }

    /// 验证 macOS 注册状态只匹配当前运行的应用包路径。
    ///
    /// 业务意图：
    /// - 用户机器上可能残留旧版 LogClinic.app 的 LaunchServices 记录；状态查询不能仅凭相同 bundle identifier 判定当前包已注册。
    #[test]
    fn macos_注册状态只匹配当前应用包路径() {
        let dump = r#"
bundle id: com.yueyang.logclinic
path: /Applications/LogClinic.app
"#;

        assert!(macos_lsregister_dump_contains_bundle_path(
            dump,
            Path::new("/Applications/LogClinic.app")
        ));
        assert!(!macos_lsregister_dump_contains_bundle_path(
            dump,
            Path::new("/Users/demo/Downloads/LogClinic.app")
        ));
    }

    /// 验证新版 macOS `Info.plist` 声明包含 Finder“打开方式”实际匹配所需的信息。
    ///
    /// 业务意图：
    /// - 单纯声明 `public.data` 时 LaunchServices 可能能看到应用，但 Finder 不一定会把它列入日志文件菜单。
    /// - 测试覆盖文本/压缩包 UTI、全文件兜底和常见日志扩展名，防止打包脚本退回到过窄声明。
    #[test]
    fn macos_info_plist_声明会覆盖_finder_打开方式匹配() {
        let info_plist = r#"
<key>CFBundleDocumentTypes</key>
<array>
  <dict>
    <key>CFBundleTypeName</key>
    <string>LogClinic Supported Documents</string>
    <key>LSItemContentTypes</key>
    <array>
      <string>public.item</string>
      <string>public.text</string>
      <string>public.zip-archive</string>
    </array>
  </dict>
  <dict>
    <key>CFBundleTypeName</key>
    <string>LogClinic Extension Fallbacks</string>
    <key>CFBundleTypeExtensions</key>
    <array>
      <string>*</string>
      <string>log</string>
      <string>zip</string>
    </array>
  </dict>
</array>
"#;

        assert!(macos_info_plist_declares_open_with_support(info_plist));
    }

    /// 验证旧版只声明 `public.data` 的应用包不会继续显示为“已注册”。
    ///
    /// 业务意图：
    /// - 这正是用户反馈的状态误导来源：LaunchServices 已有应用记录，但 Finder“打开方式”菜单没有 LogClinic。
    #[test]
    fn macos_info_plist_只声明_public_data_不会误判可用于_finder() {
        let old_info_plist = r#"
<key>CFBundleDocumentTypes</key>
<array>
  <dict>
    <key>CFBundleTypeName</key>
    <string>All Files</string>
    <key>LSItemContentTypes</key>
    <array>
      <string>public.data</string>
    </array>
  </dict>
</array>
"#;

        assert!(!macos_info_plist_declares_open_with_support(old_info_plist));
    }
}
