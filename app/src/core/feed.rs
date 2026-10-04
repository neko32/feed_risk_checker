//! フィードのドメインモデル。
//!
//! すべてのフィールドは newtype でラップし、生成時にバリデーションを行う。
//! 無効な値を持つ `Feed` が構築できないことをコンパイラと型で保証する方針。

use chrono::{DateTime, Utc};
use thiserror::Error;

/// フィードのバリデーションエラー。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum FeedValidationError {
    #[error("user_name must not be empty")]
    EmptyUserName,
    #[error("message must not be empty")]
    EmptyMessage,
    #[error("message exceeds max length of {max} bytes (got {actual})")]
    MessageTooLong { max: usize, actual: usize },
}

/// フィードメッセージの最大長（バイト数）。LinkedIn投稿POCの想定として十分な余裕を持たせる。
pub const MAX_MESSAGE_LEN: usize = 4096;

/// フィードの一意識別子。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FeedId(u64);

impl FeedId {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(&self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for FeedId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 投稿者のユーザー名。空文字は許容しない。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UserName(String);

impl UserName {
    /// # Errors
    /// 値が空文字（または空白のみ）の場合に `EmptyUserName` を返す。
    pub fn new(value: impl Into<String>) -> Result<Self, FeedValidationError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(FeedValidationError::EmptyUserName);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

/// フィードの本文。空文字・最大長超過は許容しない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageBody(String);

impl MessageBody {
    /// # Errors
    /// 値が空文字（または空白のみ）、または `MAX_MESSAGE_LEN` を超える場合にエラーを返す。
    pub fn new(value: impl Into<String>) -> Result<Self, FeedValidationError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(FeedValidationError::EmptyMessage);
        }
        if value.len() > MAX_MESSAGE_LEN {
            return Err(FeedValidationError::MessageTooLong {
                max: MAX_MESSAGE_LEN,
                actual: value.len(),
            });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

/// 送信時刻（UTC）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TimeSentUtc(DateTime<Utc>);

impl TimeSentUtc {
    #[must_use]
    pub fn new(value: DateTime<Utc>) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(&self) -> DateTime<Utc> {
        self.0
    }

    #[must_use]
    pub fn to_rfc3339(&self) -> String {
        self.0.to_rfc3339()
    }
}

/// メッセージフィード本体。 `{id, user_name, message, time_sent}` を表す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feed {
    pub id: FeedId,
    pub user_name: UserName,
    pub message: MessageBody,
    pub time_sent: TimeSentUtc,
}

impl Feed {
    #[must_use]
    pub fn new(
        id: FeedId,
        user_name: UserName,
        message: MessageBody,
        time_sent: TimeSentUtc,
    ) -> Self {
        Self {
            id,
            user_name,
            message,
            time_sent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_name_rejects_empty() {
        assert_eq!(UserName::new(""), Err(FeedValidationError::EmptyUserName));
        assert_eq!(
            UserName::new("   "),
            Err(FeedValidationError::EmptyUserName)
        );
    }

    #[test]
    fn user_name_accepts_non_empty() {
        let name = UserName::new("tanumaru").unwrap();
        assert_eq!(name.as_str(), "tanumaru");
    }

    #[test]
    fn message_rejects_empty() {
        assert_eq!(MessageBody::new(""), Err(FeedValidationError::EmptyMessage));
    }

    #[test]
    fn message_rejects_too_long() {
        let too_long = "a".repeat(MAX_MESSAGE_LEN + 1);
        let err = MessageBody::new(too_long).unwrap_err();
        assert_eq!(
            err,
            FeedValidationError::MessageTooLong {
                max: MAX_MESSAGE_LEN,
                actual: MAX_MESSAGE_LEN + 1
            }
        );
    }

    #[test]
    fn message_accepts_valid() {
        let msg = MessageBody::new("hello world").unwrap();
        assert_eq!(msg.as_str(), "hello world");
    }

    #[test]
    fn feed_id_roundtrip() {
        let id = FeedId::new(42);
        assert_eq!(id.value(), 42);
        assert_eq!(id.to_string(), "42");
    }

    #[test]
    fn feed_constructs_with_valid_parts() {
        let feed = Feed::new(
            FeedId::new(1),
            UserName::new("user1").unwrap(),
            MessageBody::new("hi").unwrap(),
            TimeSentUtc::new(Utc::now()),
        );
        assert_eq!(feed.id.value(), 1);
        assert_eq!(feed.user_name.as_str(), "user1");
    }
}
