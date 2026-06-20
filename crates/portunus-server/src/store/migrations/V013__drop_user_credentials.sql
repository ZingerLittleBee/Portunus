-- v3.0 remove-user-api-token: delete all user self-issued operator
-- credentials. Preserve the reserved bootstrap credentials only:
--   `_legacy`     -> operator_token (server.toml) shortcut
--   `_superadmin` -> bootstrap-superadmin CLI
-- The reserved namespace is closed: UserId::reserved is the only
-- constructor of '_'-prefixed ids and FromStr rejects operator-supplied
-- ones, so this predicate can never delete a bootstrap credential and is
-- future-proof against any new reserved id. Idempotent (plain DELETE) and
-- crash-safe inside refinery's transactional version gate.
DELETE FROM credentials WHERE user_id NOT LIKE '\_%' ESCAPE '\';
