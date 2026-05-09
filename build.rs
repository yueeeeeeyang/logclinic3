// LogClinic 构建脚本。
//
// 业务意图：
// - Windows 可执行文件需要在编译阶段嵌入 `.ico` 图标和应用 manifest，才能在资源管理器、任务栏和快捷方式中显示程序图标，
//   并让系统加载 Common Controls v6，避免 `TaskDialogIndirect` 等现代控件入口在旧版 comctl32 上解析失败。
// - macOS 图标由 `.app` bundle 的 `Info.plist` 和 `.icns` 资源控制，不在 Rust 编译阶段处理。
//
// 关键约束：
// - 构建脚本只在目标平台为 Windows 时尝试嵌入资源，普通 macOS/Linux `cargo check` 不应依赖 Windows SDK。
// - MSVC target 直接由本构建脚本生成 `.res` 文件，保证 macOS 通过 `cargo-xwin` 交叉编译时不依赖 Windows SDK 的 `rc.exe`。
// - GNU target 仍使用 `windres` 生成 COFF 对象；缺少工具时给出明确错误，避免生成没有图标的发布产物。
// - 图标资源路径固定为 `assets/icons/LogClinic.ico`，打包前应先通过 `scripts/generate-app-icons.py` 从源 PNG 重新生成。

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Windows 图标资源路径。
const WINDOWS_ICON_PATH: &str = "assets/icons/LogClinic.ico";

/// 生成到 `OUT_DIR` 的 Windows manifest 文件名。
const WINDOWS_MANIFEST_FILE_NAME: &str = "logclinic3.exe.manifest";

/// 应用默认图标和 EXE manifest 的资源 ID。
const WINDOWS_APP_RESOURCE_ID: u16 = 1;

/// Windows 资源类型：单个图标图片。
const RT_ICON: u16 = 3;

/// Windows 资源类型：图标组。
const RT_GROUP_ICON: u16 = 14;

/// Windows 资源类型：应用 manifest。
const RT_MANIFEST: u16 = 24;

/// Windows 英文美国资源语言 ID，用于图标和 manifest 这类与界面语言无关的基础资源。
const WINDOWS_RESOURCE_LANGUAGE_ID: u16 = 0x0409;

/// Windows 资源内存标志，和 rc.exe 生成的可移动、纯净、预加载资源保持一致。
const WINDOWS_RESOURCE_MEMORY_FLAGS: u16 = 0x1030;

/// 构建脚本入口。
fn main() {
    println!("cargo:rerun-if-changed={WINDOWS_ICON_PATH}");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    if let Err(error) = embed_windows_resources() {
        panic!("嵌入 Windows 程序资源失败：{error}");
    }
}

/// 嵌入 Windows 程序资源。
///
/// 业务意图：
/// - 通过标准 Windows resource 文件把 `.ico` 和应用 manifest 写进最终 `.exe`，不引入额外 Rust 依赖。
/// - manifest 显式声明 Common Controls v6，保证 Windows 启动时能解析 GPUI 依赖的 `TaskDialogIndirect`。
///
/// 边界条件：
/// - `OUT_DIR` 缺失说明 Cargo 构建环境异常，直接返回错误。
/// - 目标环境不是 MSVC/GNU 时不盲目猜测工具链，返回明确错误，防止发布出没有关键资源的 Windows 程序。
fn embed_windows_resources() -> Result<(), String> {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR")
        .map_err(|error| format!("无法读取 CARGO_MANIFEST_DIR：{error}"))?;
    let out_dir = env::var("OUT_DIR").map_err(|error| format!("无法读取 OUT_DIR：{error}"))?;
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    let icon_path = Path::new(&manifest_dir).join(WINDOWS_ICON_PATH);
    if !icon_path.exists() {
        return Err(format!("图标文件不存在：{}", icon_path.display()));
    }

    let manifest_path = Path::new(&out_dir).join(WINDOWS_MANIFEST_FILE_NAME);
    write_windows_manifest(&manifest_path)?;

    let rc_path = Path::new(&out_dir).join("logclinic3-windows-resources.rc");
    let resource_path = Path::new(&out_dir).join(if target_env == "gnu" {
        "logclinic3-windows-resources.o"
    } else {
        "logclinic3-windows-resources.res"
    });

    write_resource_script(&rc_path, &icon_path, &manifest_path)?;

    match target_env.as_str() {
        "msvc" => write_msvc_windows_resources(&icon_path, &manifest_path, &resource_path)?,
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

/// 写入 Windows 应用 manifest。
///
/// 业务意图：
/// - Common Controls v6 是 `TaskDialogIndirect` 所在的现代控件实现；没有 manifest 时旧版 Windows 可能只加载 comctl32 v5，
///   进而在程序启动加载阶段报“无法定位程序输入点”。
/// - DPI awareness 使用 per-monitor v2，和桌面日志查看器在多显示器、高 DPI 环境下的文本渲染场景一致。
///
/// 边界条件：
/// - manifest 必须内嵌到 EXE，不能依赖旁路文件；用户从微信、浏览器或压缩包中单独复制 EXE 时仍应可启动。
fn write_windows_manifest(manifest_path: &Path) -> Result<(), String> {
    fs::write(manifest_path, windows_manifest_contents()).map_err(|error| {
        format!(
            "无法写入 Windows manifest {}：{error}",
            manifest_path.display()
        )
    })
}

/// 返回 Windows manifest XML 文本。
///
/// 业务意图：
/// - 资源编译器和链接器会按字节原样嵌入该文本；保持固定字符串能让 MSVC 交叉编译和 GNU 原生编译得到一致资源。
fn windows_manifest_contents() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity version="1.0.0.0" processorArchitecture="*" name="LogClinic" type="win32"/>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity
        type="win32"
        name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0"
        processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df"
        language="*"/>
    </dependentAssembly>
  </dependency>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2, PerMonitor</dpiAwareness>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
    </windowsSettings>
  </application>
</assembly>
"#
}

/// 写入 Windows resource 脚本。
///
/// 业务意图：
/// - resource 脚本声明应用图标和 manifest，资源 ID 使用 1，符合 Windows 默认图标和 EXE manifest 查找习惯。
fn write_resource_script(
    rc_path: &Path,
    icon_path: &Path,
    manifest_path: &Path,
) -> Result<(), String> {
    let escaped_icon_path = escape_windows_resource_path(icon_path);
    let escaped_manifest_path = escape_windows_resource_path(manifest_path);
    fs::write(
        rc_path,
        format!(
            "{WINDOWS_APP_RESOURCE_ID} ICON \"{escaped_icon_path}\"\n{WINDOWS_APP_RESOURCE_ID} {RT_MANIFEST} \"{escaped_manifest_path}\"\n"
        ),
    )
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

/// 单个 ICO 图片条目。
///
/// 业务意图：
/// - Windows `.ico` 文件可能包含多个尺寸和色深的图标图片，资源文件需要把每个图片拆成独立 `RT_ICON` 资源。
///
/// 边界条件：
/// - 图片长度和偏移来自外部二进制文件，读取时必须做越界检查，不能信任文件内容。
struct IcoImageEntry {
    width: u8,
    height: u8,
    color_count: u8,
    reserved: u8,
    planes: u16,
    bit_count: u16,
    bytes_in_res: u32,
    image_data: Vec<u8>,
}

/// 为 MSVC target 直接生成 Windows `.res` 资源文件。
///
/// 业务意图：
/// - `cargo-xwin` 在 macOS/Linux 上使用 MSVC ABI 交叉编译，但宿主机通常没有 `rc.exe`。
/// - 这里只生成应用图标需要的 `RT_ICON`/`RT_GROUP_ICON` 和启动必须的 manifest 资源，避免引入额外构建依赖。
///
/// 边界条件：
/// - `.ico` 文件结构异常时直接中止构建，防止发布出没有图标或资源损坏的 Windows 可执行文件。
/// - `.res` 二进制格式要求每个资源头和数据按 4 字节对齐，因此写入时必须显式补齐。
fn write_msvc_windows_resources(
    icon_path: &Path,
    manifest_path: &Path,
    resource_path: &Path,
) -> Result<(), String> {
    let icon_entries = read_ico_entries(icon_path)?;
    let manifest_data = fs::read(manifest_path).map_err(|error| {
        format!(
            "无法读取 Windows manifest {}：{error}",
            manifest_path.display()
        )
    })?;
    let mut resource = Vec::new();

    write_resource_entry(&mut resource, 0, 0, 0, 0, &[]);

    for (index, entry) in icon_entries.iter().enumerate() {
        let icon_id = icon_resource_id(index)?;
        write_resource_entry(
            &mut resource,
            RT_ICON,
            icon_id,
            WINDOWS_RESOURCE_MEMORY_FLAGS,
            WINDOWS_RESOURCE_LANGUAGE_ID,
            &entry.image_data,
        );
    }

    let group_icon_data = build_group_icon_data(&icon_entries)?;
    write_resource_entry(
        &mut resource,
        RT_GROUP_ICON,
        WINDOWS_APP_RESOURCE_ID,
        WINDOWS_RESOURCE_MEMORY_FLAGS,
        WINDOWS_RESOURCE_LANGUAGE_ID,
        &group_icon_data,
    );
    write_resource_entry(
        &mut resource,
        RT_MANIFEST,
        WINDOWS_APP_RESOURCE_ID,
        WINDOWS_RESOURCE_MEMORY_FLAGS,
        WINDOWS_RESOURCE_LANGUAGE_ID,
        &manifest_data,
    );

    fs::write(resource_path, resource)
        .map_err(|error| format!("无法写入 Windows 资源 {}：{error}", resource_path.display()))
}

/// 读取 ICO 文件中的所有图片条目。
///
/// 业务意图：
/// - Windows 会根据显示场景从同一个应用图标中选择不同尺寸，因此需要保留 `.ico` 中的所有图片。
///
/// 边界条件：
/// - 只接受标准图标文件 `type=1`，不接受光标文件或空图标。
/// - 所有偏移和长度都必须落在文件范围内，避免损坏文件导致构建脚本 panic。
fn read_ico_entries(icon_path: &Path) -> Result<Vec<IcoImageEntry>, String> {
    let bytes = fs::read(icon_path)
        .map_err(|error| format!("无法读取 Windows 图标 {}：{error}", icon_path.display()))?;

    if bytes.len() < 6 {
        return Err(format!("图标文件过短：{}", icon_path.display()));
    }

    let reserved = read_u16_le(&bytes, 0)?;
    let icon_type = read_u16_le(&bytes, 2)?;
    let count = read_u16_le(&bytes, 4)?;
    if reserved != 0 || icon_type != 1 {
        return Err(format!(
            "图标文件头无效：{}，期望 ICO type=1",
            icon_path.display()
        ));
    }
    if count == 0 {
        return Err(format!("图标文件不包含任何图片：{}", icon_path.display()));
    }

    let directory_size = 6usize
        .checked_add(usize::from(count) * 16)
        .ok_or_else(|| "图标目录大小溢出".to_string())?;
    if bytes.len() < directory_size {
        return Err(format!("图标目录被截断：{}", icon_path.display()));
    }

    (0..usize::from(count))
        .map(|index| read_ico_entry(&bytes, index))
        .collect()
}

/// 读取 ICO 目录中的单个图片条目。
///
/// 业务意图：
/// - 把目录元数据和真实图片字节绑定在一起，后续生成 `RT_GROUP_ICON` 时不再重复解析二进制文件。
fn read_ico_entry(bytes: &[u8], index: usize) -> Result<IcoImageEntry, String> {
    let offset = 6 + index * 16;
    let bytes_in_res = read_u32_le(bytes, offset + 8)?;
    let image_offset = read_u32_le(bytes, offset + 12)?;
    let image_start = usize::try_from(image_offset)
        .map_err(|_| format!("第 {index} 个图标图片偏移无法转换为 usize"))?;
    let image_len = usize::try_from(bytes_in_res)
        .map_err(|_| format!("第 {index} 个图标图片长度无法转换为 usize"))?;
    let image_end = image_start
        .checked_add(image_len)
        .ok_or_else(|| format!("第 {index} 个图标图片范围溢出"))?;

    if image_end > bytes.len() {
        return Err(format!(
            "第 {index} 个图标图片范围越界：offset={image_offset}, size={bytes_in_res}"
        ));
    }

    Ok(IcoImageEntry {
        width: bytes[offset],
        height: bytes[offset + 1],
        color_count: bytes[offset + 2],
        reserved: bytes[offset + 3],
        planes: read_u16_le(bytes, offset + 4)?,
        bit_count: read_u16_le(bytes, offset + 6)?,
        bytes_in_res,
        image_data: bytes[image_start..image_end].to_vec(),
    })
}

/// 构建 `RT_GROUP_ICON` 资源数据。
///
/// 业务意图：
/// - Windows 通过 group icon 资源把多个 `RT_ICON` 图片组合成同一个应用图标入口，资源 ID 1 会作为默认图标使用。
///
/// 边界条件：
/// - group icon 使用 16 位图片资源 ID；如果未来图标图片数量异常增大，需要在这里明确失败。
fn build_group_icon_data(entries: &[IcoImageEntry]) -> Result<Vec<u8>, String> {
    let mut data = Vec::with_capacity(6 + entries.len() * 14);
    push_u16_le(&mut data, 0);
    push_u16_le(&mut data, 1);
    push_u16_le(
        &mut data,
        u16::try_from(entries.len())
            .map_err(|_| "图标图片数量超过 Windows 资源上限".to_string())?,
    );

    for (index, entry) in entries.iter().enumerate() {
        data.push(entry.width);
        data.push(entry.height);
        data.push(entry.color_count);
        data.push(entry.reserved);
        push_u16_le(&mut data, entry.planes);
        push_u16_le(&mut data, entry.bit_count);
        push_u32_le(&mut data, entry.bytes_in_res);
        push_u16_le(&mut data, icon_resource_id(index)?);
    }

    Ok(data)
}

/// 根据 ICO 条目下标生成 Windows 图标图片资源 ID。
///
/// 边界条件：
/// - Windows 资源 ID 0 不用于真实图标图片，因此从 1 开始分配。
fn icon_resource_id(index: usize) -> Result<u16, String> {
    u16::try_from(index + 1).map_err(|_| "图标图片资源 ID 超过 u16 上限".to_string())
}

/// 写入一个 Windows `.res` 资源条目。
///
/// 业务意图：
/// - MSVC 链接器接受标准 `.res` 文件；手写条目可以覆盖本项目当前唯一需要的图标资源场景。
///
/// 边界条件：
/// - 资源头和资源数据必须分别按 4 字节对齐，否则 `lld-link` 可能拒绝资源或生成不可识别图标。
fn write_resource_entry(
    output: &mut Vec<u8>,
    type_id: u16,
    name_id: u16,
    memory_flags: u16,
    language_id: u16,
    data: &[u8],
) {
    let mut header_tail = Vec::with_capacity(24);
    push_numeric_resource_id(&mut header_tail, type_id);
    push_numeric_resource_id(&mut header_tail, name_id);
    pad_to_4_bytes(&mut header_tail);
    push_u32_le(&mut header_tail, 0);
    push_u16_le(&mut header_tail, memory_flags);
    push_u16_le(&mut header_tail, language_id);
    push_u32_le(&mut header_tail, 0);
    push_u32_le(&mut header_tail, 0);

    push_u32_le(output, data.len() as u32);
    push_u32_le(output, (8 + header_tail.len()) as u32);
    output.extend_from_slice(&header_tail);
    output.extend_from_slice(data);
    pad_to_4_bytes(output);
}

/// 写入数值型 Windows 资源标识。
///
/// 业务意图：
/// - 图标资源使用数字 ID，不需要写入 UTF-16 名称；`0xffff + id` 是 `.res` 的标准数值标识编码。
fn push_numeric_resource_id(output: &mut Vec<u8>, id: u16) {
    push_u16_le(output, 0xffff);
    push_u16_le(output, id);
}

/// 将缓冲区补齐到 4 字节边界。
///
/// 边界条件：
/// - `.res` 对齐字节必须为 0，避免不同链接器对未初始化填充字节产生不一致解释。
fn pad_to_4_bytes(output: &mut Vec<u8>) {
    while output.len() % 4 != 0 {
        output.push(0);
    }
}

/// 从字节切片读取 little-endian `u16`。
///
/// 边界条件：
/// - 图标文件来自磁盘，任何读取都必须先检查范围，避免损坏输入触发 panic。
fn read_u16_le(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let end = offset
        .checked_add(2)
        .ok_or_else(|| "读取 u16 时偏移溢出".to_string())?;
    let slice = bytes
        .get(offset..end)
        .ok_or_else(|| format!("读取 u16 越界：offset={offset}"))?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

/// 从字节切片读取 little-endian `u32`。
///
/// 边界条件：
/// - 所有 ICO 偏移和长度都通过该函数读取，读取失败时返回中文错误给 Cargo 构建输出。
fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| "读取 u32 时偏移溢出".to_string())?;
    let slice = bytes
        .get(offset..end)
        .ok_or_else(|| format!("读取 u32 越界：offset={offset}"))?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// 追加 little-endian `u16`。
///
/// 业务意图：
/// - Windows 资源和 ICO 目录均使用 little-endian 字段，集中写入可以避免手写字节顺序错误。
fn push_u16_le(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

/// 追加 little-endian `u32`。
///
/// 业务意图：
/// - Windows 资源头中多个字段都是 32 位整数，集中写入可以让 `.res` 生成逻辑保持可读。
fn push_u32_le(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
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
