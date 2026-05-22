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
    /// 连接所属分类 ID；`None` 表示挂在根层，兼容升级前没有分类的旧连接。
    pub(crate) category_id: Option<String>,
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

/// SMB 文件共享连接配置。
///
/// 业务意图：
/// - SMB 连接只提供文件管理，不创建终端 tab；该结构保存连接共享所需的最小参数和分类归属。
/// - `host/port/share/initial_path` 是规范化后的地址拆分结果，UI 地址输入允许 UNC、URL 和普通斜线路径，
///   但持久化层始终保存拆分字段，方便后端直接建立 SMB 会话和连接固定 share。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SmbConnectionProfile {
    /// 本地稳定 ID，用于 SQLite 主键、左侧树选中和 AEAD 附加认证数据。
    pub(crate) id: String,
    /// 用户可见连接名称。
    pub(crate) name: String,
    /// SMB 服务器主机名或 IP。
    pub(crate) host: String,
    /// SMB TCP 端口，默认 445。
    pub(crate) port: u16,
    /// 固定连接的共享名。
    pub(crate) share: String,
    /// 文件管理窗口初始进入的共享内路径，始终以 `/` 开头。
    pub(crate) initial_path: String,
    /// SMB 用户名；如需域账号，第一版由用户输入服务端接受的完整格式。
    pub(crate) username: String,
    /// 连接所属分类 ID；`None` 表示挂在根层。
    pub(crate) category_id: Option<String>,
    /// 加密后的 SMB 密码。
    pub(crate) encrypted_password: String,
    /// 创建时间，Unix epoch 毫秒。
    pub(crate) created_at_ms: i64,
    /// 最近更新时间，Unix epoch 毫秒。
    pub(crate) updated_at_ms: i64,
    /// 最近成功打开文件管理列表的时间，Unix epoch 毫秒。
    pub(crate) last_connected_at_ms: Option<i64>,
}

/// 规范化后的 SMB 地址。
///
/// 业务意图：
/// - 表单只暴露一个“地址”输入框，用户可以粘贴 `server/share/path`、UNC 或 `smb://` URL。
/// - 解析后将服务器、端口、共享名和共享内路径拆开，避免后端再依赖字符串切割。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedSmbAddress {
    /// SMB 服务器主机名或 IP。
    pub(crate) host: String,
    /// SMB TCP 端口。
    pub(crate) port: u16,
    /// SMB share 名称。
    pub(crate) share: String,
    /// 共享内初始路径，始终以 `/` 开头；根路径为 `/`。
    pub(crate) initial_path: String,
}

impl ParsedSmbAddress {
    /// 返回适合在编辑表单展示的规范化地址文本。
    ///
    /// 边界条件：
    /// - 默认端口 445 使用用户最熟悉的 UNC 写法，避免编辑时把 `\\server\share` 视觉上改成
    ///   `server/share`，让用户误以为连接目标被改变。
    /// - UNC 无法表达非默认端口；非 445 端口使用 `smb://host:port/share/path`，避免端口信息丢失。
    /// - 初始路径为根目录时只展示到 share，其它路径继续展示共享内路径。
    pub(crate) fn to_address_text(&self) -> String {
        if self.port == DEFAULT_SMB_PORT {
            let path = self.initial_path.trim_matches('/').replace('/', "\\");
            if path.is_empty() {
                format!(r"\\{}\{}", self.host, self.share)
            } else {
                format!(r"\\{}\{}\{}", self.host, self.share, path)
            }
        } else {
            let path = self.initial_path.trim_end_matches('/');
            format!("smb://{}:{}/{}{}", self.host, self.port, self.share, path)
        }
    }
}

/// 连接分类。
///
/// 业务意图：
/// - 分类只管理左侧连接树展示和连接归属，不直接影响 SSH 连接参数或终端 tab 生命周期。
/// - 根层是 UI 虚拟节点，不写入数据库；`parent_id=None` 表示顶级分类。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectionCategory {
    /// 本地稳定 ID，用于 SQLite 主键、父子关系和 UI 展开状态。
    pub(crate) id: String,
    /// 父分类 ID；`None` 表示顶级分类。
    pub(crate) parent_id: Option<String>,
    /// 用户可见分类名称。
    pub(crate) name: String,
    /// 创建时间，Unix epoch 毫秒；同级分类按该值稳定排序。
    pub(crate) created_at_ms: i64,
    /// 最近更新时间，Unix epoch 毫秒。
    pub(crate) updated_at_ms: i64,
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
    /// 连接所属分类 ID；`None` 表示挂在根层。
    pub(crate) category_id: Option<String>,
    /// SSH 密码明文草稿；保存后必须立即清空 UI 草稿。
    pub(crate) password: String,
}

/// SMB 连接表单草稿。
///
/// 业务意图：
/// - SMB 新增和编辑弹窗字段与 SSH 不同：地址是一个整体输入，保存时再解析成 host/port/share/path。
/// - 编辑时密码允许为空，表示继续使用 SQLite 中已有密文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SmbConnectionProfileDraft {
    /// 用户可见连接名称。
    pub(crate) name: String,
    /// SMB 地址文本，接受普通斜线、UNC 和 `smb://` 形式。
    pub(crate) address: String,
    /// SMB 用户名。
    pub(crate) username: String,
    /// 连接所属分类 ID；`None` 表示挂在根层。
    pub(crate) category_id: Option<String>,
    /// SMB 密码明文草稿；保存后必须立即清空 UI 草稿。
    pub(crate) password: String,
}

impl Drop for SmbConnectionProfileDraft {
    /// 草稿销毁时清理 SMB 密码明文。
    ///
    /// 安全边界：
    /// - 这里只能清理当前字符串缓冲区，不能追溯清理平台输入法或分配器历史副本。
    fn drop(&mut self) {
        self.password.zeroize();
    }
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
            category_id: None,
            password: String::new(),
        }
    }
}

/// 连接分类表单草稿。
///
/// 业务意图：
/// - 新建根分类、新建子分类和重命名分类都只需要名称字段；父分类由对话框模式保存，避免用户误改层级。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectionCategoryDraft {
    /// 用户可见分类名称。
    pub(crate) name: String,
}

impl ConnectionCategoryDraft {
    /// 创建空分类草稿。
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self {
            name: String::new(),
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

/// 校验 SMB 连接表单并返回规范化字段。
///
/// 业务意图：
/// - SMB 地址支持多种用户常见输入，但保存入口必须统一成 `ParsedSmbAddress`，避免存储层和后端各自解释。
/// - 新增时密码必填；编辑时空密码表示不修改旧密文。
pub(crate) fn validate_smb_connection_profile_draft(
    draft: &SmbConnectionProfileDraft,
    password_required: bool,
) -> Result<(String, ParsedSmbAddress, String, Option<Zeroizing<String>>), String> {
    let name = draft.name.trim().to_string();
    if name.is_empty() {
        return Err("连接名称不能为空".to_string());
    }

    let parsed = parse_smb_connection_address(&draft.address)?;
    let username = draft.username.trim().to_string();
    if username.is_empty() {
        return Err("用户名不能为空".to_string());
    }

    if password_required && draft.password.is_empty() {
        return Err("密码不能为空".to_string());
    }
    let password = (!draft.password.is_empty()).then(|| Zeroizing::new(draft.password.clone()));
    Ok((name, parsed, username, password))
}

/// 解析 SMB 地址文本。
///
/// 业务意图：
/// - 用户可能从 Windows 资源管理器、浏览器或文档中复制不同格式的 SMB 地址；解析函数集中兼容这些入口。
/// - 保存时不保留原始字符串，避免 `\\server\share` 与 `server/share` 在后续比较、编辑和测试中表现不一致。
///
/// 边界条件：
/// - 地址至少需要 host 和 share；缺少路径时共享内路径默认 `/`。
/// - `host:port/share` 支持自定义端口，端口必须是 1 到 65535。
pub(crate) fn parse_smb_connection_address(text: &str) -> Result<ParsedSmbAddress, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("SMB 地址不能为空".to_string());
    }

    let without_scheme = trimmed
        .strip_prefix("smb://")
        .or_else(|| trimmed.strip_prefix("SMB://"))
        .unwrap_or(trimmed);
    let normalized_separators = without_scheme.replace('\\', "/");
    let normalized = normalized_separators.trim_start_matches('/');
    let mut parts = normalized
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.len() < 2 {
        return Err("SMB 地址必须包含服务器和共享名，例如 server/share".to_string());
    }

    let host_port = parts.remove(0).trim();
    let (host, port) = parse_smb_host_and_port(host_port)?;
    let share = parts.remove(0).trim().to_string();
    if share.is_empty() {
        return Err("SMB 共享名不能为空".to_string());
    }

    let initial_path = normalize_smb_initial_path(&parts);
    Ok(ParsedSmbAddress {
        host,
        port,
        share,
        initial_path,
    })
}

/// 解析 SMB 地址中的 host 和可选端口。
fn parse_smb_host_and_port(host_port: &str) -> Result<(String, u16), String> {
    if host_port.is_empty() {
        return Err("SMB 服务器不能为空".to_string());
    }
    if let Some((host, port_text)) = host_port.rsplit_once(':')
        && !port_text.is_empty()
        && port_text
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        let host = host.trim().to_string();
        if host.is_empty() {
            return Err("SMB 服务器不能为空".to_string());
        }
        let port = port_text
            .parse::<u16>()
            .map_err(|_| "SMB 端口必须是 1 到 65535 的数字".to_string())?;
        if port == 0 {
            return Err("SMB 端口必须是 1 到 65535 的数字".to_string());
        }
        return Ok((host, port));
    }
    Ok((host_port.to_string(), DEFAULT_SMB_PORT))
}

/// 规范化共享内路径。
///
/// 边界条件：
/// - 用户地址中的多余斜线和 `.` 不参与保存。
/// - `..` 只在共享内部向上一级，不能越过 share 根目录。
fn normalize_smb_initial_path(parts: &[&str]) -> String {
    let mut normalized = Vec::new();
    for part in parts {
        match part.trim() {
            "" | "." => {}
            ".." => {
                normalized.pop();
            }
            value => normalized.push(value),
        }
    }
    if normalized.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", normalized.join("/"))
    }
}

/// 校验连接分类表单并返回规范化名称。
///
/// 业务意图：
/// - 分类名称只用于左侧树展示，允许中文、空格和常见符号，但首尾空白不应参与保存。
/// - 空名称会导致右键菜单和连接表单分类选择器难以识别，必须在保存入口统一拒绝。
pub(crate) fn validate_connection_category_draft(
    draft: &ConnectionCategoryDraft,
) -> Result<String, String> {
    let name = draft.name.trim().to_string();
    if name.is_empty() {
        return Err("分类名称不能为空".to_string());
    }
    Ok(name)
}

/// 判断候选父分类是否会让分类树形成环。
///
/// 业务意图：
/// - 第一版 UI 不暴露拖拽移动分类，但存储和后续扩展仍可能更新父级；提前提供纯函数可以锁住“树不能成环”的边界。
/// - 当 `next_parent_id` 为目标分类自己或其任意后代时，设置父级会形成循环，必须拒绝。
#[cfg(test)]
pub(crate) fn connection_category_parent_would_cycle(
    categories: &[ConnectionCategory],
    category_id: &str,
    next_parent_id: Option<&str>,
) -> bool {
    let Some(mut current_parent_id) = next_parent_id else {
        return false;
    };
    while let Some(parent) = categories
        .iter()
        .find(|category| category.id == current_parent_id)
    {
        if parent.id == category_id {
            return true;
        }
        let Some(parent_id) = parent.parent_id.as_deref() else {
            return false;
        };
        current_parent_id = parent_id;
    }
    current_parent_id == category_id
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
mod smb_tests {
    use super::*;

    /// 验证 SMB 地址解析兼容普通斜线、UNC、URL 和自定义端口。
    #[test]
    fn smb_地址解析会规范化常见输入格式() {
        let parsed = parse_smb_connection_address("server/share/logs").unwrap();
        assert_eq!(parsed.host, "server");
        assert_eq!(parsed.port, DEFAULT_SMB_PORT);
        assert_eq!(parsed.share, "share");
        assert_eq!(parsed.initial_path, "/logs");
        assert_eq!(parsed.to_address_text(), r"\\server\share\logs");

        let parsed = parse_smb_connection_address(r"\\server\share\logs\app").unwrap();
        assert_eq!(parsed.initial_path, "/logs/app");
        assert_eq!(parsed.to_address_text(), r"\\server\share\logs\app");

        let parsed = parse_smb_connection_address("//server:1445/share").unwrap();
        assert_eq!(parsed.host, "server");
        assert_eq!(parsed.port, 1445);
        assert_eq!(parsed.initial_path, "/");
        assert_eq!(parsed.to_address_text(), "smb://server:1445/share");

        let parsed = parse_smb_connection_address("smb://server/share/a/../b").unwrap();
        assert_eq!(parsed.initial_path, "/b");
    }

    /// 验证非法 SMB 地址会返回中文错误。
    #[test]
    fn smb_地址缺少服务器或共享名会拒绝() {
        assert!(
            parse_smb_connection_address("")
                .unwrap_err()
                .contains("不能为空")
        );
        assert!(
            parse_smb_connection_address("server")
                .unwrap_err()
                .contains("共享")
        );
        assert!(
            parse_smb_connection_address("server:0/share")
                .unwrap_err()
                .contains("端口")
        );
    }

    /// 验证 SMB 表单校验覆盖必填字段和编辑空密码语义。
    #[test]
    fn smb_连接草稿校验处理必填字段和编辑密码() {
        let mut draft = SmbConnectionProfileDraft {
            name: "SMB".to_string(),
            address: "server/share".to_string(),
            username: "user".to_string(),
            category_id: None,
            password: "secret".to_string(),
        };
        let (name, parsed, username, password) =
            validate_smb_connection_profile_draft(&draft, true).unwrap();
        assert_eq!(name, "SMB");
        assert_eq!(parsed.share, "share");
        assert_eq!(username, "user");
        assert!(password.is_some());

        draft.password.clear();
        let (_, _, _, password) = validate_smb_connection_profile_draft(&draft, false).unwrap();
        assert!(password.is_none());
        assert!(
            validate_smb_connection_profile_draft(&draft, true)
                .unwrap_err()
                .contains("密码")
        );
    }
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
            category_id: None,
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
            category_id: None,
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
            category_id: None,
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

    /// 验证分类名称保存前会去掉首尾空白，并拒绝空名称。
    #[test]
    fn 分类表单会校验名称() {
        let draft = ConnectionCategoryDraft::new();
        assert!(validate_connection_category_draft(&draft).is_err());

        let draft = ConnectionCategoryDraft {
            name: "  生产环境  ".to_string(),
        };
        assert_eq!(
            validate_connection_category_draft(&draft).expect("有效分类名称应通过校验"),
            "生产环境"
        );
    }

    /// 验证分类父级关系不能形成循环。
    #[test]
    fn 分类父级不能形成循环() {
        let categories = vec![
            ConnectionCategory {
                id: "cat-root".to_string(),
                parent_id: None,
                name: "根分类".to_string(),
                created_at_ms: 1,
                updated_at_ms: 1,
            },
            ConnectionCategory {
                id: "cat-child".to_string(),
                parent_id: Some("cat-root".to_string()),
                name: "子分类".to_string(),
                created_at_ms: 2,
                updated_at_ms: 2,
            },
        ];

        assert!(connection_category_parent_would_cycle(
            &categories,
            "cat-root",
            Some("cat-child")
        ));
        assert!(connection_category_parent_would_cycle(
            &categories,
            "cat-root",
            Some("cat-root")
        ));
        assert!(!connection_category_parent_would_cycle(
            &categories,
            "cat-child",
            None
        ));
    }
}
