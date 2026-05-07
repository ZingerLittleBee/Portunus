/// Shared TypeScript shapes for the operator HTTP API.
/// Mirrors the serde-Serialize structs in `crates/forward-server/src/`
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
}

export interface PushRuleBody {
  client: string;
  listen_port: number;
  listen_port_end?: number;
  target_host: string;
  target_port: number;
  target_port_end?: number;
  protocol?: "tcp" | "udp";
  prefer_ipv6?: boolean;
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
    if (raw === "Pending" || raw === "Active" || raw === "Removed") return { kind: raw };
  }
  if (raw && typeof raw === "object" && "Failed" in raw) {
    const f = (raw as { Failed: { reason?: string } }).Failed;
    return { kind: "Failed", reason: f?.reason ?? "" };
  }
  return { kind: "Pending" };
}
