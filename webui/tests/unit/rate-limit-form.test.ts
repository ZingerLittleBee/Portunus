/// 011-rate-limiting-qos T036 (helpers): unit-test the pure helpers
/// that bridge the form state to the wire shape and back, plus the
/// rules-table compact summary rendering.

import { describe, expect, it } from "vitest";

import {
  EMPTY_RATE_LIMIT_FORM,
  formStateToRateLimit,
  rateLimitToFormState,
  summarizeRateLimit,
} from "@/components/RateLimitForm.helpers";

describe("formStateToRateLimit", () => {
  it("returns undefined when every field is empty", () => {
    expect(formStateToRateLimit(EMPTY_RATE_LIMIT_FORM)).toBeUndefined();
  });

  it("strips empty strings and emits only set caps", () => {
    const out = formStateToRateLimit({
      ...EMPTY_RATE_LIMIT_FORM,
      bandwidth_in_bps: "1048576",
      concurrent_connections: "100",
    });
    expect(out).toEqual({ bandwidth_in_bps: 1048576, concurrent_connections: 100 });
  });

  it("includes burst overrides when set", () => {
    const out = formStateToRateLimit({
      ...EMPTY_RATE_LIMIT_FORM,
      bandwidth_in_bps: "1000000",
      bandwidth_in_burst: "2000000",
    });
    expect(out).toEqual({ bandwidth_in_bps: 1000000, bandwidth_in_burst: 2000000 });
  });

  it("ignores non-numeric input by treating it as undefined", () => {
    const out = formStateToRateLimit({
      ...EMPTY_RATE_LIMIT_FORM,
      bandwidth_in_bps: "garbage",
      concurrent_connections: "10",
    });
    expect(out).toEqual({ concurrent_connections: 10 });
  });
});

describe("rateLimitToFormState", () => {
  it("returns the empty shape for null/undefined", () => {
    expect(rateLimitToFormState(null)).toEqual(EMPTY_RATE_LIMIT_FORM);
    expect(rateLimitToFormState(undefined)).toEqual(EMPTY_RATE_LIMIT_FORM);
  });

  it("hydrates set fields and leaves unset ones as empty strings", () => {
    expect(
      rateLimitToFormState({
        bandwidth_in_bps: 1024,
        concurrent_connections: 5,
      }),
    ).toEqual({
      ...EMPTY_RATE_LIMIT_FORM,
      bandwidth_in_bps: "1024",
      concurrent_connections: "5",
    });
  });

  it("round-trips through formStateToRateLimit", () => {
    const original = {
      bandwidth_in_bps: 100000,
      bandwidth_out_bps: 200000,
      new_connections_per_sec: 50,
      concurrent_connections: 100,
    };
    expect(formStateToRateLimit(rateLimitToFormState(original))).toEqual(original);
  });
});

describe("summarizeRateLimit", () => {
  it("returns null for null/undefined", () => {
    expect(summarizeRateLimit(null)).toBeNull();
    expect(summarizeRateLimit(undefined)).toBeNull();
  });

  it("formats bandwidth with K/M suffix", () => {
    expect(summarizeRateLimit({ bandwidth_in_bps: 500 })).toBe("↓500");
    expect(summarizeRateLimit({ bandwidth_in_bps: 2048 })).toBe("↓2K");
    expect(summarizeRateLimit({ bandwidth_out_bps: 5 * 1024 * 1024 })).toBe("↑5.0M");
  });

  it("renders all four cap dimensions concisely", () => {
    expect(
      summarizeRateLimit({
        bandwidth_in_bps: 1024 * 1024,
        bandwidth_out_bps: 1024 * 1024,
        new_connections_per_sec: 50,
        concurrent_connections: 100,
      }),
    ).toBe("↓1.0M · ↑1.0M · 50/s · ≤100");
  });

  it("returns null when the envelope is empty", () => {
    expect(summarizeRateLimit({})).toBeNull();
  });
});
