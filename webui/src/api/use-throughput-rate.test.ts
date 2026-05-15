import { describe, expect, it } from "vitest";
import { computeRate } from "@/api/use-throughput-rate";

describe("computeRate", () => {
  it("returns null when there is no prior sample", () => {
    expect(computeRate(null, { totalBytes: 1000, ts: 1000 })).toBeNull();
  });

  it("returns positive bytes/sec for normal growth", () => {
    const prev = { totalBytes: 1_000, ts: 1_000 };
    const next = { totalBytes: 6_000, ts: 1_005 }; // +5000 in 5s
    expect(computeRate(prev, next)).toBe(1_000);
  });

  it("returns 0 when timestamps are identical (avoid div by zero)", () => {
    const prev = { totalBytes: 1, ts: 100 };
    const next = { totalBytes: 5, ts: 100 };
    expect(computeRate(prev, next)).toBe(0);
  });

  it("returns 0 on counter reset (negative delta)", () => {
    const prev = { totalBytes: 5_000, ts: 1_000 };
    const next = { totalBytes: 1_000, ts: 1_005 };
    expect(computeRate(prev, next)).toBe(0);
  });
});
