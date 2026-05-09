#!/usr/bin/env bash
#
# LogClinic 打包统一入口。
#
# 业务意图：
# - 给维护者提供一个稳定入口，按参数选择生成 macOS DMG 或 Windows EXE，避免记忆多个底层脚本路径。
# - 默认只执行当前平台最可靠的打包任务：macOS 上生成 DMG；非 Windows 上的 Windows 产物通过 cargo-xwin 构建 x64 MSVC EXE。
#
# 用法：
# - scripts/package-all.sh macos
# - scripts/package-all.sh windows
# - scripts/package-all.sh all
#
# 边界条件：
# - `all` 会先尝试 macOS DMG，再尝试 Windows EXE；如果当前机器没有 Windows 交叉编译环境，Windows 阶段会按脚本提示失败。
# - Windows 本机阶段依赖 PowerShell 7 的 `pwsh` 或系统 `powershell`；macOS/Linux 阶段依赖 `cargo-xwin` 和 `x86_64-pc-windows-msvc` target。

set -euo pipefail

# 业务意图：通过脚本所在位置定位仓库根目录，允许用户从任意工作目录执行脚本。
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE_TARGET="${1:-macos}"

run_macos_package() {
  "${SCRIPT_DIR}/package-macos-dmg.sh"
}

run_windows_package() {
  # 业务意图：macOS/Linux 发布机走 MSVC 交叉编译，Windows 本机继续走原有 PowerShell 脚本，避免强行要求 Windows 安装 Bash 交叉工具。
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*)
      ;;
    *)
      "${SCRIPT_DIR}/package-windows-x64-msvc.sh"
      return
      ;;
  esac

  # 业务意图：优先使用跨平台 PowerShell 7；Windows 旧环境仍可回退到系统 powershell。
  if command -v pwsh >/dev/null 2>&1; then
    pwsh -NoProfile -ExecutionPolicy Bypass -File "${SCRIPT_DIR}/package-windows-exe.ps1"
  elif command -v powershell >/dev/null 2>&1; then
    powershell -NoProfile -ExecutionPolicy Bypass -File "${SCRIPT_DIR}/package-windows-exe.ps1"
  else
    echo "缺少 PowerShell，无法执行 Windows EXE 打包脚本" >&2
    exit 1
  fi
}

case "${PACKAGE_TARGET}" in
  macos)
    run_macos_package
    ;;
  windows)
    run_windows_package
    ;;
  all)
    run_macos_package
    run_windows_package
    ;;
  *)
    echo "未知打包目标：${PACKAGE_TARGET}" >&2
    echo "用法：scripts/package-all.sh macos|windows|all" >&2
    exit 1
    ;;
esac
