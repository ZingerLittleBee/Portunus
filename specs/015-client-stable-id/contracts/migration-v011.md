# Contract: `V011` migration (re-key client tables to `client_id`)

File: `crates/portunus-server/src/store/migrations/V011__client_id.sql` (+ a Rust-side ULID
assignment step where SQL alone cannot mint ULIDs — see research R-003).

## Preconditions

- DB at refinery head `V010`.
- `client_tokens` is the authoritative client roster.

## Steps (single refinery transaction)

1. **Mint ids**: assign a fresh ULID `client_id` to every existing `client_tokens` row.
   (ULIDs minted in Rust; SQLite has no ULID function.)
2. **Rebuild `client_tokens`** with `client_id TEXT PRIMARY KEY`, keeping `client_name` as a
   normal (non-unique) display column; copy rows; swap.
3. **For each dependent table** (`rules`, `rate_limit_owner`, `traffic_quotas`,
   `traffic_usage_minute`, `traffic_usage_hour`, `client_enrollments`):
   - create the new-shape table with a `client_id` column in the PK/FK,
   - `INSERT … SELECT … JOIN client_tokens USING (client_name)` to backfill `client_id`,
   - drop old, rename new,
   - recreate indexes (e.g. `rules_client_idx(client_id, listen_port)`).
4. Drop any `UNIQUE`/PK constraint that referenced `client_name`.

## Post-conditions (test assertions)

- **SC-004**: every former client has a `client_id`; `COUNT` of each dependent table is
  preserved (zero orphans — every dependent row's `client_name` matched a `client_tokens`
  row). A row whose `client_name` has no token row is a data-integrity error surfaced by the
  migration, not silently dropped.
- **SC-006**: running the refinery runner a second time is a no-op (refinery version gate);
  the test invokes the runner twice on the same DB and asserts no error and unchanged data.
- No table retains a `client_name`-based PK or UNIQUE constraint.
- A seeded client with rules + quota + minute/hour usage resolves entirely by its new
  `client_id` after migration.

## Test fixtures

A `V010`-shaped seed database with ≥2 clients, each having: ≥1 rule, ≥1 owner rate-limit,
≥1 traffic quota, and ≥1 minute + ≥1 hour usage row; plus ≥1 enrollment row. Drive the
migration and assert the post-conditions above.
