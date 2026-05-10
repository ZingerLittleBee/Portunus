-- 009-tls-sni-routing T013: add the optional SNI selector to TCP single-port rules.
--
-- Per `specs/009-tls-sni-routing/data-model.md` §1.2 and research.md R-003:
-- the column is purely additive. NO `UNIQUE` constraint is added because
-- the `rules` table has no `client_name` column today; per-(client,
-- listen_port) uniqueness is enforced authoritatively by ServerRuleStore
-- in memory. A global UNIQUE per port would forbid two clients from
-- each owning `:443 + api.example.com` (FR-021), which is a legitimate
-- multi-tenant pattern.
--
-- The helper index is partial (only non-NULL `sni_pattern`) so the
-- legacy "scan rules by listen_port" plan is unaffected.

ALTER TABLE rules ADD COLUMN sni_pattern TEXT;

CREATE INDEX rules_sni_lookup
    ON rules (listen_port, sni_pattern)
    WHERE sni_pattern IS NOT NULL;
