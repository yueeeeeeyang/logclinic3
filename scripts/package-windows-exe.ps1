# LogClinic Windows EXE 打包脚本。
#
# 业务意图：
# - 将当前 Rust 项目构建为 Windows release `.exe`，并复制到稳定的 dist/windows 目录，方便发布或上传。
# - Windows 原生运行时默认使用 MSVC target；非 Windows 机器可显式传入 GNU target，但需要自行准备交叉链接器。
#
# 关键约束：
# - 该脚本只产出可执行文件，不生成 MSI/NSIS 安装器；如果后续需要安装向导，应另行引入专门安装器工具链。
# - 字体资源当前通过 Rust `include_bytes!` 编译进二进制，不需要额外复制 assets 目录。
# - 程序图标来自 `assets/icons/LogClinic.ico`，由 `build.rs` 在 Windows target 构建时嵌入最终 `.exe`。
# - 如果目标 Rust target 未安装，脚本会提示 `rustup target add`，避免构建阶段出现难懂的链接错误。
#
# 用法示例：
# - Windows 本机：powershell -ExecutionPolicy Bypass -File scripts/package-windows-exe.ps1
# - 指定 target：powershell -ExecutionPolicy Bypass -File scripts/package-windows-exe.ps1 -Target x86_64-pc-windows-msvc

param(
    # Windows Rust 编译目标。默认在 Windows 上使用 MSVC，在其他平台上使用 GNU 作为交叉编译候选。
    [string]$Target = ""
)

$ErrorActionPreference = "Stop"

# 业务意图：通过脚本所在位置定位仓库根目录，允许用户从任意工作目录执行脚本。
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Resolve-Path (Join-Path $ScriptDir "..")
$CargoToml = Join-Path $RepoRoot "Cargo.toml"
$BinaryName = "logclinic3"
$AppName = "LogClinic"

# 业务意图：版本号直接读取 Cargo.toml，保证产物目录和文件名与 Cargo 包版本一致。
$VersionLine = Select-String -Path $CargoToml -Pattern '^\s*version\s*=\s*"([^"]+)"' | Select-Object -First 1
if ($null -eq $VersionLine) {
    throw "无法从 Cargo.toml 读取版本号"
}
$Version = $VersionLine.Matches[0].Groups[1].Value

# 边界条件：Windows 本机优先使用 MSVC target；非 Windows 上使用 GNU target 仍依赖本机安装 mingw-w64 等链接器。
# 实现原因：Windows PowerShell 5.1 没有 PowerShell 7 的 `$IsWindows` 变量，因此用环境变量和 PSEdition 做兼容判断。
$RunningOnWindows = ($env:OS -eq "Windows_NT") -or ($PSVersionTable.PSEdition -eq "Desktop")
if ([string]::IsNullOrWhiteSpace($Target)) {
    if ($RunningOnWindows) {
        $Target = "x86_64-pc-windows-msvc"
    } else {
        $Target = "x86_64-pc-windows-gnu"
    }
}

$InstalledTargets = rustup target list --installed
if ($InstalledTargets -notcontains $Target) {
    throw "Rust target '$Target' 尚未安装，请先执行：rustup target add $Target"
}

Push-Location $RepoRoot
try {
    Write-Host "构建 Windows release 可执行文件，target=$Target ..."
    cargo build --release --target $Target

    $ExeSource = Join-Path $RepoRoot "target/$Target/release/$BinaryName.exe"
    if (-not (Test-Path $ExeSource)) {
        throw "未找到构建产物：$ExeSource"
    }

    # 业务意图：使用版本和 target 创建独立目录，避免不同 Windows ABI 或版本之间互相覆盖。
    $PackageDir = Join-Path $RepoRoot "dist/windows/$AppName-$Version-windows-$Target"
    if (Test-Path $PackageDir) {
        Remove-Item -Recurse -Force $PackageDir
    }
    New-Item -ItemType Directory -Force -Path $PackageDir | Out-Null

    $ExeTarget = Join-Path $PackageDir "$AppName.exe"
    Copy-Item $ExeSource $ExeTarget

    Write-Host "Windows 打包完成：$ExeTarget"
} finally {
    Pop-Location
}
