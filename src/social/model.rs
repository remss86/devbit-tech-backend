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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<FriendMetadata>,
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

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FriendMetadata {
    pub note: String,
    pub starred: bool,
    pub group_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FriendGroup {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SocialSummary {
    pub total: i64,
    pub online: i64,
    pub starred: i64,
    pub incoming: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPreferences {
    pub pinned: bool,
    pub muted: bool,
    pub archived: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSummary {
    pub user: SocialUser,
    pub last_message: Option<WorkspaceMessage>,
    pub unread: i64,
    pub preferences: ConversationPreferences,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMessage {
    pub id: i32,
    pub sender: ForumUser,
    pub recipient: ForumUser,
    pub content: String,
    pub created_at: String,
    pub is_read: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_message_id: Option<String>,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FriendMetadataPatch {
    pub note: Option<String>,
    pub starred: Option<bool>,
    /// Missing means "keep", `null` means "ungroup", a number means "move to
    /// this group". The custom decoder is required because serde otherwise
    /// collapses both missing and null into `None`.
    #[serde(default, deserialize_with = "deserialize_nullable_i64")]
    pub group_id: Option<Option<i64>>,
}

fn deserialize_nullable_i64<'de, D>(deserializer: D) -> Result<Option<Option<i64>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<i64>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Deserialize)]
pub struct GroupPayload {
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPreferencesPatch {
    pub pinned: Option<bool>,
    pub muted: Option<bool>,
    pub archived: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendWorkspaceMessagePayload {
    pub recipient_id: Option<i64>,
    pub content: Option<String>,
    pub client_message_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadConversationPayload {
    pub last_read_message_id: Option<i64>,
}
