import { describe, expect, it } from "vitest";
import { formatBps, parseBpsInput, accessEntrySchema } from "./format";

describe("formatBps", () => {
  it("formats 0 as 0 bps", () => expect(formatBps(0)).toBe("0 bps"));
  it("formats 1500 as 1.5 KB/s", () => expect(formatBps(1500)).toBe("1.5 KB/s"));
  it("formats 12_500_000 as 12.5 MB/s", () => expect(formatBps(12_500_000)).toBe("12.5 MB/s"));
  it("formats 5_000_000_000 as 5.0 GB/s", () => expect(formatBps(5_000_000_000)).toBe("5.0 GB/s"));
  it("returns dash for negative", () => expect(formatBps(-1)).toBe("—"));
  it("returns dash for NaN", () => expect(formatBps(NaN)).toBe("—"));
});

describe("parseBpsInput", () => {
  it("parses '1.5 MB/s' to 1_500_000", () => expect(parseBpsInput("1.5 MB/s")).toBe(1_500_000));
  it("parses '100KB' to 100_000", () => expect(parseBpsInput("100KB")).toBe(100_000));
  it("parses bare number as raw bps", () => expect(parseBpsInput("42")).toBe(42));
  it("returns null on garbage", () => expect(parseBpsInput("abc")).toBeNull());
  it("returns null on empty", () => expect(parseBpsInput("")).toBeNull());
  it("rejects bare s suffix", () => expect(parseBpsInput("100s")).toBeNull());
  it("rejects bare Ms suffix", () => expect(parseBpsInput("1.5Ms")).toBeNull());
  it("accepts MB/s without space", () => expect(parseBpsInput("1.5MB/s")).toBe(1_500_000));
});

describe("accessEntrySchema", () => {
  it("requires at least one protocol", () => {
    const r = accessEntrySchema.safeParse({
      client_id: "c1",
      listen_port_start: 1000,
      listen_port_end: 2000,
      protocols: [],
      unlimited: true,
    });
    expect(r.success).toBe(false);
  });

  it("requires start <= end", () => {
    const r = accessEntrySchema.safeParse({
      client_id: "c1",
      listen_port_start: 2000,
      listen_port_end: 1000,
      protocols: ["tcp"],
      unlimited: true,
    });
    expect(r.success).toBe(false);
  });

  it("requires ports in 1..65535", () => {
    const r = accessEntrySchema.safeParse({
      client_id: "c1",
      listen_port_start: 0,
      listen_port_end: 100,
      protocols: ["tcp"],
      unlimited: true,
    });
    expect(r.success).toBe(false);
  });

  it("requires at least one cap > 0 when not unlimited", () => {
    const r = accessEntrySchema.safeParse({
      client_id: "c1",
      listen_port_start: 1000,
      listen_port_end: 2000,
      protocols: ["tcp"],
      unlimited: false,
      bandwidth_in_bps: null,
      bandwidth_out_bps: null,
      new_connections_per_sec: null,
      concurrent_connections: null,
    });
    expect(r.success).toBe(false);
  });

  it("accepts unlimited=true with all caps null", () => {
    const r = accessEntrySchema.safeParse({
      client_id: "c1",
      listen_port_start: 1000,
      listen_port_end: 2000,
      protocols: ["tcp", "udp"],
      unlimited: true,
    });
    expect(r.success).toBe(true);
  });

  it("rejects burst without matching rate", () => {
    const r = accessEntrySchema.safeParse({
      client_id: "c1",
      listen_port_start: 1000,
      listen_port_end: 2000,
      protocols: ["tcp"],
      unlimited: false,
      bandwidth_in_bps: null,
      bandwidth_in_burst: 1_000_000,
      bandwidth_out_bps: 500_000,
      new_connections_per_sec: null,
      concurrent_connections: null,
    });
    expect(r.success).toBe(false);
  });
});
