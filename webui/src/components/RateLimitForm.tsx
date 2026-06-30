/// 011-rate-limiting-qos T038: shared QoS cap editor.
/// Four cap inputs (bandwidth in/out, new conn/sec, concurrent
/// connections) plus three matching burst overrides hidden behind
/// an "Advanced" disclosure. `concurrent_connections_burst` is
/// intentionally absent — the server rejects it when non-null.
///
/// Empty inputs map to "uncapped on that dimension"; the parent
/// component is expected to call `formStateToRateLimit` before submitting
/// to strip empty values into proper `null`/omit shape.

import { ChevronDown, ChevronRight } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { Input } from "@/components/ui/input";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import type { RateLimitFormState } from "@/components/RateLimitForm.helpers";

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
    <div className="flex flex-col gap-3 rounded-md border border-border p-4">
      <div className="flex flex-col gap-1">
        <p className="text-sm font-medium">{t("rateLimitForm.title")}</p>
        {helper && <FieldDescription className="text-xs">{helper}</FieldDescription>}
        <FieldDescription className="text-xs">{t("rateLimitForm.help")}</FieldDescription>
      </div>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <Field>
          <FieldLabel htmlFor="rl-bw-in" className="text-xs">
            {t("rateLimitForm.bandwidthIn")}
          </FieldLabel>
          <Input
            id="rl-bw-in"
            type="number"
            min={1}
            placeholder={t("rateLimitForm.uncapped")}
            value={state.bandwidth_in_bps}
            disabled={disabled}
            onChange={(e) => setField("bandwidth_in_bps", e.target.value)}
          />
        </Field>
        <Field>
          <FieldLabel htmlFor="rl-bw-out" className="text-xs">
            {t("rateLimitForm.bandwidthOut")}
          </FieldLabel>
          <Input
            id="rl-bw-out"
            type="number"
            min={1}
            placeholder={t("rateLimitForm.uncapped")}
            value={state.bandwidth_out_bps}
            disabled={disabled}
            onChange={(e) => setField("bandwidth_out_bps", e.target.value)}
          />
        </Field>
        <Field>
          <FieldLabel htmlFor="rl-conn-rate" className="text-xs">
            {t("rateLimitForm.newConnectionsPerSec")}
          </FieldLabel>
          <Input
            id="rl-conn-rate"
            type="number"
            min={1}
            placeholder={t("rateLimitForm.uncapped")}
            value={state.new_connections_per_sec}
            disabled={disabled}
            onChange={(e) => setField("new_connections_per_sec", e.target.value)}
          />
        </Field>
        <Field>
          <FieldLabel htmlFor="rl-conn-conc" className="text-xs">
            {t("rateLimitForm.concurrentConnections")}
          </FieldLabel>
          <Input
            id="rl-conn-conc"
            type="number"
            min={1}
            placeholder={t("rateLimitForm.uncapped")}
            value={state.concurrent_connections}
            disabled={disabled}
            onChange={(e) => setField("concurrent_connections", e.target.value)}
          />
        </Field>
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
          <Field>
            <FieldLabel htmlFor="rl-bw-in-burst" className="text-xs">
              {t("rateLimitForm.bandwidthInBurst")}
            </FieldLabel>
            <Input
              id="rl-bw-in-burst"
              type="number"
              min={1}
              placeholder={t("rateLimitForm.burstDefault")}
              value={state.bandwidth_in_burst}
              disabled={disabled}
              onChange={(e) => setField("bandwidth_in_burst", e.target.value)}
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="rl-bw-out-burst" className="text-xs">
              {t("rateLimitForm.bandwidthOutBurst")}
            </FieldLabel>
            <Input
              id="rl-bw-out-burst"
              type="number"
              min={1}
              placeholder={t("rateLimitForm.burstDefault")}
              value={state.bandwidth_out_burst}
              disabled={disabled}
              onChange={(e) => setField("bandwidth_out_burst", e.target.value)}
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="rl-conn-rate-burst" className="text-xs">
              {t("rateLimitForm.newConnectionsBurst")}
            </FieldLabel>
            <Input
              id="rl-conn-rate-burst"
              type="number"
              min={1}
              placeholder={t("rateLimitForm.burstDefault")}
              value={state.new_connections_burst}
              disabled={disabled}
              onChange={(e) => setField("new_connections_burst", e.target.value)}
            />
          </Field>
          <div className="text-xs text-muted-foreground self-end pb-1">
            {t("rateLimitForm.burstHelp")}
          </div>
        </div>
      )}
    </div>
  );
}
