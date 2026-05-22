// 原生窗口外观适配。
//
// 业务意图：
// - 应用内部支持“跟随系统 / 亮色 / 暗色”三种主题偏好；GPUI 内容区可以直接按调色板重绘，
//   但 macOS 原生标题栏默认只跟随系统外观，不会跟随应用内强制主题。
// - 本模块只在 macOS 上把主题偏好同步到 `NSApplication.appearance`，让主窗口、设置窗口和文件管理窗口的标题栏
//   与应用内容区保持一致；其它平台继续使用系统窗口管理器的默认外观。
//
// 跨平台约束：
// - Windows 不使用 AppKit，因此该模块在非 macOS 平台是空操作，避免引入平台条件散落到业务 UI。
// - `ThemePreference::System` 必须把原生 appearance 置空，交还给系统外观，不能固化为当前亮色或暗色。

use crate::theme::ThemePreference;

/// 将应用主题偏好同步到原生窗口系统。
///
/// 业务意图：
/// - 当用户切换主题或应用启动读取到已保存主题时，原生标题栏应该和 GPUI 内容区使用同一亮暗模式。
/// - 该函数不返回错误；如果平台 API 暂不可用，内容区主题仍正常生效，标题栏退回系统默认外观。
pub(in crate::app) fn apply_native_theme_preference(preference: ThemePreference) {
    apply_native_theme_preference_impl(preference);
}

/// macOS 以外平台的空实现。
///
/// 边界条件：
/// - Windows 标题栏颜色由系统和 GPUI 窗口装饰共同决定，当前没有安全的跨版本强制 API；保守保持原行为。
#[cfg(not(target_os = "macos"))]
fn apply_native_theme_preference_impl(_preference: ThemePreference) {}

/// macOS 原生标题栏外观同步实现。
///
/// 实现原因：
/// - GPUI 0.2.2 只暴露窗口外观读取接口，没有提供设置原生 `NSAppearance` 的公开方法。
/// - 直接设置 `NSApplication.appearance` 会影响后续和现有窗口的标题栏，同时 `None` 可以恢复跟随系统。
#[cfg(target_os = "macos")]
fn apply_native_theme_preference_impl(preference: ThemePreference) {
    use cocoa::{
        appkit::{NSApp, NSAppearance},
        base::{id, nil},
    };
    use objc::runtime::Sel;

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        static NSAppearanceNameAqua: id;
        static NSAppearanceNameDarkAqua: id;
    }
    #[link(name = "objc")]
    unsafe extern "C" {
        #[link_name = "objc_msgSend"]
        fn objc_msg_send_set_appearance(receiver: id, selector: Sel, appearance: id);
    }

    // SAFETY:
    // - 该函数只在 macOS 主线程的 GPUI 事件流中调用，`NSApp` 和 `setAppearance:` 是 AppKit 主线程 API。
    // - `ThemePreference::System` 传入 nil 是 AppKit 官方语义，表示恢复继承系统外观。
    unsafe {
        let app = NSApp();
        if app == nil {
            return;
        }
        let appearance = match preference {
            ThemePreference::Light => NSAppearance(NSAppearanceNameAqua),
            ThemePreference::Dark => NSAppearance(NSAppearanceNameDarkAqua),
            ThemePreference::System => nil,
        };
        objc_msg_send_set_appearance(app, Sel::register("setAppearance:"), appearance);
    }
}
