// webui/src/api/access-entries.test.ts
import { describe, expect, it } from "vitest";
import { joinAccessEntries } from "./access-entries";
import type {
  GrantView,
  MonthlyQuotaView,
  OwnerRateLimitView,
} from "@/api/types";

const g = (overrides: Partial<GrantView> = {}): GrantView => ({
  grant_id: "g1",
  user_id: "alice",
  client: "edge-tokyo",
  listen_port_start: 1000,
  listen_port_end: 2000,
  protocols: ["tcp"],
  note: null,
  created_at: "2026-01-01T00:00:00Z",
  ...overrides,
});

const cap = (owner: string, client: string): OwnerRateLimitView => ({
  client_name: client,
  owner_id: owner,
  rate_limit: { bandwidth_in_bps: 1_000_000 },
  updated_at_unix_ms: 0,
});

describe("joinAccessEntries", () => {
  it("returns empty when no grants", () => {
    expect(joinAccessEntries([], [])).toEqual([]);
  });

  it("creates one entry per (user, client) with cap", () => {
    const res = joinAccessEntries([g()], [cap("alice", "edge-tokyo")]);
    expect(res).toHaveLength(1);
    expect(res[0]!.grant_id).toBe("g1");
    expect(res[0]!.unlimited).toBe(false);
    expect(res[0]!.cap?.bandwidth_in_bps).toBe(1_000_000);
  });

  it("marks unlimited when grant has no cap", () => {
    const res = joinAccessEntries([g()], []);
    expect(res[0]!.unlimited).toBe(true);
    expect(res[0]!.cap).toBeUndefined();
  });

  it("flags duplicates when same (user, client) has 2 grants", () => {
    const grants = [
      g({ grant_id: "g1", listen_port_start: 1000, listen_port_end: 2000 }),
      g({ grant_id: "g2", listen_port_start: 3000, listen_port_end: 9000 }),
    ];
    const res = joinAccessEntries(grants, []);
    expect(res).toHaveLength(1);
    expect(res[0]!.grant_id).toBe("g2"); // widest range wins
    expect(res[0]!.legacy_duplicates).toHaveLength(1);
    expect(res[0]!.legacy_duplicates![0]!.grant_id).toBe("g1");
  });

  it("keeps separate entries when same user different clients", () => {
    const grants = [g({ client: "edge-tokyo" }), g({ grant_id: "g2", client: "edge-sg" })];
    const res = joinAccessEntries(grants, []);
    expect(res).toHaveLength(2);
  });

  it("attaches quota to matching (user, client) pair", () => {
    const grants = [g()];
    const quota: MonthlyQuotaView = {
      user_id: "alice",
      client_name: "edge-tokyo",
      monthly_bytes: 1_000_000_000,
      billing_anchor: 1_700_000_000,
      current_period_started_at: 1_700_000_000,
      current_period_ends_at: 1_702_678_400,
      current_period_bytes_used: 250_000_000,
      budget_remaining_bytes: 750_000_000,
      exhausted_at: null,
      exhausted: false,
      created_at: 1_700_000_000,
      updated_at: 1_700_000_000,
    };
    const res = joinAccessEntries(grants, [], [quota]);
    expect(res[0]!.quota?.monthly_bytes).toBe(1_000_000_000);
    expect(res[0]!.quota?.budget_remaining_bytes).toBe(750_000_000);
  });

  it("leaves quota undefined when no row matches the pair", () => {
    const grants = [g()];
    const res = joinAccessEntries(grants, [], []);
    expect(res[0]!.quota).toBeUndefined();
  });
});
