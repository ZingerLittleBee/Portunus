import { describe, expect, test } from "vitest";

import { parseRuleState } from "@/api/types";

describe("parseRuleState", () => {
  test("accepts lowercase states from the operator API", () => {
    expect(parseRuleState("active")).toEqual({ kind: "Active" });
    expect(parseRuleState("pending")).toEqual({ kind: "Pending" });
    expect(parseRuleState("removed")).toEqual({ kind: "Removed" });
  });

  test("accepts lowercase failed payloads from the operator API", () => {
    expect(parseRuleState({ failed: { reason: "boom" } })).toEqual({
      kind: "Failed",
      reason: "boom",
    });
  });

  test("accepts object states with lowercase kind from the operator API", () => {
    expect(parseRuleState({ kind: "active" })).toEqual({ kind: "Active" });
    expect(parseRuleState({ kind: "pending" })).toEqual({ kind: "Pending" });
    expect(parseRuleState({ kind: "removed" })).toEqual({ kind: "Removed" });
    expect(parseRuleState({ kind: "failed", reason: "boom" })).toEqual({
      kind: "Failed",
      reason: "boom",
    });
  });
});
