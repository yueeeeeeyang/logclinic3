#!/usr/bin/env bash
#
# LogClinic macOS DMG 打包脚本。
#
# 业务意图：
# - 将当前 Rust release 可执行文件打包成标准 macOS `.app` 目录，再生成可分发的 `.dmg` 安装镜像。
# - 脚本默认使用 ad-hoc 签名保证 bundle 结构自洽；未公证产物在用户机器上仍可能触发 macOS Gatekeeper 提示。
#
# 关键约束：
# - 该脚本必须在 macOS 上运行，因为 `.dmg` 依赖系统自带的 `hdiutil`。
# - 字体资源当前通过 Rust `include_bytes!` 编译进二进制，不需要额外复制 assets 目录到 bundle。
# - 应用图标来自 `assets/icons/LogClinic.icns`，如果替换源图，应先运行 `scripts/generate-app-icons.py` 重新生成。
# - 默认签名身份为 `-`，即 ad-hoc 签名；如后续开通 Developer ID，可通过 `CODESIGN_IDENTITY="Developer ID Application: ..."` 覆盖。
# - Developer ID 分发建议保留默认时间戳；ad-hoc 签名或离线测试证书会跳过 Apple 时间戳服务。
# - 本脚本只负责签名，不执行 notarize 公证；面向陌生机器分发时仍应对最终 DMG 执行公证和 staple。
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

# 业务意图：默认用 ad-hoc 签名降低本地和临时分发门槛，同时允许发布机覆盖为 Developer ID 身份。
# 边界条件：
# - ad-hoc 签名不会产生可信开发者身份，也不能替代 notarize；它只能避免完全未签名导致的 bundle 校验问题。
# - 如果设置了 Developer ID 身份，脚本会自动为 `.app` 和 `.dmg` 请求 Apple 时间戳，满足后续公证的基础要求。
CODESIGN_IDENTITY="${CODESIGN_IDENTITY:--}"
CODESIGN_TIMESTAMP="${CODESIGN_TIMESTAMP:-true}"

# 业务意图：集中判断 `codesign` 是否应请求 Apple 时间戳，保证 `.app` 和 `.dmg` 使用一致的发布策略。
# 边界条件：
# - ad-hoc 身份 `-` 不能使用 Apple 时间戳服务，因此即使默认开启也会自动跳过。
# - 某些内网发布机无法访问 Apple 时间戳服务，可显式设置 `CODESIGN_TIMESTAMP=false`。
codesign_should_timestamp() {
  if [[ "${CODESIGN_IDENTITY}" != "-" && "${CODESIGN_TIMESTAMP}" != "false" ]]; then
    return 0
  fi
  return 1
}

# 业务意图：签名 `.app` bundle，并立即做严格校验，避免把资源封装不完整或签名无效的应用继续写入 DMG。
# 关键约束：
# - `--options runtime` 开启 hardened runtime，是后续 Developer ID 公证的基础要求。
# - `--deep` 用于覆盖 bundle 内部可执行文件和资源；当前应用没有额外 helper，但保留该参数便于后续扩展。
sign_and_verify_app() {
  echo "使用签名身份 '${CODESIGN_IDENTITY}' 签名应用..."
  if codesign_should_timestamp; then
    codesign \
      --force \
      --deep \
      --options runtime \
      --timestamp \
      --sign "${CODESIGN_IDENTITY}" \
      "${APP_DIR}"
  else
    codesign \
      --force \
      --deep \
      --options runtime \
      --sign "${CODESIGN_IDENTITY}" \
      "${APP_DIR}"
  fi

  echo "校验应用签名..."
  codesign --verify --deep --strict --verbose=4 "${APP_DIR}"
}

# 业务意图：签名最终 DMG，让下载或转发后的磁盘镜像本身也具备可校验签名。
# 边界条件：
# - DMG 不是可执行代码，不使用 hardened runtime；但它仍可被 `codesign` 附加 Developer ID 签名。
# - 公证应以签名后的 DMG 为输入，否则 staple 和用户端 Gatekeeper 校验会针对错误的制品。
sign_and_verify_dmg() {
  echo "使用签名身份 '${CODESIGN_IDENTITY}' 签名 DMG..."
  if codesign_should_timestamp; then
    codesign \
      --force \
      --timestamp \
      --sign "${CODESIGN_IDENTITY}" \
      "${DMG_PATH}"
  else
    codesign \
      --force \
      --sign "${CODESIGN_IDENTITY}" \
      "${DMG_PATH}"
  fi

  echo "校验 DMG 签名..."
  codesign --verify --verbose=4 "${DMG_PATH}"
}

# 业务意图：在构建开始前明确当前签名模式，避免维护者误以为 ad-hoc 产物已经具备 Developer ID 信任链。
describe_codesign_configuration() {
  if [[ "${CODESIGN_IDENTITY}" == "-" ]]; then
    echo "使用 ad-hoc 签名身份 '-'；该产物仍不能替代 Developer ID 公证分发。"
  else
    echo "使用代码签名身份 '${CODESIGN_IDENTITY}'。"
  fi
}

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

if ! command -v codesign >/dev/null 2>&1; then
  echo "缺少 codesign，无法执行 macOS 签名校验" >&2
  exit 1
fi

cd "${REPO_ROOT}"
describe_codesign_configuration

echo "构建 macOS release 可执行文件..."
cargo build --release

# 业务意图：重新创建 bundle，避免旧版本文件残留到新安装包中。
APP_DIR="${DIST_DIR}/${APP_NAME}.app"
APP_CONTENTS_DIR="${APP_DIR}/Contents"
APP_MACOS_DIR="${APP_CONTENTS_DIR}/MacOS"
APP_RESOURCES_DIR="${APP_CONTENTS_DIR}/Resources"
DMG_STAGING_DIR="${DIST_DIR}/dmg-staging"
DMG_PATH="${DIST_DIR}/${APP_NAME}-${VERSION}-macos.dmg"
ICON_SOURCE_PATH="${REPO_ROOT}/assets/icons/LogClinic.icns"

rm -rf "${APP_DIR}" "${DMG_STAGING_DIR}" "${DMG_PATH}"
mkdir -p "${APP_MACOS_DIR}" "${APP_RESOURCES_DIR}" "${DMG_STAGING_DIR}"

cp "${REPO_ROOT}/target/release/${BINARY_NAME}" "${APP_MACOS_DIR}/${APP_NAME}"
chmod +x "${APP_MACOS_DIR}/${APP_NAME}"

if [[ ! -f "${ICON_SOURCE_PATH}" ]]; then
  echo "缺少应用图标：${ICON_SOURCE_PATH}" >&2
  echo "请先运行：python3 scripts/generate-app-icons.py <源PNG路径>" >&2
  exit 1
fi
cp "${ICON_SOURCE_PATH}" "${APP_RESOURCES_DIR}/LogClinic.icns"

# 业务意图：生成最小可用 Info.plist，让 Finder 能识别 `.app`，并把版本号写入系统显示信息。
# 边界条件：
# - `CFBundleIconFile` 必须对应 Resources 下的 `.icns` 文件名，否则 Finder 和 Dock 会回退到默认应用图标。
# - `CFBundleDocumentTypes` 需要同时声明常见日志/文本/配置/压缩包 UTI 和扩展名兜底；
#   Finder 的“打开方式”不会仅凭应用被 LaunchServices 注册就展示，必须能匹配当前文件类型。
# - `CFBundleTypeExtensions` 在同一个字典里遇到 `LSItemContentTypes` 会被系统忽略，因此拆成独立兜底字典。
# - 真实分流仍由应用启动层按 `.hprof/.bin` 与其它文件区分，不能在 plist 中固化页面跳转规则。
cat >"${APP_CONTENTS_DIR}/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeIconFile</key>
      <string>LogClinic.icns</string>
      <key>CFBundleTypeName</key>
      <string>LogClinic Supported Documents</string>
      <key>CFBundleTypeRole</key>
      <string>Viewer</string>
      <key>LSHandlerRank</key>
      <string>Alternate</string>
      <key>LSItemContentTypes</key>
      <array>
        <string>public.item</string>
        <string>public.content</string>
        <string>public.data</string>
        <string>public.text</string>
        <string>public.plain-text</string>
        <string>public.utf8-plain-text</string>
        <string>public.xml</string>
        <string>public.json</string>
        <string>public.zip-archive</string>
        <string>public.archive</string>
        <string>public.folder</string>
        <string>com.apple.log</string>
      </array>
    </dict>
    <dict>
      <key>CFBundleTypeIconFile</key>
      <string>LogClinic.icns</string>
      <key>CFBundleTypeName</key>
      <string>LogClinic Extension Fallbacks</string>
      <key>CFBundleTypeRole</key>
      <string>Viewer</string>
      <key>LSHandlerRank</key>
      <string>Alternate</string>
      <key>CFBundleTypeExtensions</key>
      <array>
        <string>*</string>
        <string>log</string>
        <string>txt</string>
        <string>text</string>
        <string>out</string>
        <string>err</string>
        <string>trace</string>
        <string>dump</string>
        <string>hprof</string>
        <string>bin</string>
        <string>zip</string>
        <string>rar</string>
        <string>tar</string>
        <string>gz</string>
        <string>tgz</string>
        <string>7z</string>
        <string>json</string>
        <string>xml</string>
        <string>yaml</string>
        <string>yml</string>
        <string>properties</string>
        <string>conf</string>
        <string>config</string>
        <string>ini</string>
        <string>cfg</string>
        <string>toml</string>
      </array>
    </dict>
  </array>
  <key>CFBundleDevelopmentRegion</key>
  <string>zh_CN</string>
  <key>CFBundleDisplayName</key>
  <string>${APP_NAME}</string>
  <key>CFBundleExecutable</key>
  <string>${APP_NAME}</string>
  <key>CFBundleIdentifier</key>
  <string>${BUNDLE_IDENTIFIER}</string>
  <key>CFBundleIconFile</key>
  <string>LogClinic.icns</string>
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

# 业务意图：应用 bundle 必须先签名再放入 DMG，确保用户拷贝到 Applications 后仍能通过基础代码签名校验。
sign_and_verify_app

cp -R "${APP_DIR}" "${DMG_STAGING_DIR}/${APP_NAME}.app"
ln -s /Applications "${DMG_STAGING_DIR}/Applications"

echo "生成 DMG 镜像..."
hdiutil create \
  -volname "${APP_NAME}" \
  -srcfolder "${DMG_STAGING_DIR}" \
  -ov \
  -format UDZO \
  "${DMG_PATH}"

# 业务意图：最终交付的是 DMG，因此磁盘镜像本身也签名，便于接收方先校验安装包是否被改写。
sign_and_verify_dmg

echo "macOS 打包完成：${DMG_PATH}"
