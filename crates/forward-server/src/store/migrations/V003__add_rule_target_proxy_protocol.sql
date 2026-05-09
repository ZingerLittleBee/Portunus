-- 010-proxy-protocol-and-peek-histogram: add optional per-target PROXY
-- protocol mode to the persisted rule target shape.
--
-- Additive only. NULL means legacy raw forwarding; non-NULL values are the
-- lowercase wire/API forms `v1` and `v2`.

ALTER TABLE rule_targets
    ADD COLUMN proxy_protocol TEXT
    CHECK (proxy_protocol IS NULL OR proxy_protocol IN ('v1', 'v2'));
