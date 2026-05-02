use crate::ids::{ChatId, UserId};
use async_trait::async_trait;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRecord {
    pub id: i64,
    pub chat_id: ChatId,
    pub user_id: Option<UserId>,
    pub kind: MemoryKind,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryKind {
    Person,
    Preference,
    Relationship,
    GroupNorm,
    RunningJoke,
    Personality,
}

impl MemoryKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Preference => "preference",
            Self::Relationship => "relationship",
            Self::GroupNorm => "group_norm",
            Self::RunningJoke => "running_joke",
            Self::Personality => "personality",
        }
    }
}

impl FromStr for MemoryKind {
    type Err = MemoryStoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "person" => Ok(Self::Person),
            "preference" => Ok(Self::Preference),
            "relationship" => Ok(Self::Relationship),
            "group_norm" => Ok(Self::GroupNorm),
            "running_joke" => Ok(Self::RunningJoke),
            "personality" => Ok(Self::Personality),
            _ => Err(MemoryStoreError::InvalidKind(value.to_owned())),
        }
    }
}

#[async_trait]
pub trait MemoryStore: Send + Sync {
    async fn add_memory(
        &self,
        chat_id: ChatId,
        user_id: Option<UserId>,
        kind: MemoryKind,
        content: &str,
    ) -> Result<MemoryRecord, MemoryStoreError>;

    async fn list_memories(&self, chat_id: ChatId) -> Result<Vec<MemoryRecord>, MemoryStoreError>;

    async fn forget_all(&self, chat_id: ChatId) -> Result<u64, MemoryStoreError>;
}

#[async_trait]
pub trait ChatSettingsStore: Send + Sync {
    async fn voice_replies_enabled(&self, chat_id: ChatId) -> Result<bool, MemoryStoreError>;

    async fn set_voice_replies_enabled(
        &self,
        chat_id: ChatId,
        enabled: bool,
    ) -> Result<(), MemoryStoreError>;
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryStoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] sqlx::Error),
    #[error("invalid memory kind `{0}`")]
    InvalidKind(String),
    #[error("user id `{0}` cannot be stored as sqlite integer")]
    UserIdOutOfRange(u64),
    #[error("stored user id `{0}` cannot be represented as UserId")]
    InvalidStoredUserId(i64),
}
