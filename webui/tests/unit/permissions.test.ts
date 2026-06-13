import { describe, expect, it } from "vitest";

import {
  canManageGrants,
  canProvisionClient,
  canSeeAuditLog,
  canSeeMetrics,
  canSeeRule,
  canSeeUserDetail,
  canSeeUsersList,
  isSuperadmin,
  type Identity,
} from "@/lib/permissions";

const superadmin: Identity = { user_id: "_legacy", role: "superadmin", display_name: "Operator" };
const alice: Identity = { user_id: "alice", role: "user", display_name: "Alice" };
const bob: Identity = { user_id: "bob", role: "user", display_name: "Bob" };

describe("permissions", () => {
  it("isSuperadmin recognises superadmin only", () => {
    expect(isSuperadmin(superadmin)).toBe(true);
    expect(isSuperadmin(alice)).toBe(false);
    expect(isSuperadmin(null)).toBe(false);
    expect(isSuperadmin(undefined)).toBe(false);
  });

  it("admin-only gates require superadmin", () => {
    for (const gate of [canSeeUsersList, canSeeAuditLog, canManageGrants, canProvisionClient]) {
      expect(gate(superadmin)).toBe(true);
      expect(gate(alice)).toBe(false);
      expect(gate(null)).toBe(false);
    }
  });

  it("metrics gate is superadmin-only (mirrors the server's 403)", () => {
    expect(canSeeMetrics(superadmin)).toBe(true);
    expect(canSeeMetrics(alice)).toBe(false);
    expect(canSeeMetrics(null)).toBe(false);
  });

  it("user-detail gate allows self + superadmin only", () => {
    expect(canSeeUserDetail(superadmin, "alice")).toBe(true);
    expect(canSeeUserDetail(superadmin, "bob")).toBe(true);
    expect(canSeeUserDetail(alice, "alice")).toBe(true);
    expect(canSeeUserDetail(alice, "bob")).toBe(false);
    expect(canSeeUserDetail(bob, "alice")).toBe(false);
    expect(canSeeUserDetail(null, "alice")).toBe(false);
  });

  it("rule gate allows owner + superadmin only", () => {
    expect(canSeeRule(superadmin, "alice")).toBe(true);
    expect(canSeeRule(alice, "alice")).toBe(true);
    expect(canSeeRule(alice, "bob")).toBe(false);
    expect(canSeeRule(bob, "alice")).toBe(false);
    expect(canSeeRule(null, "alice")).toBe(false);
  });
});
