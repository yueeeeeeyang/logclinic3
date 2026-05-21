// SSH 连接 SQLite 持久化实现。
//
// 业务意图：
// - 连接列表、加密密码和首次信任主机指纹必须跨重启保存，因此集中写入应用配置目录中的独立 SQLite 数据库。
// - 本文件只负责路径、schema 初始化和 CRUD，不处理 UI 表单、密码明文或 SSH 网络任务。
//
// 跨平台约束：
// - 路径沿用应用配置目录，避免 macOS/Windows 分别散落数据库目录判断；目录不可用时连接页展示中文错误。

use std::{
    fs,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, params};

use crate::config::app_config_dir;

use super::*;

/// 获取连接配置数据库路径。
pub(crate) fn connections_database_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(CONNECTIONS_DATABASE_FILE_NAME))
}

/// 打开并初始化连接配置数据库。
pub(crate) fn open_connections_database(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("创建连接数据库目录失败：{error}"))?;
    }
    let connection =
        Connection::open(path).map_err(|error| format!("打开连接数据库失败：{error}"))?;
    initialize_connections_database(&connection)?;
    Ok(connection)
}

/// 初始化连接数据库 schema。
///
/// 边界条件：
/// - `user_version` 大于当前版本时说明数据库来自未来版本，不能安全降级读取，直接返回中文错误。
pub(crate) fn initialize_connections_database(connection: &Connection) -> Result<(), String> {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| format!("读取连接数据库版本失败：{error}"))?;
    if version > CONNECTIONS_DATABASE_SCHEMA_VERSION {
        return Err(format!(
            "连接数据库版本 {version} 高于当前支持版本 {CONNECTIONS_DATABASE_SCHEMA_VERSION}"
        ));
    }

    connection
        .execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS ssh_connections (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL,
                host TEXT NOT NULL,
                port INTEGER NOT NULL,
                username TEXT NOT NULL,
                encrypted_password TEXT NOT NULL,
                host_key_fingerprint TEXT,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                last_connected_at_ms INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_ssh_connections_updated_at
                ON ssh_connections(updated_at_ms DESC);
            "#,
        )
        .map_err(|error| format!("初始化连接数据库失败：{error}"))?;

    if version < CONNECTIONS_DATABASE_SCHEMA_VERSION {
        connection
            .pragma_update(None, "user_version", CONNECTIONS_DATABASE_SCHEMA_VERSION)
            .map_err(|error| format!("写入连接数据库版本失败：{error}"))?;
    }
    Ok(())
}

/// 加载 SSH 连接列表。
pub(crate) fn load_connection_profiles(path: &Path) -> Result<Vec<ConnectionProfile>, String> {
    let connection = open_connections_database(path)?;
    let mut statement = connection
        .prepare(
            r#"
            SELECT id, name, host, port, username, encrypted_password,
                   host_key_fingerprint, created_at_ms, updated_at_ms, last_connected_at_ms
            FROM ssh_connections
            ORDER BY updated_at_ms DESC, created_at_ms DESC
            "#,
        )
        .map_err(|error| format!("读取连接列表失败：{error}"))?;
    let rows = statement
        .query_map([], |row| {
            let port_i64: i64 = row.get(3)?;
            let port = u16::try_from(port_i64)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, port_i64))?;
            Ok(ConnectionProfile {
                id: row.get(0)?,
                name: row.get(1)?,
                host: row.get(2)?,
                port,
                username: row.get(4)?,
                encrypted_password: row.get(5)?,
                host_key_fingerprint: row.get(6)?,
                created_at_ms: row.get(7)?,
                updated_at_ms: row.get(8)?,
                last_connected_at_ms: row.get(9)?,
            })
        })
        .map_err(|error| format!("读取连接列表失败：{error}"))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析连接列表失败：{error}"))
}

/// 新增 SSH 连接。
pub(crate) fn insert_connection_profile(
    path: &Path,
    profile: &ConnectionProfile,
) -> Result<(), String> {
    let connection = open_connections_database(path)?;
    connection
        .execute(
            r#"
            INSERT INTO ssh_connections
                (id, name, host, port, username, encrypted_password, host_key_fingerprint,
                 created_at_ms, updated_at_ms, last_connected_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            params![
                profile.id,
                profile.name,
                profile.host,
                i64::from(profile.port),
                profile.username,
                profile.encrypted_password,
                profile.host_key_fingerprint,
                profile.created_at_ms,
                profile.updated_at_ms,
                profile.last_connected_at_ms,
            ],
        )
        .map_err(|error| format!("保存连接失败：{error}"))?;
    Ok(())
}

/// 更新 SSH 连接。
pub(crate) fn update_connection_profile(
    path: &Path,
    profile: &ConnectionProfile,
) -> Result<(), String> {
    let connection = open_connections_database(path)?;
    connection
        .execute(
            r#"
            UPDATE ssh_connections
            SET name = ?2,
                host = ?3,
                port = ?4,
                username = ?5,
                encrypted_password = ?6,
                host_key_fingerprint = ?7,
                updated_at_ms = ?8,
                last_connected_at_ms = ?9
            WHERE id = ?1
            "#,
            params![
                profile.id,
                profile.name,
                profile.host,
                i64::from(profile.port),
                profile.username,
                profile.encrypted_password,
                profile.host_key_fingerprint,
                profile.updated_at_ms,
                profile.last_connected_at_ms,
            ],
        )
        .map_err(|error| format!("更新连接失败：{error}"))?;
    Ok(())
}

/// 删除 SSH 连接。
pub(crate) fn delete_connection_profile(path: &Path, profile_id: &str) -> Result<(), String> {
    let connection = open_connections_database(path)?;
    connection
        .execute(
            "DELETE FROM ssh_connections WHERE id = ?1",
            params![profile_id],
        )
        .map_err(|error| format!("删除连接失败：{error}"))?;
    Ok(())
}

/// 更新单条连接的可信主机指纹。
pub(crate) fn update_connection_host_key_fingerprint(
    path: &Path,
    profile_id: &str,
    fingerprint: Option<&str>,
) -> Result<(), String> {
    let connection = open_connections_database(path)?;
    let now = current_connection_time_millis();
    connection
        .execute(
            r#"
            UPDATE ssh_connections
            SET host_key_fingerprint = ?2, updated_at_ms = ?3
            WHERE id = ?1
            "#,
            params![profile_id, fingerprint, now],
        )
        .map_err(|error| format!("更新连接主机指纹失败：{error}"))?;
    Ok(())
}

/// 更新最近连接成功时间。
pub(crate) fn update_connection_last_connected_at(
    path: &Path,
    profile_id: &str,
    connected_at_ms: i64,
) -> Result<(), String> {
    let connection = open_connections_database(path)?;
    connection
        .execute(
            r#"
            UPDATE ssh_connections
            SET last_connected_at_ms = ?2
            WHERE id = ?1
            "#,
            params![profile_id, connected_at_ms],
        )
        .map_err(|error| format!("更新连接最近使用时间失败：{error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{env, fs};

    use super::*;

    /// 构造连接数据库测试路径。
    fn test_connections_database_path(name: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "logclinic3-connections-test-{}-{name}.db",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        path
    }

    /// 构造测试连接配置。
    fn test_profile(id: &str) -> ConnectionProfile {
        let now = current_connection_time_millis();
        ConnectionProfile {
            id: id.to_string(),
            name: "测试连接".to_string(),
            host: "127.0.0.1".to_string(),
            port: 22,
            username: "root".to_string(),
            encrypted_password: "v1:nonce:cipher".to_string(),
            host_key_fingerprint: None,
            created_at_ms: now,
            updated_at_ms: now,
            last_connected_at_ms: None,
        }
    }

    /// 验证连接数据库可以完成基础 CRUD。
    #[test]
    fn 连接数据库可以读写更新和删除() {
        let path = test_connections_database_path("crud");
        let mut profile = test_profile("conn-1");
        insert_connection_profile(&path, &profile).unwrap();
        let loaded = load_connection_profiles(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "测试连接");

        profile.name = "修改后连接".to_string();
        profile.host_key_fingerprint = Some("SHA256:test".to_string());
        update_connection_profile(&path, &profile).unwrap();
        let loaded = load_connection_profiles(&path).unwrap();
        assert_eq!(loaded[0].name, "修改后连接");
        assert_eq!(
            loaded[0].host_key_fingerprint.as_deref(),
            Some("SHA256:test")
        );

        delete_connection_profile(&path, "conn-1").unwrap();
        assert!(load_connection_profiles(&path).unwrap().is_empty());
        let _ = fs::remove_file(&path);
    }

    /// 验证未来版本数据库不会被当前程序误读。
    #[test]
    fn 连接数据库未来版本会拒绝读取() {
        let path = test_connections_database_path("future");
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "user_version", 999i64)
            .unwrap();
        let error = load_connection_profiles(&path).expect_err("未来版本必须返回错误");
        assert!(error.contains("高于当前支持版本"));
        let _ = fs::remove_file(&path);
    }
}
