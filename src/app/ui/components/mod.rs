// 通用 GPUI 组件模块聚合入口。
//
// 业务意图：
// - 跨日志、搜索、笔记、连接、设置和 AI 页面复用的 UI 控件统一放在本目录，避免各功能继续复制自绘控件。
// - 组件层只承载通用交互、绘制和状态辅助；具体业务副作用仍由调用方在各自页面中处理。

use super::*;

/// 通用日期时间选择器组件辅助。
pub(in crate::app) mod date_time_picker;
/// 通用受控单选 Select 组件。
pub(in crate::app) mod select;
/// 通用文本输入框组件。
pub(in crate::app) mod text_input;

pub(in crate::app) use date_time_picker::*;
pub(in crate::app) use select::*;
pub(in crate::app) use text_input::*;
