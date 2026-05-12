// OpenAI 兼容模型配置功能域。
//
// 业务意图：
// - 集中维护模型档案 JSON 格式、字段校验、测试请求构造和磁盘读写。
// - 普通 UI 偏好已经拆到 `preferences`，路径选择拆到 `paths`，本文件只保留模型相关规则。
//
// 安全边界：
// - API Key 仍按既有需求明文保存在应用配置目录；测试请求和错误文案不得输出完整 Key。
// - 模型测试使用 blocking client，只应在后台执行器调用，不能阻塞 GPUI 主线程。

use std::{fs, io, path::Path, time::Duration};

use serde::{Deserialize, Serialize};

use super::paths::model_configs_preference_path;

/// 测试模型接口的超时时间。
///
/// 业务意图：
/// - 测试按钮只用于快速确认配置是否可用，不能因为网络不可达或本地服务无响应长期占用后台线程。
/// - 20 秒是需求确认的默认超时，既兼容本地模型冷启动，也能让 UI 尽快反馈失败。
pub(crate) const MODEL_TEST_TIMEOUT_SECONDS: u64 = 20;

/// OpenAI 兼容 Chat Completions 路径。
///
/// 业务意图：
/// - `base_url` 约定为 API 根路径，例如 `https://api.openai.com/v1`，测试和真实 AI 请求统一拼接该相对路径。
/// - 单独定义后测试和真实请求共用一套拼接规则，避免尾斜杠处理不一致。
pub(crate) const MODEL_TEST_CHAT_COMPLETIONS_PATH: &str = "chat/completions";

/// OpenAI 兼容模型配置文件的单条档案。
///
/// 业务意图：
/// - 每条档案保存一个可命名的 API 端点和模型 ID，用户可以在不同 OpenAI 兼容服务之间切换。
/// - 字段保持简单字符串，便于 JSON 配置手工排查和后续迁移。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ModelProfile {
    /// 稳定 ID，用于列表选择和默认模型引用；不直接展示给用户。
    pub(crate) id: String,
    /// 用户可读配置名称。
    pub(crate) name: String,
    /// OpenAI 兼容 API 根路径，例如 `https://api.openai.com/v1`。
    pub(crate) base_url: String,
    /// API Key，允许为空以兼容不需要鉴权的本地服务。
    pub(crate) api_key: String,
    /// Chat Completions 请求体中的模型 ID。
    pub(crate) model: String,
}

/// 模型配置文件的完整结构。
///
/// 业务意图：
/// - `profiles` 保存多条配置，`default_profile_id` 保存当前默认配置的稳定 ID。
/// - 删除或读取损坏配置时会通过规范化逻辑清理悬空默认 ID，避免 UI 指向不存在的配置。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ModelConfigs {
    /// 已保存的模型配置列表。
    pub(crate) profiles: Vec<ModelProfile>,
    /// 当前默认配置 ID；没有默认或默认配置被删除时为 `None`。
    pub(crate) default_profile_id: Option<String>,
}

/// 校验模型配置表单的必填字段和 URL 协议。
///
/// 业务意图：
/// - 保存、测试和 AI 对话流式请求都必须使用同一套校验，避免 UI 能保存但后台请求使用非法 URL。
/// - API Key 明确允许为空，以兼容本地 Ollama/vLLM 等不需要 Bearer 鉴权的服务。
pub(crate) fn validate_model_profile_fields(
    name: &str,
    base_url: &str,
    model: &str,
) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("配置名称不能为空".to_string());
    }
    if base_url.trim().is_empty() {
        return Err("Base URL 不能为空".to_string());
    }
    if model.trim().is_empty() {
        return Err("模型 ID 不能为空".to_string());
    }
    let base_url = base_url.trim();
    if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        return Err("Base URL 必须以 http:// 或 https:// 开头".to_string());
    }
    Ok(())
}

/// 拼接 OpenAI 兼容 Chat Completions 请求 URL。
///
/// 边界条件：
/// - 用户可能在 Base URL 末尾输入一个或多个 `/`，拼接时统一去掉末尾斜杠，避免出现双斜杠路径。
pub(crate) fn model_test_chat_completions_url(base_url: &str) -> Result<String, String> {
    let base_url = base_url.trim();
    if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        return Err("Base URL 必须以 http:// 或 https:// 开头".to_string());
    }
    Ok(format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        MODEL_TEST_CHAT_COMPLETIONS_PATH
    ))
}

/// 构造模型测试请求体。
///
/// 业务意图：
/// - 第一版只支持 OpenAI Chat Completions 兼容接口，固定发送最小 `ping` 请求，降低真实调用成本。
pub(crate) fn model_test_request_body(model: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model.trim(),
        "messages": [
            {
                "role": "user",
                "content": "ping"
            }
        ],
        "max_tokens": 1,
        "stream": false
    })
}

/// 返回 OpenAI 兼容请求需要发送的 Authorization 头。
///
/// 业务意图：
/// - API Key 非空才发送 Bearer header，避免本地模型服务因为无意义空鉴权头拒绝请求。
pub(crate) fn model_test_authorization_header(api_key: &str) -> Option<String> {
    let api_key = api_key.trim();
    (!api_key.is_empty()).then(|| format!("Bearer {api_key}"))
}

/// 判断模型测试响应是否包含 Chat Completions 的 choices。
///
/// 边界条件：
/// - 只要求 `choices` 是数组，不强制数组非空；部分兼容服务在 `max_tokens=1` 下仍可能返回空内容但格式有效。
pub(crate) fn model_test_response_has_choices(value: &serde_json::Value) -> bool {
    value
        .get("choices")
        .is_some_and(|choices| choices.is_array())
}

/// 将 HTTP 错误响应体裁剪成适合 UI 展示的短文本。
///
/// 业务意图：
/// - 兼容服务可能返回很长的 JSON 错误，设置窗口只需要展示可理解的前段原因，避免撑破状态栏。
pub(crate) fn model_test_http_error_body_snippet(body: &str) -> String {
    let normalized = body.replace(['\r', '\n'], " ");
    let trimmed = normalized.trim();
    if trimmed.chars().count() <= 160 {
        trimmed.to_string()
    } else {
        let snippet: String = trimmed.chars().take(160).collect();
        format!("{snippet}...")
    }
}

/// 执行一次 OpenAI 兼容模型测试请求。
///
/// 业务意图：
/// - 该函数只在后台执行器中调用，使用 blocking client 可以避免把额外异步运行时引入 GPUI 主线程。
/// - 请求不记录 API Key，也不会把完整请求头写入错误文案，降低明文 Key 暴露风险。
pub(crate) fn test_openai_compatible_model(profile: ModelProfile) -> Result<String, String> {
    validate_model_profile_fields(&profile.name, &profile.base_url, &profile.model)?;
    let url = model_test_chat_completions_url(&profile.base_url)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(MODEL_TEST_TIMEOUT_SECONDS))
        .build()
        .map_err(|error| format!("测试失败：创建 HTTP 客户端失败：{error}"))?;

    let mut request = client
        .post(url)
        .json(&model_test_request_body(&profile.model));
    if let Some(authorization) = model_test_authorization_header(&profile.api_key) {
        request = request.header(reqwest::header::AUTHORIZATION, authorization);
    }

    let response = request
        .send()
        .map_err(|error| format!("测试失败：请求接口失败：{error}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        let snippet = model_test_http_error_body_snippet(&body);
        if snippet.is_empty() {
            return Err(format!("测试失败：HTTP 状态码 {status}"));
        }
        return Err(format!("测试失败：HTTP 状态码 {status}，{snippet}"));
    }

    let value = response
        .json::<serde_json::Value>()
        .map_err(|error| format!("测试失败：响应 JSON 解析失败：{error}"))?;
    if model_test_response_has_choices(&value) {
        Ok("测试成功：模型接口可用".to_string())
    } else {
        Err("测试失败：响应 JSON 缺少 choices 字段".to_string())
    }
}

/// 规范化模型配置文件内容。
///
/// 业务意图：
/// - 配置文件可能被用户手工修改，默认模型 ID 可能指向不存在的档案；读取后统一清理悬空引用，避免 UI 高亮错误。
/// - 这里不主动裁剪字段内容，保存按钮的校验负责约束新写入内容，读取历史配置时尽量保持用户原文可修复。
pub(crate) fn normalize_model_configs(mut configs: ModelConfigs) -> ModelConfigs {
    if configs
        .default_profile_id
        .as_ref()
        .is_some_and(|default_id| {
            !configs
                .profiles
                .iter()
                .any(|profile| &profile.id == default_id)
        })
    {
        configs.default_profile_id = None;
    }
    configs
}

/// 从指定文件读取模型配置。
///
/// 错误处理：
/// - 文件缺失、读取失败或 JSON 损坏都返回空配置，不能阻断日志查看主流程或设置窗口打开。
/// - 损坏 JSON 不会自动覆盖原文件，避免用户仍可手工恢复其中的 API Key 和模型信息。
pub(crate) fn read_model_configs_preference(path: &Path) -> ModelConfigs {
    let Ok(raw) = fs::read_to_string(path) else {
        return ModelConfigs::default();
    };
    serde_json::from_str::<ModelConfigs>(&raw)
        .map(normalize_model_configs)
        .unwrap_or_default()
}

/// 将模型配置写入指定文件。
///
/// 业务意图：
/// - 模型配置包含列表和默认 ID，使用 pretty JSON 保存，方便用户在配置目录中直接核对。
pub(crate) fn write_model_configs_preference(
    path: &Path,
    configs: &ModelConfigs,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let serialized = serde_json::to_string_pretty(&normalize_model_configs(configs.clone()))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    fs::write(path, format!("{serialized}\n"))
}

/// 读取模型配置。
pub(crate) fn load_model_configs_preference() -> ModelConfigs {
    model_configs_preference_path()
        .map(|path| read_model_configs_preference(&path))
        .unwrap_or_default()
}

/// 保存模型配置。
///
/// 错误处理：
/// - 写入失败不回滚当前 UI 状态，仅输出诊断；这样配置目录权限问题不会让用户丢失当前表单内容。
pub(crate) fn save_model_configs_preference(configs: &ModelConfigs) {
    let Some(path) = model_configs_preference_path() else {
        return;
    };
    if let Err(error) = write_model_configs_preference(&path, configs) {
        eprintln!("保存模型配置失败：{}：{}", path.display(), error);
    }
}
