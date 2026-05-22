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
/// - v1 数据库只有 SSH 连接表；升级到 v2 时新增分类表和连接的 `category_id` 列，旧连接默认继续显示在根层。
/// - v3 新增 SMB 连接表；旧数据库升级后没有 SMB 连接，分类删除校验会同时检查 SSH 和 SMB 两类连接。
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
            CREATE TABLE IF NOT EXISTS connection_categories (
                id TEXT PRIMARY KEY NOT NULL,
                parent_id TEXT,
                name TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_connection_categories_parent_created
                ON connection_categories(parent_id, created_at_ms ASC);

            CREATE TABLE IF NOT EXISTS ssh_connections (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL,
                host TEXT NOT NULL,
                port INTEGER NOT NULL,
                username TEXT NOT NULL,
                category_id TEXT,
                encrypted_password TEXT NOT NULL,
                host_key_fingerprint TEXT,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                last_connected_at_ms INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_ssh_connections_updated_at
                ON ssh_connections(updated_at_ms DESC);

            CREATE TABLE IF NOT EXISTS smb_connections (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL,
                host TEXT NOT NULL,
                port INTEGER NOT NULL,
                share TEXT NOT NULL,
                initial_path TEXT NOT NULL,
                username TEXT NOT NULL,
                category_id TEXT,
                encrypted_password TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                last_connected_at_ms INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_smb_connections_updated_at
                ON smb_connections(updated_at_ms DESC);
            "#,
        )
        .map_err(|error| format!("初始化连接数据库失败：{error}"))?;

    if !sqlite_table_has_column(connection, "ssh_connections", "category_id")? {
        connection
            .execute(
                "ALTER TABLE ssh_connections ADD COLUMN category_id TEXT",
                [],
            )
            .map_err(|error| format!("升级连接数据库分类字段失败：{error}"))?;
    }

    connection
        .execute(
            r#"
            CREATE INDEX IF NOT EXISTS idx_ssh_connections_category_updated
                ON ssh_connections(category_id, updated_at_ms DESC)
            "#,
            [],
        )
        .map_err(|error| format!("初始化连接分类索引失败：{error}"))?;

    connection
        .execute(
            r#"
            CREATE INDEX IF NOT EXISTS idx_smb_connections_category_updated
                ON smb_connections(category_id, updated_at_ms DESC)
            "#,
            [],
        )
        .map_err(|error| format!("初始化连接分类索引失败：{error}"))?;

    if version < CONNECTIONS_DATABASE_SCHEMA_VERSION {
        connection
            .pragma_update(None, "user_version", CONNECTIONS_DATABASE_SCHEMA_VERSION)
            .map_err(|error| format!("写入连接数据库版本失败：{error}"))?;
    }
    Ok(())
}

/// 判断 SQLite 表是否存在指定列。
///
/// 业务意图：
/// - `ALTER TABLE ADD COLUMN` 在重复添加时会失败；升级 v1 数据库和创建新库都会走初始化入口，因此必须先读取 `PRAGMA table_info`。
fn sqlite_table_has_column(
    connection: &Connection,
    table_name: &str,
    column_name: &str,
) -> Result<bool, String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table_name})"))
        .map_err(|error| format!("读取连接数据库表结构失败：{error}"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| format!("读取连接数据库表结构失败：{error}"))?;
    for row in rows {
        let name = row.map_err(|error| format!("解析连接数据库表结构失败：{error}"))?;
        if name == column_name {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 加载连接分类列表。
pub(crate) fn load_connection_categories(path: &Path) -> Result<Vec<ConnectionCategory>, String> {
    let connection = open_connections_database(path)?;
    let mut statement = connection
        .prepare(
            r#"
            SELECT id, parent_id, name, created_at_ms, updated_at_ms
            FROM connection_categories
            ORDER BY created_at_ms ASC, updated_at_ms ASC
            "#,
        )
        .map_err(|error| format!("读取连接分类失败：{error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(ConnectionCategory {
                id: row.get(0)?,
                parent_id: row.get(1)?,
                name: row.get(2)?,
                created_at_ms: row.get(3)?,
                updated_at_ms: row.get(4)?,
            })
        })
        .map_err(|error| format!("读取连接分类失败：{error}"))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析连接分类失败：{error}"))
}

/// 加载 SSH 连接列表。
pub(crate) fn load_connection_profiles(path: &Path) -> Result<Vec<ConnectionProfile>, String> {
    let connection = open_connections_database(path)?;
    let mut statement = connection
        .prepare(
            r#"
            SELECT id, name, host, port, username, category_id, encrypted_password,
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
                category_id: row.get(5)?,
                encrypted_password: row.get(6)?,
                host_key_fingerprint: row.get(7)?,
                created_at_ms: row.get(8)?,
                updated_at_ms: row.get(9)?,
                last_connected_at_ms: row.get(10)?,
            })
        })
        .map_err(|error| format!("读取连接列表失败：{error}"))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析连接列表失败：{error}"))
}

/// 加载 SMB 连接列表。
pub(crate) fn load_smb_connection_profiles(
    path: &Path,
) -> Result<Vec<SmbConnectionProfile>, String> {
    let connection = open_connections_database(path)?;
    let mut statement = connection
        .prepare(
            r#"
            SELECT id, name, host, port, share, initial_path, username, category_id,
                   encrypted_password, created_at_ms, updated_at_ms, last_connected_at_ms
            FROM smb_connections
            ORDER BY updated_at_ms DESC, created_at_ms DESC
            "#,
        )
        .map_err(|error| format!("读取 SMB 连接列表失败：{error}"))?;
    let rows = statement
        .query_map([], |row| {
            let port_i64: i64 = row.get(3)?;
            let port = u16::try_from(port_i64)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, port_i64))?;
            Ok(SmbConnectionProfile {
                id: row.get(0)?,
                name: row.get(1)?,
                host: row.get(2)?,
                port,
                share: row.get(4)?,
                initial_path: row.get(5)?,
                username: row.get(6)?,
                category_id: row.get(7)?,
                encrypted_password: row.get(8)?,
                created_at_ms: row.get(9)?,
                updated_at_ms: row.get(10)?,
                last_connected_at_ms: row.get(11)?,
            })
        })
        .map_err(|error| format!("读取 SMB 连接列表失败：{error}"))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 SMB 连接列表失败：{error}"))
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
                (id, name, host, port, username, category_id, encrypted_password, host_key_fingerprint,
                 created_at_ms, updated_at_ms, last_connected_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            "#,
            params![
                profile.id,
                profile.name,
                profile.host,
                i64::from(profile.port),
                profile.username,
                profile.category_id,
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

/// 新增 SMB 连接。
pub(crate) fn insert_smb_connection_profile(
    path: &Path,
    profile: &SmbConnectionProfile,
) -> Result<(), String> {
    let connection = open_connections_database(path)?;
    connection
        .execute(
            r#"
            INSERT INTO smb_connections
                (id, name, host, port, share, initial_path, username, category_id,
                 encrypted_password, created_at_ms, updated_at_ms, last_connected_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            "#,
            params![
                profile.id,
                profile.name,
                profile.host,
                i64::from(profile.port),
                profile.share,
                profile.initial_path,
                profile.username,
                profile.category_id,
                profile.encrypted_password,
                profile.created_at_ms,
                profile.updated_at_ms,
                profile.last_connected_at_ms,
            ],
        )
        .map_err(|error| format!("保存 SMB 连接失败：{error}"))?;
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
                category_id = ?6,
                encrypted_password = ?7,
                host_key_fingerprint = ?8,
                updated_at_ms = ?9,
                last_connected_at_ms = ?10
            WHERE id = ?1
            "#,
            params![
                profile.id,
                profile.name,
                profile.host,
                i64::from(profile.port),
                profile.username,
                profile.category_id,
                profile.encrypted_password,
                profile.host_key_fingerprint,
                profile.updated_at_ms,
                profile.last_connected_at_ms,
            ],
        )
        .map_err(|error| format!("更新连接失败：{error}"))?;
    Ok(())
}

/// 更新 SMB 连接。
pub(crate) fn update_smb_connection_profile(
    path: &Path,
    profile: &SmbConnectionProfile,
) -> Result<(), String> {
    let connection = open_connections_database(path)?;
    connection
        .execute(
            r#"
            UPDATE smb_connections
            SET name = ?2,
                host = ?3,
                port = ?4,
                share = ?5,
                initial_path = ?6,
                username = ?7,
                category_id = ?8,
                encrypted_password = ?9,
                updated_at_ms = ?10,
                last_connected_at_ms = ?11
            WHERE id = ?1
            "#,
            params![
                profile.id,
                profile.name,
                profile.host,
                i64::from(profile.port),
                profile.share,
                profile.initial_path,
                profile.username,
                profile.category_id,
                profile.encrypted_password,
                profile.updated_at_ms,
                profile.last_connected_at_ms,
            ],
        )
        .map_err(|error| format!("更新 SMB 连接失败：{error}"))?;
    Ok(())
}

/// 新增连接分类。
pub(crate) fn insert_connection_category(
    path: &Path,
    category: &ConnectionCategory,
) -> Result<(), String> {
    let connection = open_connections_database(path)?;
    connection
        .execute(
            r#"
            INSERT INTO connection_categories
                (id, parent_id, name, created_at_ms, updated_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                category.id,
                category.parent_id,
                category.name,
                category.created_at_ms,
                category.updated_at_ms,
            ],
        )
        .map_err(|error| format!("保存连接分类失败：{error}"))?;
    Ok(())
}

/// 更新连接分类。
pub(crate) fn update_connection_category(
    path: &Path,
    category: &ConnectionCategory,
) -> Result<(), String> {
    let connection = open_connections_database(path)?;
    connection
        .execute(
            r#"
            UPDATE connection_categories
            SET parent_id = ?2,
                name = ?3,
                updated_at_ms = ?4
            WHERE id = ?1
            "#,
            params![
                category.id,
                category.parent_id,
                category.name,
                category.updated_at_ms,
            ],
        )
        .map_err(|error| format!("更新连接分类失败：{error}"))?;
    Ok(())
}

/// 删除空连接分类。
///
/// 边界条件：
/// - 有子分类或连接挂载时直接返回中文错误，避免误删后连接“掉回根层”造成用户以为数据丢失。
pub(crate) fn delete_connection_category(path: &Path, category_id: &str) -> Result<(), String> {
    let connection = open_connections_database(path)?;
    let child_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM connection_categories WHERE parent_id = ?1",
            params![category_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("检查连接分类子分类失败：{error}"))?;
    if child_count > 0 {
        return Err("分类下还有子分类，不能删除".to_string());
    }
    let ssh_profile_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM ssh_connections WHERE category_id = ?1",
            params![category_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("检查连接分类下连接失败：{error}"))?;
    let smb_profile_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM smb_connections WHERE category_id = ?1",
            params![category_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("检查连接分类下 SMB 连接失败：{error}"))?;
    if ssh_profile_count + smb_profile_count > 0 {
        return Err("分类下还有连接，不能删除".to_string());
    }

    connection
        .execute(
            "DELETE FROM connection_categories WHERE id = ?1",
            params![category_id],
        )
        .map_err(|error| format!("删除连接分类失败：{error}"))?;
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

/// 删除 SMB 连接。
pub(crate) fn delete_smb_connection_profile(path: &Path, profile_id: &str) -> Result<(), String> {
    let connection = open_connections_database(path)?;
    connection
        .execute(
            "DELETE FROM smb_connections WHERE id = ?1",
            params![profile_id],
        )
        .map_err(|error| format!("删除 SMB 连接失败：{error}"))?;
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

/// 更新 SMB 最近连接成功时间。
pub(crate) fn update_smb_connection_last_connected_at(
    path: &Path,
    profile_id: &str,
    connected_at_ms: i64,
) -> Result<(), String> {
    let connection = open_connections_database(path)?;
    connection
        .execute(
            r#"
            UPDATE smb_connections
            SET last_connected_at_ms = ?2
            WHERE id = ?1
            "#,
            params![profile_id, connected_at_ms],
        )
        .map_err(|error| format!("更新 SMB 连接最近使用时间失败：{error}"))?;
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
            category_id: None,
            encrypted_password: "v1:nonce:cipher".to_string(),
            host_key_fingerprint: None,
            created_at_ms: now,
            updated_at_ms: now,
            last_connected_at_ms: None,
        }
    }

    /// 构造测试 SMB 连接配置。
    fn test_smb_profile(id: &str) -> SmbConnectionProfile {
        let now = current_connection_time_millis();
        SmbConnectionProfile {
            id: id.to_string(),
            name: "测试 SMB".to_string(),
            host: "fileserver".to_string(),
            port: DEFAULT_SMB_PORT,
            share: "logs".to_string(),
            initial_path: "/".to_string(),
            username: "root".to_string(),
            category_id: None,
            encrypted_password: "v1:nonce:cipher".to_string(),
            created_at_ms: now,
            updated_at_ms: now,
            last_connected_at_ms: None,
        }
    }

    /// 构造测试连接分类。
    fn test_category(id: &str, parent_id: Option<&str>, created_at_ms: i64) -> ConnectionCategory {
        ConnectionCategory {
            id: id.to_string(),
            parent_id: parent_id.map(ToString::to_string),
            name: format!("分类 {id}"),
            created_at_ms,
            updated_at_ms: created_at_ms,
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
        profile.category_id = Some("cat-1".to_string());
        profile.host_key_fingerprint = Some("SHA256:test".to_string());
        update_connection_profile(&path, &profile).unwrap();
        let loaded = load_connection_profiles(&path).unwrap();
        assert_eq!(loaded[0].name, "修改后连接");
        assert_eq!(loaded[0].category_id.as_deref(), Some("cat-1"));
        assert_eq!(
            loaded[0].host_key_fingerprint.as_deref(),
            Some("SHA256:test")
        );

        delete_connection_profile(&path, "conn-1").unwrap();
        assert!(load_connection_profiles(&path).unwrap().is_empty());
        let _ = fs::remove_file(&path);
    }

    /// 验证 SMB 连接数据库可以完成基础 CRUD。
    ///
    /// 业务风险：
    /// - SMB 与 SSH 共用分类和密码加密规则，但落在不同表；CRUD 测试可以防止 schema v3 字段顺序或端口转换写错。
    #[test]
    fn smb_连接数据库可以读写更新和删除() {
        let path = test_connections_database_path("smb-crud");
        let mut profile = test_smb_profile("smb-1");
        insert_smb_connection_profile(&path, &profile).unwrap();
        let loaded = load_smb_connection_profiles(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "测试 SMB");
        assert_eq!(loaded[0].initial_path, "/");

        profile.name = "修改后 SMB".to_string();
        profile.category_id = Some("cat-1".to_string());
        profile.initial_path = "/logs".to_string();
        update_smb_connection_profile(&path, &profile).unwrap();
        let loaded = load_smb_connection_profiles(&path).unwrap();
        assert_eq!(loaded[0].name, "修改后 SMB");
        assert_eq!(loaded[0].category_id.as_deref(), Some("cat-1"));
        assert_eq!(loaded[0].initial_path, "/logs");

        update_smb_connection_last_connected_at(&path, "smb-1", 42).unwrap();
        let loaded = load_smb_connection_profiles(&path).unwrap();
        assert_eq!(loaded[0].last_connected_at_ms, Some(42));

        delete_smb_connection_profile(&path, "smb-1").unwrap();
        assert!(load_smb_connection_profiles(&path).unwrap().is_empty());
        let _ = fs::remove_file(&path);
    }

    /// 验证连接分类可以完成基础 CRUD。
    #[test]
    fn 连接分类数据库可以读写更新和删除空分类() {
        let path = test_connections_database_path("category-crud");
        let mut category = test_category("cat-1", None, 1);
        insert_connection_category(&path, &category).unwrap();
        let loaded = load_connection_categories(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "分类 cat-1");

        category.name = "重命名分类".to_string();
        category.updated_at_ms = 2;
        update_connection_category(&path, &category).unwrap();
        let loaded = load_connection_categories(&path).unwrap();
        assert_eq!(loaded[0].name, "重命名分类");

        delete_connection_category(&path, "cat-1").unwrap();
        assert!(load_connection_categories(&path).unwrap().is_empty());
        let _ = fs::remove_file(&path);
    }

    /// 验证非空分类不会被删除，避免连接或子分类静默丢失层级。
    #[test]
    fn 连接分类非空时拒绝删除() {
        let path = test_connections_database_path("category-non-empty");
        let parent = test_category("cat-parent", None, 1);
        let child = test_category("cat-child", Some("cat-parent"), 2);
        insert_connection_category(&path, &parent).unwrap();
        insert_connection_category(&path, &child).unwrap();
        let error =
            delete_connection_category(&path, "cat-parent").expect_err("有子分类时必须拒绝删除");
        assert!(error.contains("子分类"));

        delete_connection_category(&path, "cat-child").unwrap();
        let mut profile = test_profile("conn-1");
        profile.category_id = Some("cat-parent".to_string());
        insert_connection_profile(&path, &profile).unwrap();
        let error =
            delete_connection_category(&path, "cat-parent").expect_err("有连接时必须拒绝删除");
        assert!(error.contains("连接"));
        let _ = fs::remove_file(&path);
    }

    /// 验证分类删除会同时识别 SMB 连接占用。
    #[test]
    fn 连接分类被_smb_连接占用时拒绝删除() {
        let path = test_connections_database_path("category-smb-non-empty");
        let parent = test_category("cat-parent", None, 1);
        insert_connection_category(&path, &parent).unwrap();
        let mut profile = test_smb_profile("smb-1");
        profile.category_id = Some("cat-parent".to_string());
        insert_smb_connection_profile(&path, &profile).unwrap();

        let error =
            delete_connection_category(&path, "cat-parent").expect_err("有 SMB 连接时必须拒绝删除");
        assert!(error.contains("连接"));
        let _ = fs::remove_file(&path);
    }

    /// 验证 v1 数据库升级到 v3 后旧连接会保留并显示在根层。
    #[test]
    fn 连接数据库_v1_升级后旧连接保留在根层并创建_smb_表() {
        let path = test_connections_database_path("migrate-v1");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                r#"
                CREATE TABLE ssh_connections (
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
                INSERT INTO ssh_connections
                    (id, name, host, port, username, encrypted_password, host_key_fingerprint,
                     created_at_ms, updated_at_ms, last_connected_at_ms)
                VALUES
                    ('conn-old', '旧连接', '127.0.0.1', 22, 'root', 'v1:nonce:cipher',
                     NULL, 1, 1, NULL);
                PRAGMA user_version = 1;
                "#,
            )
            .unwrap();
        drop(connection);

        let loaded = load_connection_profiles(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "conn-old");
        assert!(loaded[0].category_id.is_none());
        assert!(load_connection_categories(&path).unwrap().is_empty());
        assert!(load_smb_connection_profiles(&path).unwrap().is_empty());
        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, CONNECTIONS_DATABASE_SCHEMA_VERSION);
        let _ = fs::remove_file(&path);
    }

    /// 验证 v2 数据库升级到 v3 时新增 SMB 表但不影响已有分类和 SSH 连接。
    #[test]
    fn 连接数据库_v2_升级到_v3_会创建_smb_表() {
        let path = test_connections_database_path("migrate-v2-to-v3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                r#"
                CREATE TABLE connection_categories (
                    id TEXT PRIMARY KEY NOT NULL,
                    parent_id TEXT,
                    name TEXT NOT NULL,
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL
                );
                CREATE TABLE ssh_connections (
                    id TEXT PRIMARY KEY NOT NULL,
                    name TEXT NOT NULL,
                    host TEXT NOT NULL,
                    port INTEGER NOT NULL,
                    username TEXT NOT NULL,
                    category_id TEXT,
                    encrypted_password TEXT NOT NULL,
                    host_key_fingerprint TEXT,
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    last_connected_at_ms INTEGER
                );
                INSERT INTO connection_categories
                    (id, parent_id, name, created_at_ms, updated_at_ms)
                VALUES ('cat-1', NULL, '分类', 1, 1);
                INSERT INTO ssh_connections
                    (id, name, host, port, username, category_id, encrypted_password, host_key_fingerprint,
                     created_at_ms, updated_at_ms, last_connected_at_ms)
                VALUES
                    ('conn-old', '旧连接', '127.0.0.1', 22, 'root', 'cat-1', 'v1:nonce:cipher',
                     NULL, 1, 1, NULL);
                PRAGMA user_version = 2;
                "#,
            )
            .unwrap();
        drop(connection);

        assert_eq!(load_connection_profiles(&path).unwrap().len(), 1);
        assert_eq!(load_connection_categories(&path).unwrap().len(), 1);
        assert!(load_smb_connection_profiles(&path).unwrap().is_empty());
        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, CONNECTIONS_DATABASE_SCHEMA_VERSION);
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
