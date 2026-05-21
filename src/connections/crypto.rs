// SSH 连接密码加密实现。
//
// 业务意图：
// - 用户选择保存密码，但密码不能明文写入 SQLite；本文件负责使用系统安全存储中的主密钥加密密码。
// - SQLite 只保存密文、nonce 和版本；系统安全存储不可用时返回中文错误，让 UI 明确提示用户无法保存连接。
//
// 安全边界：
// - 主密钥通过 macOS Keychain 或 Windows 凭据存储保存；当前进程仍会在连接时短暂持有密码明文。
// - AEAD 附加认证数据绑定连接 ID，避免把一个连接的密文复制到另一个连接后仍可解密。

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use keyring_core::{Entry, Error as KeyringError};
use zeroize::Zeroize;

use super::*;

/// 初始化当前平台的系统安全存储。
///
/// 跨平台约束：
/// - macOS 使用 Keychain，Windows 使用原生凭据存储；其它平台虽然可构建，但不是产品目标，失败会阻止保存密码。
fn initialize_connection_keyring_store() -> Result<(), String> {
    keyring::use_native_store(false).map_err(|error| format!("初始化系统安全存储失败：{error:?}"))
}

/// 打开连接密码主密钥条目。
fn connection_master_key_entry() -> Result<Entry, String> {
    initialize_connection_keyring_store()?;
    Entry::new(
        CONNECTION_KEYRING_SERVICE,
        CONNECTION_KEYRING_MASTER_KEY_USER,
    )
    .map_err(|error| format!("打开连接密码主密钥失败：{error:?}"))
}

/// 读取或创建连接密码主密钥。
///
/// 业务意图：
/// - 第一次保存连接时生成 32 字节随机主密钥并写入系统安全存储；后续保存和连接复用同一主密钥解密旧密码。
/// - 主密钥用 base64 文本存储，便于跨平台凭据 API 只接受字符串时稳定读写。
pub(crate) fn load_or_create_connection_master_key()
-> Result<[u8; CONNECTION_PASSWORD_MASTER_KEY_LEN], String> {
    let entry = connection_master_key_entry()?;
    match entry.get_password() {
        Ok(raw) => decode_connection_master_key(&raw),
        Err(KeyringError::NoEntry) => {
            let key = Aes256Gcm::generate_key(OsRng);
            let mut key_bytes = [0u8; CONNECTION_PASSWORD_MASTER_KEY_LEN];
            key_bytes.copy_from_slice(key.as_slice());
            let encoded = STANDARD.encode(key_bytes);
            entry
                .set_password(&encoded)
                .map_err(|error| format!("保存连接密码主密钥失败：{error:?}"))?;
            Ok(key_bytes)
        }
        Err(error) => Err(format!("读取连接密码主密钥失败：{error:?}")),
    }
}

/// 解析系统安全存储中的主密钥文本。
fn decode_connection_master_key(
    raw: &str,
) -> Result<[u8; CONNECTION_PASSWORD_MASTER_KEY_LEN], String> {
    let decoded = STANDARD
        .decode(raw)
        .map_err(|error| format!("连接密码主密钥不是有效 base64：{error}"))?;
    decoded
        .try_into()
        .map_err(|_| "连接密码主密钥长度无效，无法解密已保存密码".to_string())
}

/// 使用系统安全存储中的主密钥加密 SSH 密码。
pub(crate) fn encrypt_connection_password(
    connection_id: &str,
    password: &str,
) -> Result<String, String> {
    let key = load_or_create_connection_master_key()?;
    encrypt_connection_password_with_key(connection_id, password, &key)
}

/// 使用调用方提供的主密钥加密 SSH 密码。
///
/// 业务意图：
/// - 单元测试需要避免访问真实 Keychain/Windows 凭据存储，因此纯加密逻辑拆成可注入主密钥的函数。
pub(crate) fn encrypt_connection_password_with_key(
    connection_id: &str,
    password: &str,
    key: &[u8; CONNECTION_PASSWORD_MASTER_KEY_LEN],
) -> Result<String, String> {
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|_| "连接密码主密钥长度无效".to_string())?;
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(
            &nonce,
            aes_gcm::aead::Payload {
                msg: password.as_bytes(),
                aad: connection_id.as_bytes(),
            },
        )
        .map_err(|_| "加密连接密码失败".to_string())?;
    Ok(format!(
        "{}:{}:{}",
        CONNECTION_PASSWORD_CIPHERTEXT_VERSION,
        STANDARD.encode(nonce),
        STANDARD.encode(ciphertext)
    ))
}

/// 使用系统安全存储中的主密钥解密 SSH 密码。
pub(crate) fn decrypt_connection_password(
    connection_id: &str,
    encrypted_password: &str,
) -> Result<String, String> {
    let key = load_or_create_connection_master_key()?;
    decrypt_connection_password_with_key(connection_id, encrypted_password, &key)
}

/// 使用调用方提供的主密钥解密 SSH 密码。
pub(crate) fn decrypt_connection_password_with_key(
    connection_id: &str,
    encrypted_password: &str,
    key: &[u8; CONNECTION_PASSWORD_MASTER_KEY_LEN],
) -> Result<String, String> {
    let (version, nonce_raw, ciphertext_raw) =
        parse_connection_password_ciphertext(encrypted_password)?;
    if version != CONNECTION_PASSWORD_CIPHERTEXT_VERSION {
        return Err(format!("不支持的连接密码密文版本：{version}"));
    }

    let nonce_bytes = STANDARD
        .decode(nonce_raw)
        .map_err(|error| format!("连接密码 nonce 不是有效 base64：{error}"))?;
    let nonce_array: [u8; CONNECTION_PASSWORD_NONCE_LEN] = nonce_bytes
        .try_into()
        .map_err(|_| "连接密码 nonce 长度无效".to_string())?;
    let ciphertext = STANDARD
        .decode(ciphertext_raw)
        .map_err(|error| format!("连接密码密文不是有效 base64：{error}"))?;
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|_| "连接密码主密钥长度无效".to_string())?;
    let mut plaintext = cipher
        .decrypt(
            Nonce::from_slice(&nonce_array),
            aes_gcm::aead::Payload {
                msg: ciphertext.as_ref(),
                aad: connection_id.as_bytes(),
            },
        )
        .map_err(|_| "连接密码解密失败，请检查系统安全存储中的主密钥是否可用".to_string())?;
    let password = String::from_utf8(plaintext.clone())
        .map_err(|_| "连接密码解密后不是有效 UTF-8".to_string())?;
    plaintext.zeroize();
    Ok(password)
}

/// 拆分密文格式。
fn parse_connection_password_ciphertext(raw: &str) -> Result<(&str, &str, &str), String> {
    let mut parts = raw.splitn(3, ':');
    let version = parts
        .next()
        .ok_or_else(|| "连接密码密文缺少版本".to_string())?;
    let nonce = parts
        .next()
        .ok_or_else(|| "连接密码密文缺少 nonce".to_string())?;
    let ciphertext = parts
        .next()
        .ok_or_else(|| "连接密码密文缺少内容".to_string())?;
    if version.is_empty() || nonce.is_empty() || ciphertext.is_empty() {
        return Err("连接密码密文格式不完整".to_string());
    }
    Ok((version, nonce, ciphertext))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证密码加密后可以用同一连接 ID 和主密钥解密。
    ///
    /// 业务风险：
    /// - 如果 AEAD 参数或密文格式变化导致旧密码无法解密，用户将无法打开已保存连接。
    #[test]
    fn 连接密码可以加密并解密() {
        let key = [7u8; CONNECTION_PASSWORD_MASTER_KEY_LEN];
        let encrypted = encrypt_connection_password_with_key("conn-1", "secret", &key).unwrap();
        assert_ne!(encrypted, "secret");
        assert!(encrypted.starts_with("v1:"));
        let decrypted = decrypt_connection_password_with_key("conn-1", &encrypted, &key).unwrap();
        assert_eq!(decrypted, "secret");
    }

    /// 验证连接 ID 作为附加认证数据参与校验。
    #[test]
    fn 连接密码不能复制到其它连接解密() {
        let key = [9u8; CONNECTION_PASSWORD_MASTER_KEY_LEN];
        let encrypted = encrypt_connection_password_with_key("conn-1", "secret", &key).unwrap();
        let error = decrypt_connection_password_with_key("conn-2", &encrypted, &key)
            .expect_err("连接 ID 不一致时必须解密失败");
        assert!(!error.contains("secret"));
    }

    /// 验证密文被篡改后不会返回明文。
    #[test]
    fn 连接密码密文篡改后解密失败() {
        let key = [3u8; CONNECTION_PASSWORD_MASTER_KEY_LEN];
        let encrypted = encrypt_connection_password_with_key("conn-1", "secret", &key).unwrap();
        let tampered = encrypted.replace('A', "B");
        if tampered != encrypted {
            assert!(decrypt_connection_password_with_key("conn-1", &tampered, &key).is_err());
        }
    }
}
