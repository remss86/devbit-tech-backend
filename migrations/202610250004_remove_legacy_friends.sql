-- Remove the legacy one-way contact model.
--
-- Before deploying this migration, take a backup as well:
--
--   pg_dump "$DATABASE_URL" -t friends -f friends_backup.sql
--
-- As a second safety net the migration copies the rows into
-- `friends_legacy_archive` inside the same database. Verify the deployment and
-- then drop that archive manually if the data is no longer needed.

DO $$
BEGIN
    IF to_regclass('public.friends') IS NOT NULL THEN
        CREATE TABLE IF NOT EXISTS friends_legacy_archive AS
        SELECT * FROM friends;
        DROP TABLE friends;
    END IF;
END
$$;
