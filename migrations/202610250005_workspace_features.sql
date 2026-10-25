-- Workspace extensions required by the Nuxt social workspace, plus nested
-- forum replies, reposts and account recovery.
--
-- All new tables are additive. Existing rows keep their current meaning.

-- ── Nested comments ──────────────────────────────────────────────────────────
-- Deleting a comment removes its replies in the same statement. Existing
-- comments have no parent and remain top-level.
ALTER TABLE forum_comments
ADD COLUMN IF NOT EXISTS parent_id INTEGER REFERENCES forum_comments(id) ON DELETE CASCADE;

CREATE INDEX IF NOT EXISTS forum_comments_parent_idx
ON forum_comments (parent_id, created_at ASC, id ASC);

-- ── Reposts ──────────────────────────────────────────────────────────────────
-- A repost is a lightweight share: one row per (post, user). The original post
-- is never duplicated, so its comments and likes stay in one place.
CREATE TABLE IF NOT EXISTS forum_reposts (
    post_id INTEGER NOT NULL REFERENCES forum_posts(id) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (post_id, user_id)
);

CREATE INDEX IF NOT EXISTS forum_reposts_user_created_idx
ON forum_reposts (user_id, created_at DESC, post_id DESC);

-- ── Private friend metadata and groups ───────────────────────────────────────
CREATE TABLE IF NOT EXISTS social_friend_groups (
    id BIGSERIAL PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name VARCHAR(30) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (user_id > 0),
    CHECK (char_length(btrim(name)) BETWEEN 1 AND 30)
);

CREATE UNIQUE INDEX IF NOT EXISTS social_friend_groups_owner_name_idx
ON social_friend_groups (user_id, LOWER(name));

CREATE TABLE IF NOT EXISTS social_friend_metadata (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    friend_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    note VARCHAR(60) NOT NULL DEFAULT '',
    starred BOOLEAN NOT NULL DEFAULT FALSE,
    group_id BIGINT REFERENCES social_friend_groups(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, friend_id),
    CHECK (user_id <> friend_id)
);

CREATE INDEX IF NOT EXISTS social_friend_metadata_owner_starred_idx
ON social_friend_metadata (user_id, starred, friend_id);

-- ── Per-account conversation preferences ─────────────────────────────────────
CREATE TABLE IF NOT EXISTS social_conversation_preferences (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    partner_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    pinned BOOLEAN NOT NULL DEFAULT FALSE,
    muted BOOLEAN NOT NULL DEFAULT FALSE,
    archived BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, partner_id),
    CHECK (user_id <> partner_id)
);

-- ── Idempotent message sends ─────────────────────────────────────────────────
ALTER TABLE forum_messages
ADD COLUMN IF NOT EXISTS client_message_id UUID;

CREATE UNIQUE INDEX IF NOT EXISTS forum_messages_sender_client_idx
ON forum_messages (sender_id, client_message_id)
WHERE client_message_id IS NOT NULL;

-- ── Password reset codes ─────────────────────────────────────────────────────
-- Kept separate from registration codes so a pending registration cannot be
-- consumed by a password reset (or vice versa).
CREATE TABLE IF NOT EXISTS password_reset_codes (
    email TEXT PRIMARY KEY,
    code VARCHAR(6) NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS password_reset_codes_expiry_idx
ON password_reset_codes (expires_at);
