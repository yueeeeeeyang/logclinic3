// AI 对话功能域模块入口。
//
// 业务意图：
// - 该目录把 AI 对话从应用根文件中拆分出来，但仍作为 app 的私有子模块使用，避免重构扩大公开 API 面。
// - 子模块按职责拆分存储、流式请求、状态方法、渲染方法和自绘输入元素，便于后续维护时只阅读相关文件。

use super::*;

mod constants;
mod domain;
mod input_element;
mod state;
mod storage;
mod stream;
mod view;

pub(in crate::app) use constants::*;
pub(in crate::app) use domain::*;
pub(in crate::app) use input_element::*;
pub(in crate::app) use storage::*;
pub(in crate::app) use stream::*;

#[cfg(test)]
mod tests;
