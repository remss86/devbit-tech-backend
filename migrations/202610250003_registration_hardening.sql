-- Registration verification hardening: track when a code was issued so a
-- per-email cooldown can be enforced, and count failed attempts so a six-digit
-- code cannot be brute-forced indefinitely.

ALTER TABLE verify_code
ADD COLUMN IF NOT EXISTS created_at TIMESTAMPTZ NOT NULL DEFAULT NOW();

ALTER TABLE verify_code
ADD COLUMN IF NOT EXISTS attempts INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS verify_code_created_idx
ON verify_code (created_at);
