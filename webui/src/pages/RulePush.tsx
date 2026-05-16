import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";
import { Plus, Trash2 } from "lucide-react";

import { usePushRule } from "@/api/rules";
import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  EMPTY_RATE_LIMIT_FORM,
  RateLimitForm,
  formStateToRateLimit,
} from "@/components/RateLimitForm";

type FormMode = "single" | "multi";
const PROXY_PROTOCOL_NONE = "__none";

interface TargetRow {
  host: string;
  port: string;
  proxyProtocol: "" | "v1" | "v2";
}

export function RulePush() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const push = usePushRule();
  const [client, setClient] = useState("client-a");
  const [listenStart, setListenStart] = useState("30000");
  const [listenEnd, setListenEnd] = useState("");
  const [target, setTarget] = useState("127.0.0.1");
  const [targetStart, setTargetStart] = useState("9000");
  const [targetEnd, setTargetEnd] = useState("");
  const [protocol, setProtocol] = useState<"tcp" | "udp">("tcp");
  // 009-tls-sni-routing T084: optional TLS Server Name Indication
  // selector. Server-side validation rejects this on UDP / range
  // rules, so the input is rendered only when those constraints
  // hold. The empty string maps to "absent" — the wire field is
  // serialised only when non-empty.
  const [sniPattern, setSniPattern] = useState("");
  const [error, setError] = useState<string | null>(null);

  const sniEligible = protocol === "tcp" && !listenEnd;

  // 007-multi-target-failover T046: opt into the multi-target form. The
  // legacy single-target shape stays the default to keep operator
  // muscle memory intact.
  const [mode, setMode] = useState<FormMode>("single");
  const [targets, setTargets] = useState<TargetRow[]>([
    { host: "127.0.0.1", port: "9000", proxyProtocol: "" },
    { host: "127.0.0.1", port: "9001", proxyProtocol: "" },
  ]);
  const [healthCheckInterval, setHealthCheckInterval] = useState("");
  // 011-rate-limiting-qos T039: optional QoS caps. Server validates
  // (non-zero, burst-without-rate, range, capability gate). Empty
  // form = no rate_limit field on the wire (preserves SC-004
  // byte-stability for opt-out rules).
  const [rateLimit, setRateLimit] = useState({ ...EMPTY_RATE_LIMIT_FORM });

  function addTarget() {
    setTargets((rows) => [...rows, { host: "", port: "", proxyProtocol: "" }]);
  }
  function removeTarget(idx: number) {
    setTargets((rows) => (rows.length <= 1 ? rows : rows.filter((_, i) => i !== idx)));
  }
  function updateTarget(idx: number, key: keyof TargetRow, value: string) {
    setTargets((rows) =>
      rows.map((row, i) => (i === idx ? { ...row, [key]: value } : row)),
    );
  }

  async function handleSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setError(null);
    try {
      const trimmedSni = sniPattern.trim();
      const rl = formStateToRateLimit(rateLimit);
      const baseBody = {
        client,
        listen_port: Number(listenStart),
        ...(listenEnd ? { listen_port_end: Number(listenEnd) } : {}),
        protocol,
        // 009-tls-sni-routing T084: thread the SNI selector through
        // the wire body. Empty / non-eligible inputs are omitted so
        // the server applies its grammar-validated default (legacy
        // shape).
        ...(sniEligible && trimmedSni ? { sni_pattern: trimmedSni } : {}),
        // 011-rate-limiting-qos T039: omit rate_limit entirely when
        // the operator left every cap blank. Preserves SC-004 wire
        // byte-stability for v0.10-shaped rule pushes.
        ...(rl ? { rate_limit: rl } : {}),
      };
      const body =
        mode === "single"
          ? {
              ...baseBody,
              target_host: target,
              target_port: Number(targetStart),
              ...(targetEnd ? { target_port_end: Number(targetEnd) } : {}),
            }
          : {
              ...baseBody,
              targets: targets.map((row, idx) => ({
                host: row.host,
                port: Number(row.port),
                priority: idx,
                ...(protocol === "tcp" && row.proxyProtocol
                  ? { proxy_protocol: row.proxyProtocol }
                  : {}),
              })),
              ...(healthCheckInterval
                ? { health_check_interval_secs: Number(healthCheckInterval) }
                : {}),
            };
      const res = await push.mutateAsync(body);
      navigate(`/rules/${res.rule_id}`);
    } catch (err) {
      setError(err instanceof ApiError ? `${err.code}: ${err.message}` : (err as Error).message);
    }
  }

  return (
    <Card className="w-full max-w-2xl">
      <CardHeader>
        <CardTitle>{t("rulePush.title")}</CardTitle>
      </CardHeader>
      <CardContent>
        <form onSubmit={handleSubmit} className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="client">{t("rules.client")}</Label>
            <Input id="client" value={client} onChange={(e) => setClient(e.target.value)} required />
          </div>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <div className="space-y-2">
              <Label htmlFor="ls">{t("rulePush.listenStart")}</Label>
              <Input id="ls" type="number" value={listenStart} onChange={(e) => setListenStart(e.target.value)} required />
            </div>
            <div className="space-y-2">
              <Label htmlFor="le">{t("rulePush.listenEnd")}</Label>
              <Input id="le" type="number" value={listenEnd} onChange={(e) => setListenEnd(e.target.value)} placeholder={t("rulePush.optional")} />
            </div>
          </div>

          <div className="space-y-2">
            <Label>{t("rulePush.targetMode")}</Label>
            <div className="flex gap-3 text-sm">
              {(["single", "multi"] as const).map((m) => (
                <label key={m} className="flex items-center gap-2">
                  <input
                    type="radio"
                    checked={mode === m}
                    onChange={() => setMode(m)}
                  />
                  {t(`rulePush.targetMode_${m}`)}
                </label>
              ))}
            </div>
          </div>

          {mode === "single" ? (
            <>
              <div className="space-y-2">
                <Label htmlFor="target">{t("rulePush.targetHost")}</Label>
                <Input id="target" value={target} onChange={(e) => setTarget(e.target.value)} required />
              </div>
              <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                <div className="space-y-2">
                  <Label htmlFor="ts">{t("rulePush.targetStart")}</Label>
                  <Input id="ts" type="number" value={targetStart} onChange={(e) => setTargetStart(e.target.value)} required />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="te">{t("rulePush.targetEnd")}</Label>
                  <Input id="te" type="number" value={targetEnd} onChange={(e) => setTargetEnd(e.target.value)} placeholder={t("rulePush.optional")} />
                </div>
              </div>
            </>
          ) : (
            <div className="space-y-3">
              <Label>{t("rulePush.targets")}</Label>
              <div className="space-y-2">
                {targets.map((row, idx) => (
                  <div
                    key={idx}
                    className="grid grid-cols-1 gap-2 rounded-md border p-3 sm:grid-cols-[1fr_120px_120px_72px_auto] sm:items-center sm:border-0 sm:p-0"
                  >
                    <Input
                      placeholder={t("rulePush.targetHost")}
                      value={row.host}
                      onChange={(e) => updateTarget(idx, "host", e.target.value)}
                      required
                    />
                    <Input
                      placeholder={t("rulePush.targetPort")}
                      type="number"
                      value={row.port}
                      onChange={(e) => updateTarget(idx, "port", e.target.value)}
                      required
                    />
                    <Select
                      value={row.proxyProtocol || PROXY_PROTOCOL_NONE}
                      onValueChange={(value) =>
                        updateTarget(
                          idx,
                          "proxyProtocol",
                          value === PROXY_PROTOCOL_NONE ? "" : value,
                        )
                      }
                      disabled={protocol !== "tcp"}
                    >
                      <SelectTrigger aria-label={t("rulePush.proxyProtocolDisabled")}>
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectGroup>
                          <SelectItem value={PROXY_PROTOCOL_NONE}>
                            {t("rulePush.proxyProtocolDisabled")}
                          </SelectItem>
                          <SelectItem value="v1">{t("rulePush.proxyProtocolV1")}</SelectItem>
                          <SelectItem value="v2">{t("rulePush.proxyProtocolV2")}</SelectItem>
                        </SelectGroup>
                      </SelectContent>
                    </Select>
                    <span className="text-sm text-muted-foreground sm:text-center">
                      {t("rulePush.priority")} {idx}
                    </span>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      onClick={() => removeTarget(idx)}
                      disabled={targets.length <= 1}
                      aria-label={t("rulePush.removeTarget")}
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </div>
                ))}
              </div>
              <Button type="button" variant="outline" size="sm" onClick={addTarget} className="w-full sm:w-auto">
                <Plus className="h-4 w-4 mr-1" />
                {t("rulePush.addTarget")}
              </Button>
              <div className="space-y-2">
                <Label htmlFor="hci">{t("rulePush.healthCheckInterval")}</Label>
                <Input
                  id="hci"
                  type="number"
                  min={1}
                  max={3600}
                  value={healthCheckInterval}
                  onChange={(e) => setHealthCheckInterval(e.target.value)}
                  placeholder={t("rulePush.healthCheckIntervalPlaceholder")}
                />
                <p className="text-xs text-muted-foreground">
                  {t("rulePush.healthCheckIntervalHelp")}
                </p>
              </div>
            </div>
          )}

          <div className="space-y-2">
            <Label>{t("rules.protocol")}</Label>
            <div className="flex gap-3 text-sm">
              {(["tcp", "udp"] as const).map((p) => (
                <label key={p} className="flex items-center gap-2">
                  <input type="radio" checked={protocol === p} onChange={() => setProtocol(p)} />
                  {p.toUpperCase()}
                </label>
              ))}
            </div>
          </div>

          {/* 009-tls-sni-routing T084: SNI selector input. Only
              rendered when the rule is TCP single-port; UDP and
              port-range rules would be rejected at the API layer. */}
          {sniEligible && (
            <div className="space-y-2">
              <Label htmlFor="sni">{t("rulePush.sniPattern")}</Label>
              <Input
                id="sni"
                value={sniPattern}
                onChange={(e) => setSniPattern(e.target.value)}
                placeholder={t("rulePush.sniPatternPlaceholder")}
              />
              <p className="text-xs text-muted-foreground">
                {t("rulePush.sniPatternHelp")}
              </p>
            </div>
          )}

          <div className="space-y-2">
            <Label>{t("rulePush.rateLimitTitle")}</Label>
            <p className="text-xs text-muted-foreground">{t("rulePush.rateLimitHelp")}</p>
            <RateLimitForm state={rateLimit} onChange={setRateLimit} />
          </div>

          {error && <p className="text-sm text-destructive">{error}</p>}
          <div className="flex flex-col gap-2 sm:flex-row">
            <Button type="submit" disabled={push.isPending}>
              {push.isPending ? t("confirm.busy") : t("rulePush.submit")}
            </Button>
            <Button type="button" variant="outline" onClick={() => navigate(-1)}>
              {t("confirm.cancel")}
            </Button>
          </div>
        </form>
      </CardContent>
    </Card>
  );
}
