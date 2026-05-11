// AI 对话业务功能域入口。
//
// 业务意图：
// - 该顶层模块承载 AI 对话的领域类型、SQLite 持久化和 OpenAI 兼容流式请求，不依赖 GPUI 或 `app`。
// - `app/ai_chat` 只负责输入控件、页面渲染、滚动条和 UI 事件适配，依赖方向固定为 `app -> ai_chat`。
//
// 边界条件：
// - 数据库文件名、schema、默认会话、SSE 解析和错误文案保持迁移前一致，避免重构改变用户历史数据。
// - API Key 仍按既有配置明文存放在应用配置目录，网络请求错误不输出完整鉴权信息。

mod constants;
mod domain;
mod storage;
mod stream;

pub(crate) use constants::*;
pub(crate) use domain::*;
pub(crate) use storage::*;
pub(crate) use stream::*;
