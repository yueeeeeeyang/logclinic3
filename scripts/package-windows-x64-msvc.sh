#!/usr/bin/env bash
#
# LogClinic Windows x64 MSVC 交叉编译脚本。
#
# 业务意图：
# - 在 macOS 或 Linux 发布机上通过 `cargo-xwin` 构建 Windows x64 MSVC `.exe`，避免必须切换到 Windows 机器。
# - 产物复制到稳定的 `dist/windows` 目录，方便后续上传、压缩或人工拷贝到 Windows 机器验证。
#
# 关键约束：
# - 目标固定为 `x86_64-pc-windows-msvc`，这是 Windows x64 原生发布更常用的 ABI。
# - `cargo-xwin` 会下载并缓存 Microsoft CRT/SDK 文件；首次运行需要网络，缓存默认放在 `target/cargo-xwin-v16`，避免污染用户全局缓存。
# - 当前 macOS 自带 Xcode clang-cl 17 与 VS2022 最新 STL 不兼容，因此默认使用 xwin v16；可通过 `XWIN_VERSION=17` 显式覆盖。
# - 本脚本只产出 `.exe` 和目录，不生成 MSI/NSIS 安装器；安装器需求应由独立脚本处理。
# - 程序图标由 `build.rs` 在 MSVC target 下直接生成 `.res` 文件并嵌入，不需要宿主机安装 `rc.exe`。
#
# 用法：
# - scripts/package-windows-x64-msvc.sh

set -euo pipefail

# 业务意图：通过脚本所在位置定位仓库根目录，允许用户从任意工作目录执行脚本。
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
CARGO_TOML="${REPO_ROOT}/Cargo.toml"
BINARY_NAME="logclinic3"
APP_NAME="LogClinic"
TARGET="x86_64-pc-windows-msvc"
XWIN_VERSION="${XWIN_VERSION:-16}"
XWIN_CACHE_DIR="${XWIN_CACHE_DIR:-${REPO_ROOT}/target/cargo-xwin-v${XWIN_VERSION}}"

# 业务意图：版本号直接读取 Cargo.toml，保证 Windows 产物目录与 Rust 包版本一致。
VERSION="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)"/\1/p' "${CARGO_TOML}" | head -n 1)"
if [[ -z "${VERSION}" ]]; then
  echo "无法从 Cargo.toml 读取版本号" >&2
  exit 1
fi

# 边界条件：Windows 本机无需通过 cargo-xwin 交叉编译，应继续使用 PowerShell 原生打包脚本。
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    echo "当前环境看起来是 Windows shell，请使用 scripts/package-windows-exe.ps1 原生构建。" >&2
    exit 1
    ;;
esac

if ! command -v cargo >/dev/null 2>&1; then
  echo "缺少 cargo，请先安装 Rust 工具链。" >&2
  exit 1
fi

if ! command -v rustup >/dev/null 2>&1; then
  echo "缺少 rustup，无法确认 Windows MSVC target 是否已安装。" >&2
  exit 1
fi

if ! rustup target list --installed | grep -Fxq "${TARGET}"; then
  echo "Rust target '${TARGET}' 尚未安装，请先执行：rustup target add ${TARGET}" >&2
  exit 1
fi

# 业务意图：`cargo xwin` 是 cargo 子命令，真实可执行文件通常名为 `cargo-xwin`。
# 边界条件：缺少该工具时不自动安装，避免脚本在 CI 或开发机上静默修改全局 Cargo 环境。
if ! cargo xwin --version >/dev/null 2>&1; then
  echo "缺少 cargo-xwin，请先执行：cargo install cargo-xwin --locked" >&2
  exit 1
fi

cd "${REPO_ROOT}"

# 业务意图：cargo-xwin 会把 `llvm-ar` 链接成 `llvm-lib`，但部分环境下 cc-rs 仍会先查找环境变量；
# 显式指向 Rust 自带 LLVM 工具可以减少额外安装 Homebrew LLVM 的要求。
LLVM_AR_PATH="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/llvm-ar"
if [[ ! -f "${LLVM_AR_PATH}" ]]; then
  echo "缺少 llvm-ar，请先执行：rustup component add llvm-tools-preview" >&2
  exit 1
fi
if [[ -z "${AR_x86_64_pc_windows_msvc:-}" ]]; then
  export AR_x86_64_pc_windows_msvc="${LLVM_AR_PATH}"
fi

# 业务意图：unrar_sys 的 Windows x64 C++ 源码包含 SSSE3/AES 优化路径；clang-cl 交叉编译时不会默认开启这些 intrinsic。
# 边界条件：目标固定为 Windows x64 桌面端，SSSE3/AES 已是实际发布环境的合理最低 CPU 假设；若后续需要兼容极老 CPU，应先禁用 unrar 的 SIMD 源码而不是静默移除标志。
export CXXFLAGS="${CXXFLAGS:-} -mssse3 -maes"

echo "构建 Windows x64 MSVC release 可执行文件，target=${TARGET}, xwin=${XWIN_VERSION} ..."
XWIN_CACHE_DIR="${XWIN_CACHE_DIR}" cargo xwin build --release --target "${TARGET}" --xwin-version "${XWIN_VERSION}"

EXE_SOURCE="${REPO_ROOT}/target/${TARGET}/release/${BINARY_NAME}.exe"
if [[ ! -f "${EXE_SOURCE}" ]]; then
  echo "未找到构建产物：${EXE_SOURCE}" >&2
  exit 1
fi

# 业务意图：使用版本和 target 创建独立目录，避免不同 ABI 或版本之间互相覆盖。
PACKAGE_DIR="${REPO_ROOT}/dist/windows/${APP_NAME}-${VERSION}-windows-${TARGET}"
rm -rf "${PACKAGE_DIR}"
mkdir -p "${PACKAGE_DIR}"

EXE_TARGET="${PACKAGE_DIR}/${APP_NAME}.exe"
cp "${EXE_SOURCE}" "${EXE_TARGET}"

echo "Windows x64 MSVC 打包完成：${EXE_TARGET}"
