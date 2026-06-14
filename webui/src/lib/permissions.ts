/// Pure role-gate predicates consumed by `<AuthGate>`, the navigation,
/// and per-page guards. Mirror the role table from
/// `specs/006-management-web-ui/contracts/ui-routes.md`.

export type Role = "superadmin" | "user";

export interface Identity {
  user_id: string;
  role: Role;
  display_name: string;
}

export function isSuperadmin(identity: Identity | null | undefined): boolean {
  return identity?.role === "superadmin";
}

export function canSeeUsersList(identity: Identity | null | undefined): boolean {
  return isSuperadmin(identity);
}

export function canSeeAuditLog(identity: Identity | null | undefined): boolean {
  return isSuperadmin(identity);
}

export function canSeeMetrics(identity: Identity | null | undefined): boolean {
  // The server's `GET /v1/metrics` mirror is superadmin-only (returns 403
  // otherwise), and the tenant Dashboard already skips it — so the Metrics
  // page must be gated the same way, not left to render a "Forbidden" stub.
  return isSuperadmin(identity);
}

export function canSeeUserDetail(
  identity: Identity | null | undefined,
  targetUserId: string,
): boolean {
  if (!identity) return false;
  if (isSuperadmin(identity)) return true;
  return identity.user_id === targetUserId;
}

export function canSeeRule(
  identity: Identity | null | undefined,
  ruleOwnerUserId: string,
): boolean {
  if (!identity) return false;
  if (isSuperadmin(identity)) return true;
  return identity.user_id === ruleOwnerUserId;
}

export function canProvisionClient(identity: Identity | null | undefined): boolean {
  return isSuperadmin(identity);
}

export function canManageGrants(identity: Identity | null | undefined): boolean {
  return isSuperadmin(identity);
}
