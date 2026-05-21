// SSH 连接领域类型和纯状态辅助函数。
//
// 业务意图：
// - 这里定义连接配置、表单草稿、主机指纹校验和终端 tab 需要的纯数据，不依赖 GPUI 或 russh 具体会话。
// - UI、SQLite 和后台连接都通过这些类型传递数据，避免同一字段在不同层使用不同命名或默认值。

use std::{
    sync::atomic::Ordering,
    time::{SystemTime, UNIX_EPOCH},
};
use zeroize::{Zeroize, Zeroizing};

use super::*;

/// SSH 连接配置。
///
/// 业务意图：
/// - 该结构是 SQLite 中单条连接的内存镜像，包含显示名称、连接目标、加密后的密码和首次信任主机指纹。
/// - 密码字段永远是密文；后台连接前才通过系统安全存储中的主密钥解密成短生命周期明文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectionProfile {
    /// 本地稳定 ID，用于 SQLite 主键、tab 关联和 AEAD 附加认证数据。
    pub(crate) id: String,
    /// 用户可见连接名称。
    pub(crate) name: String,
    /// SSH 主机名或 IP。
    pub(crate) host: String,
    /// SSH 端口。
    pub(crate) port: u16,
    /// SSH 用户名。
    pub(crate) username: String,
    /// 加密后的 SSH 密码。
    pub(crate) encrypted_password: String,
    /// 首次信任保存的服务器公钥指纹；`None` 表示下一次连接需要用户确认。
    pub(crate) host_key_fingerprint: Option<String>,
    /// 创建时间，Unix epoch 毫秒。
    pub(crate) created_at_ms: i64,
    /// 最近更新时间，Unix epoch 毫秒。
    pub(crate) updated_at_ms: i64,
    /// 最近连接成功时间，Unix epoch 毫秒。
    pub(crate) last_connected_at_ms: Option<i64>,
}

/// SSH 连接表单草稿。
///
/// 业务意图：
/// - 新增和编辑弹窗都用同一套字段；编辑时密码允许为空，表示沿用旧密文，不强制用户重新输入。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectionProfileDraft {
    /// 用户可见连接名称。
    pub(crate) name: String,
    /// SSH 主机名或 IP。
    pub(crate) host: String,
    /// SSH 端口文本；保存时再解析为 `u16`，便于 UI 显示用户当前输入。
    pub(crate) port_text: String,
    /// SSH 用户名。
    pub(crate) username: String,
    /// SSH 密码明文草稿；保存后必须立即清空 UI 草稿。
    pub(crate) password: String,
}

impl ConnectionProfileDraft {
    /// 创建新增连接的默认草稿。
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            port_text: DEFAULT_SSH_PORT.to_string(),
            username: String::new(),
            password: String::new(),
        }
    }
}

impl Drop for ConnectionProfileDraft {
    /// 草稿销毁时清理 SSH 密码明文。
    ///
    /// 安全边界：
    /// - 这里无法清理用户输入过程中已经被分配器移动过的历史副本，但可以确保当前草稿生命周期结束后不再保留明文密码。
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

/// 当前 Unix epoch 毫秒。
///
/// 边界条件：
/// - 系统时间早于 epoch 时回退 0，避免排序字段写入负数后影响连接列表展示。
pub(crate) fn current_connection_time_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// 生成连接域实体 ID。
pub(crate) fn new_connection_entity_id(prefix: &str) -> String {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = CONNECTION_ENTITY_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{seed}-{sequence}")
}

/// 校验连接表单并返回规范化字段。
///
/// 业务意图：
/// - 新增、编辑和测试连接必须使用同一套规则，避免 UI 可以保存但后台无法连接。
/// - 编辑时 `password_required` 为 false，空密码表示不改旧密码；新增时必须输入密码。
pub(crate) fn validate_connection_profile_draft(
    draft: &ConnectionProfileDraft,
    password_required: bool,
) -> Result<(String, String, u16, String, Option<Zeroizing<String>>), String> {
    let name = draft.name.trim().to_string();
    if name.is_empty() {
        return Err("连接名称不能为空".to_string());
    }

    let host = draft.host.trim().to_string();
    if host.is_empty() {
        return Err("主机不能为空".to_string());
    }
    if host.contains('/') || host.contains('\\') {
        return Err("主机只能填写域名或 IP，不能包含路径分隔符".to_string());
    }

    let port = draft
        .port_text
        .trim()
        .parse::<u16>()
        .map_err(|_| "端口必须是 1 到 65535 的数字".to_string())?;
    if port == 0 {
        return Err("端口必须是 1 到 65535 的数字".to_string());
    }

    let username = draft.username.trim().to_string();
    if username.is_empty() {
        return Err("用户名不能为空".to_string());
    }

    if password_required && draft.password.is_empty() {
        return Err("密码不能为空".to_string());
    }
    let password = (!draft.password.is_empty()).then(|| Zeroizing::new(draft.password.clone()));

    Ok((name, host, port, username, password))
}

/// 根据编辑后的 SSH 端点决定是否保留已信任主机指纹。
///
/// 业务意图：
/// - 主机指纹只对某个 host/port 端点有意义；用户把连接改到另一台机器或另一个端口后，
///   继续沿用旧指纹会把下一次首次连接错误地判定为“指纹不匹配”。
/// - 用户名变化不影响服务器主机密钥，因此仅在 host 或 port 改变时清空信任状态。
pub(crate) fn retained_connection_host_key_fingerprint_after_endpoint_edit(
    existing: &ConnectionProfile,
    next_host: &str,
    next_port: u16,
) -> Option<String> {
    if existing.host == next_host && existing.port == next_port {
        existing.host_key_fingerprint.clone()
    } else {
        None
    }
}

/// 判断主机指纹是否通过首次信任规则。
///
/// 业务意图：
/// - 后台 SSH handler 只能做纯判断和事件上报，用户确认必须回到 UI；该函数把“未信任、匹配、变化”三种结果明确建模。
pub(crate) fn verify_connection_host_key(
    trusted_fingerprint: Option<&str>,
    actual_fingerprint: &str,
) -> HostKeyVerification {
    match trusted_fingerprint {
        None => HostKeyVerification::Unknown {
            actual: actual_fingerprint.to_string(),
        },
        Some(expected) if expected == actual_fingerprint => HostKeyVerification::Trusted,
        Some(expected) => HostKeyVerification::Mismatch {
            expected: expected.to_string(),
            actual: actual_fingerprint.to_string(),
        },
    }
}

/// 主机指纹校验结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HostKeyVerification {
    /// 指纹已信任并匹配。
    Trusted,
    /// 当前连接没有保存指纹，需要用户确认。
    Unknown {
        /// 服务器本次返回的公钥指纹。
        actual: String,
    },
    /// 已保存指纹和本次指纹不一致，必须阻止连接。
    Mismatch {
        /// 之前保存的可信指纹。
        expected: String,
        /// 服务器本次返回的公钥指纹。
        actual: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证新增连接必须填写完整字段。
    ///
    /// 业务风险：
    /// - UI 如果允许保存空主机、空用户名或空密码，后台 SSH 连接只能在更晚阶段失败，错误会更难理解。
    #[test]
    fn 连接表单会校验必填字段() {
        let mut draft = ConnectionProfileDraft::new();
        assert!(validate_connection_profile_draft(&draft, true).is_err());

        draft.name = "生产服务器".to_string();
        draft.host = "example.com".to_string();
        draft.username = "root".to_string();
        assert!(validate_connection_profile_draft(&draft, true).is_err());

        draft.password = "secret".to_string();
        assert!(validate_connection_profile_draft(&draft, true).is_ok());
    }

    /// 验证非法端口会被拒绝。
    #[test]
    fn 连接表单会拒绝非法端口() {
        let draft = ConnectionProfileDraft {
            name: "测试".to_string(),
            host: "127.0.0.1".to_string(),
            port_text: "70000".to_string(),
            username: "root".to_string(),
            password: "secret".to_string(),
        };

        let error = validate_connection_profile_draft(&draft, true)
            .expect_err("超过 u16 范围的端口必须被拒绝");
        assert!(error.contains("端口"));
    }

    /// 验证编辑连接时空密码表示不修改旧密码。
    #[test]
    fn 编辑连接允许密码为空() {
        let draft = ConnectionProfileDraft {
            name: "测试".to_string(),
            host: "127.0.0.1".to_string(),
            port_text: "22".to_string(),
            username: "root".to_string(),
            password: String::new(),
        };

        let (_, _, _, _, password) =
            validate_connection_profile_draft(&draft, false).expect("编辑时空密码应被接受");
        assert!(password.is_none());
    }

    /// 验证编辑连接目标变化时会进入新的首次信任流程。
    #[test]
    fn 编辑连接端点变化会清空旧主机指纹() {
        let mut profile = ConnectionProfile {
            id: "conn-1".to_string(),
            name: "测试".to_string(),
            host: "old.example.com".to_string(),
            port: 22,
            username: "root".to_string(),
            encrypted_password: "v1:nonce:cipher".to_string(),
            host_key_fingerprint: Some("SHA256:old".to_string()),
            created_at_ms: 1,
            updated_at_ms: 1,
            last_connected_at_ms: None,
        };

        assert_eq!(
            retained_connection_host_key_fingerprint_after_endpoint_edit(
                &profile,
                "old.example.com",
                22,
            )
            .as_deref(),
            Some("SHA256:old")
        );
        assert!(
            retained_connection_host_key_fingerprint_after_endpoint_edit(
                &profile,
                "new.example.com",
                22,
            )
            .is_none()
        );

        profile.host = "new.example.com".to_string();
        assert!(
            retained_connection_host_key_fingerprint_after_endpoint_edit(
                &profile,
                "new.example.com",
                2022
            )
            .is_none()
        );
    }

    /// 验证主机指纹首次信任、匹配和不匹配三种分支。
    #[test]
    fn 主机指纹校验覆盖首次信任和不匹配() {
        assert_eq!(
            verify_connection_host_key(None, "SHA256:new"),
            HostKeyVerification::Unknown {
                actual: "SHA256:new".to_string()
            }
        );
        assert_eq!(
            verify_connection_host_key(Some("SHA256:old"), "SHA256:old"),
            HostKeyVerification::Trusted
        );
        assert_eq!(
            verify_connection_host_key(Some("SHA256:old"), "SHA256:new"),
            HostKeyVerification::Mismatch {
                expected: "SHA256:old".to_string(),
                actual: "SHA256:new".to_string()
            }
        );
    }
}
