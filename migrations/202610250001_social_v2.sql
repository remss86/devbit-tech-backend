-- Social V2: symmetric friendships, friend requests, blocks, preferences and
-- per-user message history visibility.
--
-- The legacy `friends` table is intentionally kept untouched: it stores
-- one-way contacts and V2 must not treat them as mutually accepted friends.

CREATE TABLE IF NOT EXISTS social_friend_requests (
    id BIGSERIAL PRIMARY KEY,
    sender_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    recipient_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    message VARCHAR(200) NOT NULL DEFAULT '',
    status VARCHAR(16) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'accepted', 'rejected', 'cancelled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (sender_id <> recipient_id)
);

CREATE UNIQUE INDEX IF NOT EXISTS social_friend_requests_pending_pair_idx
ON social_friend_requests (sender_id, recipient_id)
WHERE status = 'pending';

CREATE INDEX IF NOT EXISTS social_friend_requests_incoming_idx
ON social_friend_requests (recipient_id, status, created_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS social_friend_requests_outgoing_idx
ON social_friend_requests (sender_id, status, created_at DESC, id DESC);

CREATE TABLE IF NOT EXISTS social_friendships (
    user_low INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    user_high INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_low, user_high),
    CHECK (user_low < user_high)
);

CREATE INDEX IF NOT EXISTS social_friendships_high_idx
ON social_friendships (user_high, created_at DESC);

CREATE TABLE IF NOT EXISTS social_blocks (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    blocked_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, blocked_id),
    CHECK (user_id <> blocked_id)
);

CREATE INDEX IF NOT EXISTS social_blocks_blocked_idx
ON social_blocks (blocked_id, created_at DESC);

CREATE TABLE IF NOT EXISTS social_preferences (
    user_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    message_policy VARCHAR(16) NOT NULL DEFAULT 'everyone'
        CHECK (message_policy IN ('everyone', 'friends', 'nobody')),
    friend_request_policy VARCHAR(16) NOT NULL DEFAULT 'everyone'
        CHECK (friend_request_policy IN ('everyone', 'nobody')),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Clearing a conversation hides messages up to the recorded message id for
-- one participant only. The underlying messages are never deleted.
CREATE TABLE IF NOT EXISTS social_message_clears (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    partner_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    cleared_through_id BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, partner_id),
    CHECK (user_id <> partner_id)
);

-- Query indexes for the existing forum endpoints. These are additive and do
-- not change any response shape.
CREATE INDEX IF NOT EXISTS forum_posts_category_created_idx
ON forum_posts (category, created_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS forum_posts_author_created_idx
ON forum_posts (author_id, created_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS forum_comments_post_created_idx
ON forum_comments (post_id, created_at ASC, id ASC);

CREATE INDEX IF NOT EXISTS forum_messages_conversation_idx
ON forum_messages (sender_id, recipient_id, created_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS forum_messages_recipient_unread_idx
ON forum_messages (recipient_id, is_read, created_at DESC);
