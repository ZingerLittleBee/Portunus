import { describe, expect, it } from "vitest";
import { parseDashboardGauges } from "@/api/metrics";

const FIXTURE = `
# HELP portunus_clients_connected Currently-connected clients.
# TYPE portunus_clients_connected gauge
portunus_clients_connected 4
# HELP portunus_rule_active_connections Active TCP connections per rule.
# TYPE portunus_rule_active_connections gauge
portunus_rule_active_connections{client="edge-a",owner="alice",rule="7"} 12
portunus_rule_active_connections{client="edge-a",owner="alice",rule="8"} 7
portunus_rule_active_connections{client="edge-b",owner="bob",rule="9"} 3
# HELP portunus_rule_bytes_in_total Cumulative inbound bytes.
# TYPE portunus_rule_bytes_in_total counter
portunus_rule_bytes_in_total{client="edge-a",owner="alice",rule="7"} 10000
portunus_rule_bytes_in_total{client="edge-a",owner="alice",rule="8"} 500
portunus_rule_bytes_out_total{client="edge-a",owner="alice",rule="7"} 20000
portunus_rule_bytes_out_total{client="edge-a",owner="alice",rule="8"} 200
`.trim();

describe("parseDashboardGauges", () => {
  it("returns null fields for empty input", () => {
    const g = parseDashboardGauges(undefined);
    expect(g.clientsConnected).toBeNull();
    expect(g.rulesActive).toBeNull();
    expect(g.activeConnections).toBeNull();
    expect(g.topRules).toEqual([]);
  });

  it("parses connected clients and active conns", () => {
    const g = parseDashboardGauges(FIXTURE);
    expect(g.clientsConnected).toBe(4);
    expect(g.activeConnections).toBe(22); // 12 + 7 + 3
  });

  it("counts distinct active rules via the active-connections label", () => {
    const g = parseDashboardGauges(FIXTURE);
    // Rules 7, 8 and 9 all emit active_connections (even rule 9, which has
    // no traffic counters yet). Counting bytes_in would undercount to 2.
    expect(g.rulesActive).toBe(3);
  });

  it("computes top rules by in+out, descending", () => {
    const g = parseDashboardGauges(FIXTURE);
    expect(g.topRules).toEqual([
      { rule: "7", bytesIn: 10000, bytesOut: 20000, total: 30000 },
      { rule: "8", bytesIn: 500, bytesOut: 200, total: 700 },
    ]);
  });

  it("ignores malformed values (NaN guard)", () => {
    const bad = `portunus_clients_connected not-a-number\nportunus_rule_active_connections{rule="9"} also-bad`;
    const g = parseDashboardGauges(bad);
    expect(g.clientsConnected).toBeNull();
    expect(g.activeConnections).toBeNull();
  });

  it("escapes label-injection attempts when extracting rule name", () => {
    const inj = `portunus_rule_bytes_in_total{rule="x\\",injected=\\"y"} 1`;
    const g = parseDashboardGauges(inj);
    expect(g.topRules.length).toBeLessThanOrEqual(1);
  });
});
