/// 011-rate-limiting-qos T038: shared QoS cap editor.
/// Four cap inputs (bandwidth in/out, new conn/sec, concurrent
/// connections) plus three matching burst overrides hidden behind
/// an "Advanced" disclosure. `concurrent_connections_burst` is
/// intentionally absent — the server rejects it when non-null.
///
/// Empty inputs map to "uncapped on that dimension"; the parent
/// component is expected to call [`rateLimitToBody`] before submitting
/// to strip empty values into proper `null`/omit shape.

import { ChevronDown, ChevronRight } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import type { RateLimit } from "@/api/types";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

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

/// Hydrate the form state from a server-returned `RateLimit`. Used
/// when prefilling an edit form on a rule that already has caps.
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

/// Convert form state into a `RateLimit` body suitable for the
/// operator API. Returns `undefined` when every cap is empty so the
/// caller can omit the wire field entirely (preserves SC-004
/// byte-stability for rules that opted out).
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
  for (const [k, v] of Object.entries(numeric)) {
    if (v !== undefined) (out as Record<string, number>)[k] = v;
  }
  return out;
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

interface Props {
  state: RateLimitFormState;
  onChange: (state: RateLimitFormState) => void;
  /// Disable input editing (e.g., capability gate failed for the
  /// target client). Inputs render as read-only with a tooltip
  /// explaining why.
  disabled?: boolean;
  /// Optional helper rendered below the title, e.g., capability
  /// gate explanation.
  helper?: string;
}

export function RateLimitForm({ state, onChange, disabled, helper }: Props) {
  const { t } = useTranslation();
  const [advancedOpen, setAdvancedOpen] = useState(
    state.bandwidth_in_burst !== "" ||
      state.bandwidth_out_burst !== "" ||
      state.new_connections_burst !== "",
  );

  function setField<K extends keyof RateLimitFormState>(key: K, value: string) {
    onChange({ ...state, [key]: value });
  }

  return (
    <div className="space-y-3 rounded-md border border-border p-4">
      <div className="space-y-1">
        <Label className="text-sm font-medium">{t("rateLimitForm.title")}</Label>
        {helper && <p className="text-xs text-muted-foreground">{helper}</p>}
        <p className="text-xs text-muted-foreground">{t("rateLimitForm.help")}</p>
      </div>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <div className="space-y-1">
          <Label htmlFor="rl-bw-in" className="text-xs">
            {t("rateLimitForm.bandwidthIn")}
          </Label>
          <Input
            id="rl-bw-in"
            type="number"
            min={1}
            placeholder={t("rateLimitForm.uncapped")}
            value={state.bandwidth_in_bps}
            disabled={disabled}
            onChange={(e) => setField("bandwidth_in_bps", e.target.value)}
          />
        </div>
        <div className="space-y-1">
          <Label htmlFor="rl-bw-out" className="text-xs">
            {t("rateLimitForm.bandwidthOut")}
          </Label>
          <Input
            id="rl-bw-out"
            type="number"
            min={1}
            placeholder={t("rateLimitForm.uncapped")}
            value={state.bandwidth_out_bps}
            disabled={disabled}
            onChange={(e) => setField("bandwidth_out_bps", e.target.value)}
          />
        </div>
        <div className="space-y-1">
          <Label htmlFor="rl-conn-rate" className="text-xs">
            {t("rateLimitForm.newConnectionsPerSec")}
          </Label>
          <Input
            id="rl-conn-rate"
            type="number"
            min={1}
            placeholder={t("rateLimitForm.uncapped")}
            value={state.new_connections_per_sec}
            disabled={disabled}
            onChange={(e) => setField("new_connections_per_sec", e.target.value)}
          />
        </div>
        <div className="space-y-1">
          <Label htmlFor="rl-conn-conc" className="text-xs">
            {t("rateLimitForm.concurrentConnections")}
          </Label>
          <Input
            id="rl-conn-conc"
            type="number"
            min={1}
            placeholder={t("rateLimitForm.uncapped")}
            value={state.concurrent_connections}
            disabled={disabled}
            onChange={(e) => setField("concurrent_connections", e.target.value)}
          />
        </div>
      </div>
      <button
        type="button"
        className="flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
        onClick={() => setAdvancedOpen((v) => !v)}
        aria-expanded={advancedOpen}
      >
        {advancedOpen ? (
          <ChevronDown className="h-3 w-3" />
        ) : (
          <ChevronRight className="h-3 w-3" />
        )}
        {t("rateLimitForm.advanced")}
      </button>
      {advancedOpen && (
        <div className="grid grid-cols-1 gap-3 border-l border-border pl-4 sm:grid-cols-2">
          <div className="space-y-1">
            <Label htmlFor="rl-bw-in-burst" className="text-xs">
              {t("rateLimitForm.bandwidthInBurst")}
            </Label>
            <Input
              id="rl-bw-in-burst"
              type="number"
              min={1}
              placeholder={t("rateLimitForm.burstDefault")}
              value={state.bandwidth_in_burst}
              disabled={disabled}
              onChange={(e) => setField("bandwidth_in_burst", e.target.value)}
            />
          </div>
          <div className="space-y-1">
            <Label htmlFor="rl-bw-out-burst" className="text-xs">
              {t("rateLimitForm.bandwidthOutBurst")}
            </Label>
            <Input
              id="rl-bw-out-burst"
              type="number"
              min={1}
              placeholder={t("rateLimitForm.burstDefault")}
              value={state.bandwidth_out_burst}
              disabled={disabled}
              onChange={(e) => setField("bandwidth_out_burst", e.target.value)}
            />
          </div>
          <div className="space-y-1">
            <Label htmlFor="rl-conn-rate-burst" className="text-xs">
              {t("rateLimitForm.newConnectionsBurst")}
            </Label>
            <Input
              id="rl-conn-rate-burst"
              type="number"
              min={1}
              placeholder={t("rateLimitForm.burstDefault")}
              value={state.new_connections_burst}
              disabled={disabled}
              onChange={(e) => setField("new_connections_burst", e.target.value)}
            />
          </div>
          <div className="text-xs text-muted-foreground self-end pb-1">
            {t("rateLimitForm.burstHelp")}
          </div>
        </div>
      )}
    </div>
  );
}

/// Render a compact summary string for the rules-table `Caps` column
/// (T039). Returns `null` when there are no caps to render so the
/// caller can render a `—` cell.
export function summarizeRateLimit(rl: RateLimit | null | undefined): string | null {
  if (!rl) return null;
  const parts: string[] = [];
  if (rl.bandwidth_in_bps != null) parts.push(`↓${formatBps(rl.bandwidth_in_bps)}`);
  if (rl.bandwidth_out_bps != null) parts.push(`↑${formatBps(rl.bandwidth_out_bps)}`);
  if (rl.new_connections_per_sec != null) parts.push(`${rl.new_connections_per_sec}/s`);
  if (rl.concurrent_connections != null) parts.push(`≤${rl.concurrent_connections}`);
  return parts.length ? parts.join(" · ") : null;
}

function formatBps(n: number): string {
  if (n >= 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)}M`;
  if (n >= 1024) return `${(n / 1024).toFixed(0)}K`;
  return String(n);
}
