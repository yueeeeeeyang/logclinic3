# Zed Editor 组件替换验证记录

## 结论

本轮不替换右侧日志正文查看器，也不修改 `Cargo.toml` 依赖。

原因是 Zed `editor` 组件当前不是可独立轻量接入的发布 crate。它位于 Zed monorepo 的 `crates/editor`，依赖大量 workspace 内部 crate，并要求与同一 workspace 来源的 `gpui`、`language`、`multi_buffer`、`theme`、`workspace`、`ui` 等模块协同使用。在当前工程中直接引入固定 tag 后，无法在合理时间内稳定完成依赖获取和 `cargo check` 验证；继续改主工程会留下无法编译的半成品状态。

## 已验证事实

- 固定验证版本：`zed-industries/zed` 的 `v1.1.6` tag。
- `crates/editor/Cargo.toml` 声明 `license = "GPL-3.0-or-later"`，并依赖 Zed workspace 中大量内部 crate。
- `crates/gpui/Cargo.toml` 中 `gpui` 版本仍是 `0.2.2`，但如果使用 Zed `editor`，当前项目必须迁移到同一 Zed git/workspace 来源的 `gpui`，否则会出现同名不同来源的 GPUI 类型不兼容。
- 当前工程没有项目级 LICENSE；如果后续真正合入 Zed `editor`，需要先补齐 GPL-3.0-or-later 兼容的项目级许可证声明。

## 本地验证结果

执行过的临时验证：

```sh
CARGO_HOME=/private/tmp/logclinic3-cargo-home \
  cargo check --manifest-path /private/tmp/logclinic-zed-editor-poc/Cargo.toml
```

临时 `Cargo.toml` 仅包含：

```toml
[dependencies]
editor = { git = "https://github.com/zed-industries/zed", tag = "v1.1.6", package = "editor" }
gpui = { git = "https://github.com/zed-industries/zed", tag = "v1.1.6", package = "gpui" }
```

结果：

- 沙箱内网络无法解析 GitHub。
- 外部网络权限下 Cargo git fetch 长时间停留在拉取 Zed monorepo，随后连接中断。
- 额外尝试 `git clone --depth 1 --branch v1.1.6 https://github.com/zed-industries/zed`，同样未能稳定完成。

这意味着当前“直接替换默认 viewer 并保持主工程可 `cargo check`”的准入条件没有满足。

## 后续可行路径

1. 如果仍坚持 Zed editor 路线，先把 Zed 源码以稳定方式 vendored 到单独目录或子模块，并确认固定 commit 可在本机完整 `cargo check`。
2. 在完成 vendoring 后，再做一个独立分支迁移 `gpui` 来源，并只验证最小只读 editor：显示文本、行号、选择复制、跳转行。
3. 如果不能接受 vendoring 和 GPL 传染边界，应改选可独立发布、许可证更轻的文本编辑器组件，或继续演进当前自绘 viewer。

## 参考来源

- Zed editor package: <https://github.com/zed-industries/zed/blob/v1.1.6/crates/editor/Cargo.toml>
- Zed gpui package: <https://github.com/zed-industries/zed/blob/v1.1.6/crates/gpui/Cargo.toml>
- Zed GPL license: <https://github.com/zed-industries/zed/blob/v1.1.6/LICENSE-GPL>
