// AI 对话 GPUI 面板模块入口。
//
// 业务意图：
// - 该目录只保留 AI 对话页面的输入控件、状态方法、渲染和滚动交互，纯业务能力来自顶层 `crate::ai_chat`。
// - 依赖方向固定为 `app -> ai_chat`，避免 SQLite schema、SSE 协议和请求体构造继续被 UI 模块拥有。

use super::*;

mod actions;
mod constants;
mod input_element;
mod markdown;
mod ui_state;
mod view;

pub(in crate::app) use crate::ai_chat::*;
pub(in crate::app) use constants::*;
pub(in crate::app) use input_element::*;
pub(in crate::app) use markdown::*;
pub(in crate::app) use ui_state::*;

#[cfg(test)]
mod tests;
