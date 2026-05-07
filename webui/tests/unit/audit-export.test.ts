import { describe, expect, it } from "vitest";

import { toNdjson, toNdjsonBlob } from "@/lib/ndjson";
import type { AuditEntry } from "@/api/types";

const rows: AuditEntry[] = [
  {
    timestamp: "2026-05-08T10:00:00Z",
    actor: "alice",
    role: "user",
    method: "POST",
    path: "/v1/rules",
    outcome: "deny",
    reason: "port_outside_grant",
  },
  {
    timestamp: "2026-05-08T10:00:01Z",
    actor: "_legacy",
    role: "superadmin",
    method: "GET",
    path: "/v1/users",
    outcome: "allow",
  },
];

describe("ndjson export", () => {
  it("emits one JSON object per line with a trailing newline", () => {
    const text = toNdjson(rows);
    const lines = text.split("\n");
    expect(lines).toHaveLength(3); // 2 entries + trailing empty after final \n
    expect(lines[2]).toBe("");
    for (let i = 0; i < rows.length; i++) {
      const parsed = JSON.parse(lines[i]!);
      expect(parsed.actor).toBe(rows[i]!.actor);
      expect(parsed.outcome).toBe(rows[i]!.outcome);
    }
  });

  it("returns an empty string for zero rows (no leading newline)", () => {
    expect(toNdjson<AuditEntry>([])).toBe("");
  });

  it("blob carries the right MIME type", () => {
    const blob = toNdjsonBlob(rows);
    expect(blob.type).toBe("application/x-ndjson");
  });
});
