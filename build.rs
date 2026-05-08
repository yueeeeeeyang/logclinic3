// LogClinic 构建脚本。
//
// 业务意图：
// - Windows 可执行文件需要在编译阶段嵌入 `.ico` 资源，才能在资源管理器、任务栏和快捷方式中显示程序图标。
// - macOS 图标由 `.app` bundle 的 `Info.plist` 和 `.icns` 资源控制，不在 Rust 编译阶段处理。
//
// 关键约束：
// - 构建脚本只在目标平台为 Windows 时尝试嵌入图标，普通 macOS/Linux `cargo check` 不应依赖 Windows SDK。
// - MSVC target 使用 Windows SDK 的 `rc.exe`；GNU target 使用 `windres`。缺少对应工具时给出明确错误，避免生成没有图标的发布产物。
// - 图标资源路径固定为 `assets/icons/LogClinic.ico`，打包前应先通过 `scripts/generate-app-icons.py` 从源 PNG 重新生成。

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Windows 图标资源路径。
const WINDOWS_ICON_PATH: &str = "assets/icons/LogClinic.ico";

/// 构建脚本入口。
fn main() {
    println!("cargo:rerun-if-changed={WINDOWS_ICON_PATH}");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    if let Err(error) = embed_windows_icon() {
        panic!("嵌入 Windows 程序图标失败：{error}");
    }
}

/// 嵌入 Windows 程序图标。
///
/// 业务意图：
/// - 通过标准 Windows resource 文件把 `.ico` 写进最终 `.exe`，不引入额外 Rust 依赖。
///
/// 边界条件：
/// - `OUT_DIR` 缺失说明 Cargo 构建环境异常，直接返回错误。
/// - 目标环境不是 MSVC/GNU 时不盲目猜测工具链，返回明确错误，防止发布出没有图标的 Windows 程序。
fn embed_windows_icon() -> Result<(), String> {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR")
        .map_err(|error| format!("无法读取 CARGO_MANIFEST_DIR：{error}"))?;
    let out_dir = env::var("OUT_DIR").map_err(|error| format!("无法读取 OUT_DIR：{error}"))?;
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    let icon_path = Path::new(&manifest_dir).join(WINDOWS_ICON_PATH);
    if !icon_path.exists() {
        return Err(format!("图标文件不存在：{}", icon_path.display()));
    }

    let rc_path = Path::new(&out_dir).join("logclinic3-icon.rc");
    let resource_path = Path::new(&out_dir).join(if target_env == "gnu" {
        "logclinic3-icon.o"
    } else {
        "logclinic3-icon.res"
    });

    write_resource_script(&rc_path, &icon_path)?;

    match target_env.as_str() {
        "msvc" => compile_resource_with_rc(&rc_path, &resource_path)?,
        "gnu" => compile_resource_with_windres(&rc_path, &resource_path)?,
        other => {
            return Err(format!(
                "不支持的 Windows target env：{other}，请使用 msvc 或 gnu"
            ));
        }
    }

    println!("cargo:rustc-link-arg-bins={}", resource_path.display());
    Ok(())
}

/// 写入 Windows resource 脚本。
///
/// 业务意图：
/// - resource 脚本只声明一个应用图标，资源 ID 使用 1，符合 Windows 默认应用图标查找习惯。
fn write_resource_script(rc_path: &Path, icon_path: &Path) -> Result<(), String> {
    let escaped_icon_path = escape_windows_resource_path(icon_path);
    fs::write(rc_path, format!("1 ICON \"{escaped_icon_path}\"\n"))
        .map_err(|error| format!("无法写入 resource 脚本 {}：{error}", rc_path.display()))
}

/// 转义 Windows resource 脚本中的路径。
///
/// 边界条件：
/// - Windows 路径可能包含反斜杠或双引号；resource 文件字符串需要显式转义，避免路径被截断。
fn escape_windows_resource_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

/// 使用 Windows SDK 的 `rc.exe` 编译 resource。
///
/// 业务意图：
/// - MSVC target 通常随 Visual Studio Build Tools / Windows SDK 提供 `rc.exe`，生成 `.res` 后交给链接器。
fn compile_resource_with_rc(rc_path: &Path, resource_path: &Path) -> Result<(), String> {
    run_resource_command(
        Command::new("rc.exe")
            .arg("/nologo")
            .arg(format!("/fo{}", resource_path.display()))
            .arg(rc_path),
        "rc.exe",
    )
}

/// 使用 GNU 工具链的 `windres` 编译 resource。
///
/// 业务意图：
/// - GNU target 需要把 resource 编译为 COFF 对象文件，再作为链接参数传给最终 `.exe`。
fn compile_resource_with_windres(rc_path: &Path, resource_path: &Path) -> Result<(), String> {
    run_resource_command(
        Command::new("windres")
            .arg(rc_path)
            .arg("-O")
            .arg("coff")
            .arg("-o")
            .arg(resource_path),
        "windres",
    )
}

/// 执行 resource 编译命令。
///
/// 边界条件：
/// - 命令不存在或返回失败都要中止构建，因为 Windows 发布产物明确要求包含程序图标。
fn run_resource_command(command: &mut Command, tool_name: &str) -> Result<(), String> {
    let status = command.status().map_err(|error| {
        format!("无法执行 {tool_name}，请确认 Windows 资源编译工具已安装：{error}")
    })?;

    if !status.success() {
        return Err(format!("{tool_name} 编译图标资源失败，退出状态：{status}"));
    }

    Ok(())
}
