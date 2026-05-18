#!/usr/bin/env bash
# 泛微日志插件独立编译和 ZIP 打包脚本。
#
# 业务意图：
# - 插件不再作为主程序内置二进制发布，而是从本目录独立 `cargo build`，再把 `plugin.json` 和 sidecar 二进制打成 ZIP。
# - 生成的 ZIP 可在 LogClinic 设置页“插件”Tab 中直接加载，宿主会把 ZIP 解压到应用配置目录。
#
# 边界条件：
# - 默认构建当前宿主平台 release 包；如需交叉编译，可传入目标三元组，例如：
#   `./package.sh x86_64-pc-windows-msvc`。
# - ZIP 中的二进制必须位于 plugin.json 同级目录，manifest 的 `entry.command` 使用相对插件目录的 `weaver-logext`。

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_ID="weaver-logext"
VERSION="$(python3 - "${SCRIPT_DIR}/plugin.json" <<'PY'
import json
import pathlib
import sys

print(json.loads(pathlib.Path(sys.argv[1]).read_text())["version"])
PY
)"

TARGET_TRIPLE="${1:-${CARGO_BUILD_TARGET:-}}"
BUILD_ARGS=(build --manifest-path "${SCRIPT_DIR}/Cargo.toml" --release)
if [[ -n "${TARGET_TRIPLE}" ]]; then
  BUILD_ARGS+=(--target "${TARGET_TRIPLE}")
fi

echo "Building ${PLUGIN_ID}..."
cargo "${BUILD_ARGS[@]}"

if [[ -n "${TARGET_TRIPLE}" ]]; then
  TARGET_DIR="${SCRIPT_DIR}/target/${TARGET_TRIPLE}/release"
else
  TARGET_DIR="${SCRIPT_DIR}/target/release"
fi

EXE_EXT=""
case "${TARGET_TRIPLE:-$(uname -s)}" in
  *windows*|*Windows*|MINGW*|MSYS*|CYGWIN*) EXE_EXT=".exe" ;;
esac

SOURCE_BIN="${TARGET_DIR}/${PLUGIN_ID}${EXE_EXT}"
if [[ ! -f "${SOURCE_BIN}" ]]; then
  echo "找不到插件二进制：${SOURCE_BIN}" >&2
  exit 1
fi

PACKAGE_ROOT="${SCRIPT_DIR}/target/package/${PLUGIN_ID}"
DIST_DIR="${SCRIPT_DIR}/dist"
ZIP_NAME="${PLUGIN_ID}-${VERSION}"
if [[ -n "${TARGET_TRIPLE}" ]]; then
  ZIP_NAME="${ZIP_NAME}-${TARGET_TRIPLE}"
fi
ZIP_PATH="${DIST_DIR}/${ZIP_NAME}.zip"

rm -rf "${PACKAGE_ROOT}"
mkdir -p "${PACKAGE_ROOT}" "${DIST_DIR}"
cp "${SCRIPT_DIR}/plugin.json" "${PACKAGE_ROOT}/plugin.json"
cp "${SOURCE_BIN}" "${PACKAGE_ROOT}/${PLUGIN_ID}${EXE_EXT}"

python3 - "${PACKAGE_ROOT}" "${ZIP_PATH}" <<'PY'
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

echo "插件包已生成：${ZIP_PATH}"
