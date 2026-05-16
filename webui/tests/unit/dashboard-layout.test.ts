import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

function source(path: string) {
  return readFileSync(resolve(__dirname, "../../", path), "utf8");
}

describe("dashboard layout", () => {
  it("removes recent activity from the superadmin dashboard and merges issue metrics", () => {
    const dashboard = source("src/pages/dashboard/SuperadminDashboard.tsx");
    const issueBlocks = source("src/pages/dashboard/components/DashboardIssueBlocks.tsx");

    expect(dashboard).not.toContain("RecentAuditPanel");
    expect(dashboard).toContain("<DashboardIssueBlocks");
    expect(issueBlocks.match(/<Card>/g)).toHaveLength(1);
    expect(issueBlocks).toContain("dashboard.operationalStatus");
    expect(issueBlocks).toContain("dashboard.unhealthyTargets");
    expect(issueBlocks).toContain("dashboard.offlineClients");
  });
});
