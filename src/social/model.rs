//! Wire types for the `/api/v2/social` contract.
//!
//! Field names are camelCase because the Nuxt client consumes them directly.
//! Relationships are always computed for the authenticated viewer; the server
//! never trusts a relationship supplied by the client.

use crate::forum::ForumUser;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Relationship {
    #[serde(rename = "self")]
    Current,
    #[serde(rename = "none")]
    None,
    OutgoingPending,
    IncomingPending,
    Friends,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessagePolicy {
    Everyone,
    Friends,
    Nobody,
}

impl MessagePolicy {
    pub fn from_db(value: &str) -> Self {
        match value {
            "friends" => Self::Friends,
            "nobody" => Self::Nobody,
            _ => Self::Everyone,
        }
    }

    pub fn as_db(self) -> &'static str {
        match self {
            Self::Everyone => "everyone",
            Self::Friends => "friends",
            Self::Nobody => "nobody",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FriendRequestPolicy {
    Everyone,
    Nobody,
}

impl FriendRequestPolicy {
    pub fn from_db(value: &str) -> Self {
        match value {
            "nobody" => Self::Nobody,
            _ => Self::Everyone,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    Pending,
    Accepted,
    Rejected,
    Cancelled,
}

impl RequestStatus {
    pub fn from_db(value: &str) -> Self {
        match value {
            "accepted" => Self::Accepted,
            "rejected" => Self::Rejected,
            "cancelled" => Self::Cancelled,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SocialUser {
    pub id: i32,
    pub name: String,
    pub avatar: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    pub is_admin: bool,
    pub relationship: Relationship,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<i64>,
    pub can_message: bool,
}

impl SocialUser {
    pub fn new(
        base: ForumUser,
        relationship: Relationship,
        request_id: Option<i64>,
        can_message: bool,
    ) -> Self {
        Self {
            id: base.id,
            name: base.name,
            avatar: base.avatar,
            avatar_url: base.avatar_url,
            is_admin: base.is_admin,
            relationship,
            request_id,
            can_message,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Friendship {
    pub user: SocialUser,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentPost {
    pub id: i32,
    pub title: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SocialProfile {
    #[serde(flatten)]
    pub user: SocialUser,
    pub recent_posts: Vec<RecentPost>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FriendRequest {
    pub id: i64,
    pub sender: ForumUser,
    pub recipient: ForumUser,
    pub message: String,
    pub status: RequestStatus,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceEntry {
    pub user_id: i32,
    pub status: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceSnapshot {
    pub users: Vec<PresenceEntry>,
    pub generated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SocialPreferences {
    pub message_policy: MessagePolicy,
    pub friend_request_policy: FriendRequestPolicy,
}

impl Default for SocialPreferences {
    fn default() -> Self {
        Self {
            message_policy: MessagePolicy::Everyone,
            friend_request_policy: FriendRequestPolicy::Everyone,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<i64>,
}

impl<T> Page<T> {
    pub fn new(items: Vec<T>, next_cursor: Option<String>) -> Self {
        Self {
            items,
            next_cursor,
            total: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateFriendRequestPayload {
    pub recipient_id: Option<i64>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreferencesPatch {
    pub message_policy: Option<String>,
    pub friend_request_policy: Option<String>,
}
