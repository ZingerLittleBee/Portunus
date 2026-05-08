-- 009-tls-sni-routing: add the optional SNI selector to TCP single-port rules.
--
-- Body filled in T013. Phase 1 (T004) reserves the migration filename
-- so refinery picks it up at boot before Phase 2 lands the actual DDL.
--
-- NOTE: NO `UNIQUE` constraint here. The `rules` table has no
-- `client_name` column today; per-(client, listen_port) uniqueness is
-- enforced authoritatively by ServerRuleStore in memory. Adding
-- `client_name` is out of scope for v0.9. See research.md R-003.

-- Placeholder — replaced by T013. SQLite tolerates a no-op SELECT.
SELECT 1;
