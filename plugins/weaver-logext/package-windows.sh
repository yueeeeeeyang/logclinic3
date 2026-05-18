#!/usr/bin/env bash
# 泛微日志插件 Windows 版本一键交叉编译和 ZIP 打包脚本。
#
# 业务意图：
# - 插件与主程序完全分离发布，Windows 用户需要拿到包含 `plugin.json` 和 `weaver-logext.exe` 的独立 ZIP。
# - macOS/Linux 上普通 `cargo build --target x86_64-pc-windows-msvc` 依赖微软 `link.exe`，因此本脚本自动改用 `cargo-xwin`
#   完成 MSVC 目标交叉链接，避免手工安装 Visual Studio Build Tools。
# - Windows 本机执行时直接复用 `package.sh`，让安装了 Visual Studio Build Tools 的开发机继续走原生 MSVC 工具链。
#
# 关键约束：
# - ZIP 根目录必须直接包含 `plugin.json` 和 sidecar 二进制，设置页“加载 ZIP”才能识别并安装。
# - manifest 的入口命令保持 `weaver-logext`，宿主会在 Windows 下自动查找同目录的 `weaver-logext.exe`。
# - 如果本机缺少 `cargo-xwin`，默认自动安装；设置 `AUTO_INSTALL_CARGO_XWIN=0` 可改为只提示错误，适合离线 CI。
#
# 使用方式：
#   ./package-windows.sh
#   ./package-windows.sh x86_64-pc-windows-msvc

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_ID="weaver-logext"
DEFAULT_TARGET_TRIPLE="x86_64-pc-windows-msvc"
TARGET_TRIPLE="${1:-${DEFAULT_TARGET_TRIPLE}}"

if [[ "${TARGET_TRIPLE}" == "-h" || "${TARGET_TRIPLE}" == "--help" ]]; then
  cat <<EOF
用法：$(basename "$0") [Windows 目标三元组]

默认目标：${DEFAULT_TARGET_TRIPLE}
输出目录：${SCRIPT_DIR}/dist

环境变量：
  AUTO_INSTALL_CARGO_XWIN=0  缺少 cargo-xwin 时不自动安装，只输出错误提示。
EOF
  exit 0
fi

if [[ "${TARGET_TRIPLE}" != *"-pc-windows-msvc" ]]; then
  echo "本脚本只用于打包 Windows MSVC 插件目标，当前目标为：${TARGET_TRIPLE}" >&2
  exit 1
fi

require_command() {
  local command_name="$1"
  local install_hint="$2"
  if ! command -v "${command_name}" >/dev/null 2>&1; then
    echo "缺少命令：${command_name}" >&2
    echo "${install_hint}" >&2
    exit 1
  fi
}

is_windows_host() {
  case "${OS:-$(uname -s)}" in
    Windows_NT|MINGW*|MSYS*|CYGWIN*) return 0 ;;
    *) return 1 ;;
  esac
}

plugin_version() {
  python3 - "${SCRIPT_DIR}/plugin.json" <<'PY'
import json
import pathlib
import sys

print(json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))["version"])
PY
}

ensure_rust_target() {
  if rustup target list --installed | grep -qx "${TARGET_TRIPLE}"; then
    return
  fi
  echo "正在安装 Rust 目标：${TARGET_TRIPLE}"
  rustup target add "${TARGET_TRIPLE}"
}

ensure_cargo_xwin() {
  if cargo xwin --version >/dev/null 2>&1; then
    return
  fi
  if [[ "${AUTO_INSTALL_CARGO_XWIN:-1}" != "1" ]]; then
    echo "缺少 cargo-xwin，无法在非 Windows 环境交叉编译 MSVC 目标。" >&2
    echo "请先执行：cargo install cargo-xwin --locked" >&2
    exit 1
  fi
  echo "缺少 cargo-xwin，正在自动安装..."
  cargo install cargo-xwin --locked
}

package_zip() {
  local version="$1"
  local target_dir="${SCRIPT_DIR}/target/${TARGET_TRIPLE}/release"
  local source_bin="${target_dir}/${PLUGIN_ID}.exe"
  local package_root="${SCRIPT_DIR}/target/package/${PLUGIN_ID}-${TARGET_TRIPLE}"
  local dist_dir="${SCRIPT_DIR}/dist"
  local zip_path="${dist_dir}/${PLUGIN_ID}-${version}-${TARGET_TRIPLE}.zip"

  if [[ ! -f "${source_bin}" ]]; then
    echo "找不到 Windows 插件二进制：${source_bin}" >&2
    exit 1
  fi

  rm -rf "${package_root}"
  mkdir -p "${package_root}" "${dist_dir}"
  cp "${SCRIPT_DIR}/plugin.json" "${package_root}/plugin.json"
  cp "${source_bin}" "${package_root}/${PLUGIN_ID}.exe"

  python3 - "${package_root}" "${zip_path}" <<'PY'
import pathlib
import sys
import zipfile

package_root = pathlib.Path(sys.argv[1])
zip_path = pathlib.Path(sys.argv[2])
if zip_path.exists():
    zip_path.unlink()
with zipfile.ZipFile(zip_path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
    for path in sorted(package_root.rglob("*")):
        if path.is_file():
            archive.write(path, path.relative_to(package_root).as_posix())
PY

  echo "Windows 插件包已生成：${zip_path}"
}

require_command cargo "请先安装 Rust 工具链：https://rustup.rs/"
require_command rustup "请先安装 rustup，并确保 cargo/rustup 都在 PATH 中。"
require_command python3 "请先安装 Python 3，用于读取 plugin.json 和生成 ZIP。"

if is_windows_host; then
  echo "检测到 Windows 环境，使用原生 MSVC 打包流程。"
  "${SCRIPT_DIR}/package.sh" "${TARGET_TRIPLE}"
  exit 0
fi

ensure_rust_target
ensure_cargo_xwin

echo "正在交叉编译 ${PLUGIN_ID} -> ${TARGET_TRIPLE}"
cargo xwin build --manifest-path "${SCRIPT_DIR}/Cargo.toml" --release --target "${TARGET_TRIPLE}"

package_zip "$(plugin_version)"
