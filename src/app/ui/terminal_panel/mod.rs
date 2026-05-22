// 本地终端页 GPUI 模块聚合入口。
//
// 业务意图：
// - “终端”作为独立主导航功能，只负责本机 shell tab，不再混在“连接”页的远程连接管理中。
// - 本模块复用连接页已经验证过的终端后端、alacritty 渲染和文件管理窗口能力，避免维护两套终端协议实现。
// - 状态、动作和渲染拆分到独立文件，后续如果本地终端继续扩展配置项、快捷键或 shell 集成，不会再次挤压连接页代码。

use super::*;

mod actions;
mod ui_state;
mod view;

pub(in crate::app) use ui_state::*;
