/// Shared TypeScript shapes for the operator HTTP API.
/// Mirrors the serde-Serialize structs in `crates/portunus-server/src/`
/// — keep in sync when the server contract changes.

import type { Identity, Role } from "@/lib/permissions";

export type { Identity, Role };

// -----------------------------------------------------------------------------
// /v1/users
// -----------------------------------------------------------------------------

export interface UserView {
  user_id: string;
  display_name: string;
  role: "superadmin" | "user";
  disabled: boolean;
  created_at: string; // ISO-8601
  credential_count: number;
  grant_count: number;
}

export interface CreateUserBody {
  user_id: string;
  display_name: string;
  role?: "superadmin" | "user";
  initial_password?: string;
  password_change_required?: boolean;
}

export interface CreateUserResponse {
  user_id: string;
  display_name: string;
  role: "superadmin" | "user";
}

export interface DeleteUserResponse {
  user_id: string;
  removed_credential_ids: string[];
  revoked_grant_ids: string[];
  removed_rule_ids?: number[];
}

// -----------------------------------------------------------------------------
// /v1/users/{id}/credentials
// -----------------------------------------------------------------------------

export interface CredentialView {
  credential_id: string;
  user_id: string;
  label: string | null;
  created_at: string;
  last_used_at: string | null;
  status: "active" | "revoked";
  revoked_at: string | null;
}

export interface IssueCredentialBody {
  label?: string;
}

export interface IssueCredentialResponse {
  credential_id: string;
  user_id: string;
  /// Raw bearer — shown ONCE in `<TokenRevealModal>`, never persisted.
  token: string;
  label: string | null;
  created_at: string;
}

// -----------------------------------------------------------------------------
// /v1/grants
// -----------------------------------------------------------------------------

export interface GrantView {
  grant_id: string;
  user_id: string;
  client: string; // "*" or a client name
  listen_port_start: number;
  listen_port_end: number;
  protocols: ("tcp" | "udp")[];
  note: string | null;
  created_at: string;
}

export interface CreateGrantBody {
  user_id: string;
  client: string;
  listen_port_start: number;
  listen_port_end: number;
  protocols: ("tcp" | "udp")[];
  note?: string;
}

export interface DeleteGrantResponse {
  grant_id: string;
  removed_rule_ids?: number[];
}

// -----------------------------------------------------------------------------
// /v1/rules
// -----------------------------------------------------------------------------

export type Protocol = "tcp" | "udp" | "unspecified";

export type RuleState =
  | { kind: "Pending" }
  | { kind: "Active" }
  | { kind: "Failed"; reason: string }
  | { kind: "Removed" };

/// On-the-wire shape — `state` is `"Pending" | "Active" | "Removed" |
/// { Failed: { reason: string } }`. Adapt with `parseRuleState`.
export interface Rule {
  id: number;
  client_name: string;
  listen_port: number;
  listen_port_end?: number;
  target_host: string;
  target_port: number;
  target_port_end?: number;
  prefer_ipv6: boolean;
  protocol: Protocol;
  state: unknown;
  created_at: string;
  last_state_change_at: string;
  owner_user_id: string;
  /// 007-multi-target-failover: when present, the rule has ≥1 target.
  /// `GET /v1/rules` augments each rule with this synthesised array
  /// (single-target rules get a one-element list mirroring
  /// `target_host`/`target_port`). `health` is `null` until the stats
  /// cache observes the rule.
  targets?: TargetWithHealth[];
  /// 007-multi-target-failover: opt-in active TCP-connect probe cadence.
  /// `null` / absent means passive-only health tracking.
  health_check_interval_secs?: number | null;
  /// 009-tls-sni-routing: optional TLS Server Name Indication selector.
  /// Exact host (`api.example.com`) or single-label wildcard
  /// (`*.example.com`). Present only on TCP single-port rules; `null`
  /// or absent for legacy plain-TCP rules and the SNI fallback shape.
  sni_pattern?: string | null;
  /// 011-rate-limiting-qos: optional per-rule QoS envelope. Absent on
  /// pre-0.11 rules and on rules created without caps. Each cap field
  /// is independently optional — omitted = uncapped on that dimension.
  rate_limit?: RateLimit | null;
}

/// 011-rate-limiting-qos: per-rule and per-owner QoS envelope. All
/// fields independently optional. Caps must be `> 0` when present.
/// Burst overrides default to `1× rate`; `concurrent_connections_burst`
/// is reserved (server rejects when non-null).
export interface RateLimit {
  bandwidth_in_bps?: number | null;
  bandwidth_out_bps?: number | null;
  new_connections_per_sec?: number | null;
  concurrent_connections?: number | null;
  bandwidth_in_burst?: number | null;
  bandwidth_out_burst?: number | null;
  new_connections_burst?: number | null;
}

/// 007-multi-target-failover T044: a single target on a rule. Mirrors
/// `RuleTarget` server-side. `priority` defaults to row index (0 =
/// highest priority).
export interface Target {
  host: string;
  port: number;
  priority: number;
  proxy_protocol?: "v1" | "v2" | null;
}

/// 007-multi-target-failover T044: live per-target health snapshot
/// from the stats cache. Mirrors the `health` slot on
/// `rule_with_health` server-side. `null` on the parent target means
/// the cache has no snapshot yet (rule just pushed, no
/// `StatsReport` observed).
export interface TargetHealth {
  healthy: boolean;
  consecutive_failures: number;
  last_failure_at_unix_ms: number;
  last_success_at_unix_ms: number;
}

/// 007-multi-target-failover T044: union of `Target` + optional
/// `TargetHealth` as returned by `GET /v1/rules` and `GET /v1/rules/{id}`.
export interface TargetWithHealth extends Target {
  health: TargetHealth | null;
}

/// 007-multi-target-failover T044: per-target stats from
/// `RuleStatsSnapshot.per_target` (only present for multi-target rules
/// when `?per_target=true` is set on the stats endpoint or stream).
export interface PerTargetStats {
  index: number;
  host: string;
  port: number;
  priority: number;
  /// 0 = Healthy, 1 = Failed (mirrors proto wire encoding).
  health: number;
  consecutive_failures: number;
  last_failure_at_unix_ms: number;
  last_success_at_unix_ms: number;
  bytes_in: number;
  bytes_out: number;
  connections_accepted: number;
}

export interface PushRuleBody {
  client: string;
  listen_port: number;
  listen_port_end?: number;
  /// Legacy single-target shape. Mutually exclusive with `targets[]`.
  target_host?: string;
  target_port?: number;
  target_port_end?: number;
  protocol?: "tcp" | "udp";
  prefer_ipv6?: boolean;
  /// 007-multi-target-failover T044: new multi-target shape. Mutually
  /// exclusive with `target_host`/`target_port`. Length ≥ 1, ≤ 8.
  targets?: Target[];
  /// 007-multi-target-failover T044: opt-in active probe cadence in
  /// seconds (range 1..=3600). Omit or `null` for passive-only health
  /// tracking (default).
  health_check_interval_secs?: number;
  /// 009-tls-sni-routing: optional SNI selector. Server-side validation
  /// rejects this field on UDP rules, port-range rules, and grammar
  /// violations. Omit (or pass empty) for the legacy / fallback shape.
  sni_pattern?: string;
  /// 011-rate-limiting-qos: optional per-rule QoS caps. Server-side
  /// validation rejects: any cap = 0, burst-without-rate, burst out
  /// of [rate, 4×rate] range, or `concurrent_connections_burst` set.
  /// Capability gate: pre-0.11 client → 422
  /// `rate_limit_unsupported_by_client` before the rule activates.
  rate_limit?: RateLimit;
}

/// 011-rate-limiting-qos: per-owner cap envelope returned by
/// `GET /v1/clients/{id}/owners/{owner_id}/rate-limit`.
export interface OwnerRateLimitView {
  client_name: string;
  owner_id: string;
  rate_limit: RateLimit;
  updated_at_unix_ms: number;
}

/// 011-rate-limiting-qos: row in `GET /v1/clients/{id}/owners`. Used
/// by the Web UI to populate the Owner quotas tab.
export interface OwnerListEntry {
  owner_id: string;
  rule_count: number;
  has_rate_limit: boolean;
}

export interface PushRuleResponse {
  rule_id: number;
  status: string;
  target_host: string;
  prefer_ipv6: boolean;
  protocol: "tcp" | "udp";
  owner: string;
}

// -----------------------------------------------------------------------------
// /v1/rules/{id}/stats and /v1/rules/{id}/stats/stream
// -----------------------------------------------------------------------------

export interface RuleStatsSnapshot {
  rule_id: number;
  client_name: string;
  bytes_in: number;
  bytes_out: number;
  active_connections: number;
  dns_failures: number;
  datagrams_in: number;
  datagrams_out: number;
  active_flows: number;
  flows_dropped_overflow: number;
  updated_at: string;
  protocol?: "tcp" | "udp";
  /// 007-multi-target-failover T044: cumulative count of
  /// Healthy↔Failed transitions on multi-target rules. Always 0 on
  /// single-target rules (invariant I-3).
  target_failovers_total?: number;
  /// 007-multi-target-failover T044: per-target snapshots, only
  /// present when the request was made with `?per_target=true` AND
  /// the rule is multi-target. Empty/absent for single-target rules
  /// (server strips via `skip_serializing_if = "Vec::is_empty"`).
  per_target?: PerTargetStats[];
}

// -----------------------------------------------------------------------------
// /v1/clients
// -----------------------------------------------------------------------------

export interface ClientView {
  client_name: string;
  provisioned_at: string;
  revoked_at: string | null;
  connected: boolean;
  remote_addr: string | null;
  connected_at: string | null;
}

export interface ProvisionClientBody {
  name: string;
}

export interface CredentialBundle {
  client_name: string;
  bearer_token: string;
  server_endpoint: string;
  server_cert_sha256: string;
  server_cert_pem: string;
  issued_at: string;
}

// -----------------------------------------------------------------------------
// /v1/audit
// -----------------------------------------------------------------------------

export interface AuditEntry {
  timestamp: string;
  actor: string;
  role?: "superadmin" | "user";
  method: string;
  path: string;
  outcome: "allow" | "deny";
  reason?: string;
}

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

export function parseRuleState(raw: unknown): RuleState {
  if (typeof raw === "string") {
    const normalized = raw.toLowerCase();
    if (normalized === "pending") return { kind: "Pending" };
    if (normalized === "active") return { kind: "Active" };
    if (normalized === "removed") return { kind: "Removed" };
  }
  if (raw && typeof raw === "object") {
    if ("kind" in raw) {
      const kind = (raw as { kind?: string }).kind?.toLowerCase();
      if (kind === "pending") return { kind: "Pending" };
      if (kind === "active") return { kind: "Active" };
      if (kind === "removed") return { kind: "Removed" };
      if (kind === "failed") {
        const reason = (raw as { reason?: string }).reason ?? "";
        return { kind: "Failed", reason };
      }
    }
    if ("Failed" in raw) {
      const f = (raw as { Failed: { reason?: string } }).Failed;
      return { kind: "Failed", reason: f?.reason ?? "" };
    }
    if ("failed" in raw) {
      const f = (raw as { failed: { reason?: string } }).failed;
      return { kind: "Failed", reason: f?.reason ?? "" };
    }
  }
  return { kind: "Pending" };
}
