#!/usr/bin/env bash
#
# LogClinic macOS DMG 打包脚本。
#
# 业务意图：
# - 将当前 Rust release 可执行文件打包成标准 macOS `.app` 目录，再生成可分发的 `.dmg` 安装镜像。
# - 脚本只负责本地打包，不执行 notarize 公证上传；未签名或未公证产物在用户机器上可能触发 macOS Gatekeeper 提示。
#
# 关键约束：
# - 该脚本必须在 macOS 上运行，因为 `.dmg` 依赖系统自带的 `hdiutil`。
# - 字体资源当前通过 Rust `include_bytes!` 编译进二进制，不需要额外复制 assets 目录到 bundle。
# - 如果需要签名，可通过 `CODESIGN_IDENTITY="Developer ID Application: ..."` 环境变量启用；未设置时保持未签名产物。
#
# 产物：
# - dist/macos/LogClinic.app
# - dist/macos/LogClinic-<version>-macos.dmg

set -euo pipefail

# 业务意图：通过脚本所在位置定位仓库根目录，允许用户从任意工作目录执行脚本。
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DIST_DIR="${REPO_ROOT}/dist/macos"
APP_NAME="LogClinic"
BUNDLE_IDENTIFIER="com.yueyang.logclinic"
BINARY_NAME="logclinic3"

# 业务意图：版本号直接读取 Cargo.toml，保证 DMG 文件名和应用 Info.plist 与 Cargo 包版本一致。
VERSION="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)"/\1/p' "${REPO_ROOT}/Cargo.toml" | head -n 1)"
if [[ -z "${VERSION}" ]]; then
  echo "无法从 Cargo.toml 读取版本号" >&2
  exit 1
fi

# 边界条件：DMG 只能在 macOS 构建；其他平台没有 hdiutil，也无法可靠生成 Apple 磁盘镜像。
if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "macOS DMG 打包必须在 macOS 上执行" >&2
  exit 1
fi

if ! command -v hdiutil >/dev/null 2>&1; then
  echo "缺少 hdiutil，无法生成 DMG" >&2
  exit 1
fi

cd "${REPO_ROOT}"

echo "构建 macOS release 可执行文件..."
cargo build --release

# 业务意图：重新创建 bundle，避免旧版本文件残留到新安装包中。
APP_DIR="${DIST_DIR}/${APP_NAME}.app"
APP_CONTENTS_DIR="${APP_DIR}/Contents"
APP_MACOS_DIR="${APP_CONTENTS_DIR}/MacOS"
APP_RESOURCES_DIR="${APP_CONTENTS_DIR}/Resources"
DMG_STAGING_DIR="${DIST_DIR}/dmg-staging"
DMG_PATH="${DIST_DIR}/${APP_NAME}-${VERSION}-macos.dmg"

rm -rf "${APP_DIR}" "${DMG_STAGING_DIR}" "${DMG_PATH}"
mkdir -p "${APP_MACOS_DIR}" "${APP_RESOURCES_DIR}" "${DMG_STAGING_DIR}"

cp "${REPO_ROOT}/target/release/${BINARY_NAME}" "${APP_MACOS_DIR}/${APP_NAME}"
chmod +x "${APP_MACOS_DIR}/${APP_NAME}"

# 业务意图：生成最小可用 Info.plist，让 Finder 能识别 `.app`，并把版本号写入系统显示信息。
# 边界条件：当前没有应用图标资源，因此不声明 CFBundleIconFile，避免引用不存在的 icns 导致 Finder 显示异常。
cat >"${APP_CONTENTS_DIR}/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>zh_CN</string>
  <key>CFBundleDisplayName</key>
  <string>${APP_NAME}</string>
  <key>CFBundleExecutable</key>
  <string>${APP_NAME}</string>
  <key>CFBundleIdentifier</key>
  <string>${BUNDLE_IDENTIFIER}</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>${APP_NAME}</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>${VERSION}</string>
  <key>CFBundleVersion</key>
  <string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key>
  <string>12.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

# 业务意图：允许发布机通过环境变量选择签名身份；本地开发默认跳过，避免要求每台机器都配置证书。
if [[ -n "${CODESIGN_IDENTITY:-}" ]]; then
  echo "使用签名身份 '${CODESIGN_IDENTITY}' 签名应用..."
  codesign --force --deep --options runtime --sign "${CODESIGN_IDENTITY}" "${APP_DIR}"
else
  echo "未设置 CODESIGN_IDENTITY，跳过代码签名。"
fi

cp -R "${APP_DIR}" "${DMG_STAGING_DIR}/${APP_NAME}.app"
ln -s /Applications "${DMG_STAGING_DIR}/Applications"

echo "生成 DMG 镜像..."
hdiutil create \
  -volname "${APP_NAME}" \
  -srcfolder "${DMG_STAGING_DIR}" \
  -ov \
  -format UDZO \
  "${DMG_PATH}"

echo "macOS 打包完成：${DMG_PATH}"
