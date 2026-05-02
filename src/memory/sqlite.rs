use super::store::{ChatSettingsStore, MemoryKind, MemoryRecord, MemoryStore, MemoryStoreError};
use crate::ids::{ChatId, UserId};
use async_trait::async_trait;
use sqlx::{Row, SqlitePool, sqlite::SqlitePoolOptions};
use std::str::FromStr;

#[derive(Debug, Clone)]
pub struct SqliteMemoryStore {
    pool: SqlitePool,
}

impl SqliteMemoryStore {
    pub async fn connect(database_url: &str) -> Result<Self, MemoryStoreError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    async fn migrate(&self) -> Result<(), MemoryStoreError> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS memories (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                chat_id INTEGER NOT NULL,
                user_id INTEGER,
                kind TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_memories_chat_id ON memories(chat_id)")
            .execute(&self.pool)
            .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS chat_settings (
                chat_id INTEGER PRIMARY KEY,
                voice_replies_enabled INTEGER NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}

#[async_trait]
impl MemoryStore for SqliteMemoryStore {
    async fn add_memory(
        &self,
        chat_id: ChatId,
        user_id: Option<UserId>,
        kind: MemoryKind,
        content: &str,
    ) -> Result<MemoryRecord, MemoryStoreError> {
        let user_id_i64 = user_id
            .map(|id| i64::try_from(id.0).map_err(|_| MemoryStoreError::UserIdOutOfRange(id.0)))
            .transpose()?;

        let result = sqlx::query(
            "INSERT INTO memories (chat_id, user_id, kind, content) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(chat_id.0)
        .bind(user_id_i64)
        .bind(kind.as_str())
        .bind(content)
        .execute(&self.pool)
        .await?;

        Ok(MemoryRecord {
            id: result.last_insert_rowid(),
            chat_id,
            user_id,
            kind,
            content: content.to_owned(),
        })
    }

    async fn list_memories(&self, chat_id: ChatId) -> Result<Vec<MemoryRecord>, MemoryStoreError> {
        let rows = sqlx::query(
            "SELECT id, chat_id, user_id, kind, content FROM memories WHERE chat_id = ?1 ORDER BY id",
        )
        .bind(chat_id.0)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                let kind_raw: String = row.get("kind");
                let user_id_raw: Option<i64> = row.get("user_id");
                let user_id = user_id_raw
                    .map(|id| {
                        u64::try_from(id)
                            .map(UserId)
                            .map_err(|_| MemoryStoreError::InvalidStoredUserId(id))
                    })
                    .transpose()?;

                Ok(MemoryRecord {
                    id: row.get("id"),
                    chat_id: ChatId(row.get("chat_id")),
                    user_id,
                    kind: MemoryKind::from_str(&kind_raw)?,
                    content: row.get("content"),
                })
            })
            .collect()
    }

    async fn forget_all(&self, chat_id: ChatId) -> Result<u64, MemoryStoreError> {
        let result = sqlx::query("DELETE FROM memories WHERE chat_id = ?1")
            .bind(chat_id.0)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

#[async_trait]
impl ChatSettingsStore for SqliteMemoryStore {
    async fn voice_replies_enabled(&self, chat_id: ChatId) -> Result<bool, MemoryStoreError> {
        let row = sqlx::query("SELECT voice_replies_enabled FROM chat_settings WHERE chat_id = ?1")
            .bind(chat_id.0)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row
            .map(|row| row.get::<i64, _>("voice_replies_enabled") != 0)
            .unwrap_or(true))
    }

    async fn set_voice_replies_enabled(
        &self,
        chat_id: ChatId,
        enabled: bool,
    ) -> Result<(), MemoryStoreError> {
        sqlx::query(
            r#"
            INSERT INTO chat_settings (chat_id, voice_replies_enabled)
            VALUES (?1, ?2)
            ON CONFLICT(chat_id) DO UPDATE SET voice_replies_enabled = excluded.voice_replies_enabled
            "#,
        )
        .bind(chat_id.0)
        .bind(if enabled { 1_i64 } else { 0_i64 })
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn list_memories_should_return_records_for_one_chat() {
        let store = SqliteMemoryStore::connect("sqlite::memory:")
            .await
            .expect("store should connect");

        store
            .add_memory(
                ChatId(1),
                Some(UserId(2)),
                MemoryKind::Preference,
                "likes terse answers",
            )
            .await
            .expect("memory should insert");
        store
            .add_memory(ChatId(9), None, MemoryKind::GroupNorm, "unrelated")
            .await
            .expect("memory should insert");

        let memories = store
            .list_memories(ChatId(1))
            .await
            .expect("list should work");
        assert_eq!(memories[0].content, "likes terse answers");
    }

    #[tokio::test]
    async fn forget_all_should_remove_only_one_chat() {
        let store = SqliteMemoryStore::connect("sqlite::memory:")
            .await
            .expect("store should connect");

        store
            .add_memory(ChatId(1), None, MemoryKind::Personality, "dry humor")
            .await
            .expect("memory should insert");
        store
            .add_memory(ChatId(2), None, MemoryKind::Personality, "formal")
            .await
            .expect("memory should insert");

        let removed = store
            .forget_all(ChatId(1))
            .await
            .expect("delete should work");
        assert_eq!(removed, 1);
    }

    #[tokio::test]
    async fn voice_replies_should_default_to_enabled() {
        let store = SqliteMemoryStore::connect("sqlite::memory:")
            .await
            .expect("store should connect");

        assert!(
            store
                .voice_replies_enabled(ChatId(1))
                .await
                .expect("settings should load")
        );
    }

    #[tokio::test]
    async fn voice_replies_should_persist_per_chat() {
        let store = SqliteMemoryStore::connect("sqlite::memory:")
            .await
            .expect("store should connect");

        store
            .set_voice_replies_enabled(ChatId(1), false)
            .await
            .expect("setting should save");

        assert!(
            !store
                .voice_replies_enabled(ChatId(1))
                .await
                .expect("setting should load")
        );
        assert!(
            store
                .voice_replies_enabled(ChatId(2))
                .await
                .expect("other chat should use default")
        );
    }
}
