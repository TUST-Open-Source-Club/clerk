use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::{sqlite::SqlitePoolOptions, Pool, Row, Sqlite};
use std::path::Path;

#[derive(Debug, Clone, sqlx::FromRow, PartialEq)]
pub struct Message {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow, PartialEq)]
pub struct Session {
    pub id: String,
    pub title: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct Store {
    pool: Pool<Sqlite>,
}

impl Store {
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建数据库父目录失败: {}", parent.display()))?;
        }

        let url = if path.is_absolute() {
            format!("sqlite://{}?mode=rwc", path.display())
        } else {
            format!("sqlite:{}?mode=rwc", path.display())
        };
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&url)
            .await
            .with_context(|| format!("无法连接数据库: {}", path.display()))?;

        Self::migrate(&pool).await?;
        Ok(Self { pool })
    }

    async fn migrate(pool: &Pool<Sqlite>) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                title TEXT,
                created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_messages_session_id ON messages(session_id);
            "#,
        )
        .execute(pool)
        .await
        .context("数据库迁移失败")?;
        Ok(())
    }

    pub async fn create_session(&self, id: &str, title: Option<&str>) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO sessions (id, title, created_at, updated_at) VALUES (?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)"
        )
        .bind(id)
        .bind(title)
        .execute(&self.pool)
        .await
        .context("创建会话失败")?;
        Ok(())
    }

    pub async fn list_sessions(&self) -> Result<Vec<Session>> {
        let sessions = sqlx::query_as::<_, Session>(
            "SELECT id, title, created_at, updated_at FROM sessions ORDER BY updated_at DESC, id DESC",
        )
        .fetch_all(&self.pool)
        .await
        .context("列出会话失败")?;
        Ok(sessions)
    }

    pub async fn get_session(&self, id: &str) -> Result<Option<Session>> {
        let session = sqlx::query_as::<_, Session>(
            "SELECT id, title, created_at, updated_at FROM sessions WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("获取会话失败")?;
        Ok(session)
    }

    pub async fn add_message(&self,
        session_id: &str,
        role: &str,
        content: &str,
    ) -> Result<Message> {
        self.touch_session(session_id).await?;

        let id = sqlx::query(
            "INSERT INTO messages (session_id, role, content) VALUES (?, ?, ?) RETURNING id",
        )
        .bind(session_id)
        .bind(role)
        .bind(content)
        .fetch_one(&self.pool)
        .await
        .context("插入消息失败")?
        .get::<i64, _>("id");

        self.get_message(id).await
    }

    pub async fn get_message(&self, id: i64) -> Result<Message> {
        let message = sqlx::query_as::<_, Message>(
            "SELECT id, session_id, role, content, created_at FROM messages WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .with_context(|| format!("获取消息失败: id={}", id))?;
        Ok(message)
    }

    pub async fn list_messages(&self, session_id: &str) -> Result<Vec<Message>> {
        let messages = sqlx::query_as::<_, Message>(
            "SELECT id, session_id, role, content, created_at FROM messages WHERE session_id = ? ORDER BY id ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .context("列出消息失败")?;
        Ok(messages)
    }

    pub async fn delete_session(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("删除会话失败")?;
        Ok(())
    }

    async fn touch_session(&self, id: &str) -> Result<()> {
        sqlx::query(
            "UPDATE sessions SET updated_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .context("更新会话时间失败")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn create_test_store() -> (Store, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        let store = Store::open(&path).await.unwrap();
        (store, dir)
    }

    #[tokio::test]
    async fn test_create_and_get_session() {
        let (store, _dir) = create_test_store().await;
        store.create_session("s1", Some("Test Session")).await.unwrap();

        let session = store.get_session("s1").await.unwrap().unwrap();
        assert_eq!(session.id, "s1");
        assert_eq!(session.title.as_deref(), Some("Test Session"));
    }

    #[tokio::test]
    async fn test_list_sessions_order() {
        let (store, _dir) = create_test_store().await;
        store.create_session("s1", Some("A")).await.unwrap();
        store.create_session("s2", Some("B")).await.unwrap();

        let sessions = store.list_sessions().await.unwrap();
        assert_eq!(sessions.len(), 2);
        // 最新更新的排在最前
        assert_eq!(sessions[0].id, "s2");
    }

    #[tokio::test]
    async fn test_add_and_list_messages() {
        let (store, _dir) = create_test_store().await;
        store.create_session("s1", Some("Test")).await.unwrap();

        let m1 = store.add_message("s1", "user", "hello").await.unwrap();
        assert_eq!(m1.role, "user");
        assert_eq!(m1.content, "hello");

        let m2 = store.add_message("s1", "assistant", "hi").await.unwrap();
        let messages = store.list_messages("s1").await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].content, "hi");
        assert_eq!(messages[1].id, m2.id);
    }

    #[tokio::test]
    async fn test_delete_session_cascades_messages() {
        let (store, _dir) = create_test_store().await;
        store.create_session("s1", Some("Test")).await.unwrap();
        store.add_message("s1", "user", "hello").await.unwrap();

        store.delete_session("s1").await.unwrap();
        assert!(store.get_session("s1").await.unwrap().is_none());
        assert!(store.list_messages("s1").await.unwrap().is_empty());
    }
}
