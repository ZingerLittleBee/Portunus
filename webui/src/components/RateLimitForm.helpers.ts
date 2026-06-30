import type { RateLimit } from "@/api/types";

export interface RateLimitFormState {
  bandwidth_in_bps: string;
  bandwidth_out_bps: string;
  new_connections_per_sec: string;
  concurrent_connections: string;
  bandwidth_in_burst: string;
  bandwidth_out_burst: string;
  new_connections_burst: string;
}

export const EMPTY_RATE_LIMIT_FORM: RateLimitFormState = {
  bandwidth_in_bps: "",
  bandwidth_out_bps: "",
  new_connections_per_sec: "",
  concurrent_connections: "",
  bandwidth_in_burst: "",
  bandwidth_out_burst: "",
  new_connections_burst: "",
};

export function rateLimitToFormState(rl: RateLimit | null | undefined): RateLimitFormState {
  if (!rl) return { ...EMPTY_RATE_LIMIT_FORM };
  return {
    bandwidth_in_bps: stringify(rl.bandwidth_in_bps),
    bandwidth_out_bps: stringify(rl.bandwidth_out_bps),
    new_connections_per_sec: stringify(rl.new_connections_per_sec),
    concurrent_connections: stringify(rl.concurrent_connections),
    bandwidth_in_burst: stringify(rl.bandwidth_in_burst),
    bandwidth_out_burst: stringify(rl.bandwidth_out_burst),
    new_connections_burst: stringify(rl.new_connections_burst),
  };
}

export function formStateToRateLimit(form: RateLimitFormState): RateLimit | undefined {
  const numeric = {
    bandwidth_in_bps: parseOpt(form.bandwidth_in_bps),
    bandwidth_out_bps: parseOpt(form.bandwidth_out_bps),
    new_connections_per_sec: parseOpt(form.new_connections_per_sec),
    concurrent_connections: parseOpt(form.concurrent_connections),
    bandwidth_in_burst: parseOpt(form.bandwidth_in_burst),
    bandwidth_out_burst: parseOpt(form.bandwidth_out_burst),
    new_connections_burst: parseOpt(form.new_connections_burst),
  };
  const hasAny = Object.values(numeric).some((v) => v !== undefined);
  if (!hasAny) return undefined;
  const out: RateLimit = {};
  if (numeric.bandwidth_in_bps !== undefined) out.bandwidth_in_bps = numeric.bandwidth_in_bps;
  if (numeric.bandwidth_out_bps !== undefined) out.bandwidth_out_bps = numeric.bandwidth_out_bps;
  if (numeric.new_connections_per_sec !== undefined) {
    out.new_connections_per_sec = numeric.new_connections_per_sec;
  }
  if (numeric.concurrent_connections !== undefined) {
    out.concurrent_connections = numeric.concurrent_connections;
  }
  if (numeric.bandwidth_in_burst !== undefined) out.bandwidth_in_burst = numeric.bandwidth_in_burst;
  if (numeric.bandwidth_out_burst !== undefined) out.bandwidth_out_burst = numeric.bandwidth_out_burst;
  if (numeric.new_connections_burst !== undefined) {
    out.new_connections_burst = numeric.new_connections_burst;
  }
  return out;
}

export function summarizeRateLimit(rl: RateLimit | null | undefined): string | null {
  if (!rl) return null;
  const parts: string[] = [];
  if (rl.bandwidth_in_bps != null) parts.push(`↓${formatBps(rl.bandwidth_in_bps)}`);
  if (rl.bandwidth_out_bps != null) parts.push(`↑${formatBps(rl.bandwidth_out_bps)}`);
  if (rl.new_connections_per_sec != null) parts.push(`${rl.new_connections_per_sec}/s`);
  if (rl.concurrent_connections != null) parts.push(`≤${rl.concurrent_connections}`);
  return parts.length ? parts.join(" · ") : null;
}

function stringify(n: number | null | undefined): string {
  return n == null ? "" : String(n);
}

function parseOpt(s: string): number | undefined {
  const trimmed = s.trim();
  if (!trimmed) return undefined;
  const n = Number(trimmed);
  return Number.isFinite(n) ? n : undefined;
}

function formatBps(n: number): string {
  if (n >= 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)}M`;
  if (n >= 1024) return `${(n / 1024).toFixed(0)}K`;
  return String(n);
}
