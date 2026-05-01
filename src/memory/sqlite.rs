use super::store::{MemoryKind, MemoryRecord, MemoryStore, MemoryStoreError};
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
}
