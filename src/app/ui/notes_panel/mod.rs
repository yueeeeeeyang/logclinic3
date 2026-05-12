// 笔记 GPUI 面板模块入口。
//
// 业务意图：
// - 该目录承载笔记页面的树、阅读器、编辑器、确认弹窗和输入控件，纯业务能力来自顶层 `crate::notes`。
// - 依赖方向固定为 `app -> notes`，避免 SQLite schema 和树构建逻辑被 UI 模块拥有。

use super::*;

mod actions;
mod constants;
mod input_element;
mod ui_state;
mod view;

pub(in crate::app) use crate::notes::*;
pub(in crate::app) use constants::*;
pub(in crate::app) use input_element::*;
pub(in crate::app) use ui_state::*;

#[cfg(test)]
mod tests;
