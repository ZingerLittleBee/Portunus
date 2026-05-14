import { z } from "zod";

export function formatBps(n: number): string {
  if (!Number.isFinite(n) || n < 0) return "—";
  if (n < 1_000) return `${n} bps`;
  if (n < 1_000_000) return `${(n / 1_000).toFixed(1)} KB/s`;
  if (n < 1_000_000_000) return `${(n / 1_000_000).toFixed(1)} MB/s`;
  return `${(n / 1_000_000_000).toFixed(1)} GB/s`;
}

export function parseBpsInput(raw: string): number | null {
  const s = raw.trim();
  if (!s) return null;
  const m = s.match(/^(\d+(?:\.\d+)?)\s*([KMG]?)B?(?:\/s)?$/i);
  if (!m) {
    const n = Number(s);
    return Number.isFinite(n) && n >= 0 ? n : null;
  }
  const valueRaw = m[1];
  if (!valueRaw) return null;
  const value = Number(valueRaw);
  if (!Number.isFinite(value)) return null;
  const unit = (m[2] ?? "").toUpperCase();
  const mul = unit === "K" ? 1_000 : unit === "M" ? 1_000_000 : unit === "G" ? 1_000_000_000 : 1;
  return Math.round(value * mul);
}

const portInt = z.number().int().min(1).max(65535);
const positiveOrNull = z.number().int().positive().nullable().optional();

const baseShape = {
  client_name: z.string().min(1),
  listen_port_start: portInt,
  listen_port_end: portInt,
  protocols: z.array(z.enum(["tcp", "udp"])).min(1),
  note: z.string().optional(),
  unlimited: z.boolean(),
  bandwidth_in_bps: positiveOrNull,
  bandwidth_out_bps: positiveOrNull,
  new_connections_per_sec: positiveOrNull,
  concurrent_connections: positiveOrNull,
  bandwidth_in_burst: positiveOrNull,
  bandwidth_out_burst: positiveOrNull,
  new_connections_burst: positiveOrNull,
};

/// Validation schema for the user-quota form: combines a grant (client +
/// port range + protocols) with an optional rate-limit cap. When
/// `unlimited` is true, all cap fields must be empty/null; otherwise at
/// least one of the four caps must be > 0. Burst values require their
/// matching rate to be set.
export const accessEntrySchema = z
  .object(baseShape)
  .refine((d) => d.listen_port_start <= d.listen_port_end, {
    message: "listen_port_start must be <= listen_port_end",
    path: ["listen_port_end"],
  })
  .refine(
    (d) => {
      if (d.unlimited) return true;
      return [
        d.bandwidth_in_bps,
        d.bandwidth_out_bps,
        d.new_connections_per_sec,
        d.concurrent_connections,
      ].some((v) => typeof v === "number" && v > 0);
    },
    {
      message: "at least one cap must be set when not unlimited",
      path: ["bandwidth_in_bps"],
    },
  )
  .refine((d) => !(d.bandwidth_in_burst && !d.bandwidth_in_bps), {
    message: "bandwidth_in_burst requires bandwidth_in_bps",
    path: ["bandwidth_in_burst"],
  })
  .refine((d) => !(d.bandwidth_out_burst && !d.bandwidth_out_bps), {
    message: "bandwidth_out_burst requires bandwidth_out_bps",
    path: ["bandwidth_out_burst"],
  })
  .refine((d) => !(d.new_connections_burst && !d.new_connections_per_sec), {
    message: "new_connections_burst requires new_connections_per_sec",
    path: ["new_connections_burst"],
  });

export type AccessEntryInput = z.infer<typeof accessEntrySchema>;
