-- Store the operator-declared client entry address. This is the public
-- IP or DNS name users should connect to when consuming forwarding rules.
-- It is intentionally distinct from the observed gRPC peer address, which
-- can be an internal proxy/NAT address on managed platforms.

ALTER TABLE client_tokens
ADD COLUMN client_address TEXT;
