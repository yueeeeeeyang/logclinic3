// 通用单行输入框组件入口。
//
// 业务意图：
// - 第一阶段把搜索关键字输入框迁移到统一组件，后续日志树搜索、设置页、连接表单和笔记标题可逐步复用。
// - 模块按 state / editing / element 拆分，避免绘制、状态快照和编辑规则继续混在具体功能页面中。

use super::*;

mod editing;
mod element;
mod state;

pub(in crate::app) use editing::*;
pub(in crate::app) use element::*;
pub(in crate::app) use state::*;
