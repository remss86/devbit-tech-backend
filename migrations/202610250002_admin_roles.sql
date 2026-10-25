-- Replace the implicit "first three user ids are administrators" rule with an
-- explicit database flag.
--
-- The migration deliberately does NOT promote anyone automatically: a public
-- registration flow must never turn the first registrants into administrators.
-- After auditing existing accounts, grant the intended accounts explicitly:
--
--   UPDATE users SET is_admin = TRUE WHERE id IN (...);
--
-- then restart the backend so it reloads the administrator set.

ALTER TABLE users
ADD COLUMN IF NOT EXISTS is_admin BOOLEAN NOT NULL DEFAULT FALSE;

CREATE INDEX IF NOT EXISTS users_is_admin_idx
ON users (id)
WHERE is_admin;
