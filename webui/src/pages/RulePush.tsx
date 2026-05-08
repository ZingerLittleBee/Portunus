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

type FormMode = "single" | "multi";

interface TargetRow {
  host: string;
  port: string;
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
  const [error, setError] = useState<string | null>(null);

  // 007-multi-target-failover T046: opt into the multi-target form. The
  // legacy single-target shape stays the default to keep operator
  // muscle memory intact.
  const [mode, setMode] = useState<FormMode>("single");
  const [targets, setTargets] = useState<TargetRow[]>([
    { host: "127.0.0.1", port: "9000" },
    { host: "127.0.0.1", port: "9001" },
  ]);
  const [healthCheckInterval, setHealthCheckInterval] = useState("");

  function addTarget() {
    setTargets((rows) => [...rows, { host: "", port: "" }]);
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
      const baseBody = {
        client,
        listen_port: Number(listenStart),
        ...(listenEnd ? { listen_port_end: Number(listenEnd) } : {}),
        protocol,
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
    <Card className="max-w-2xl">
      <CardHeader>
        <CardTitle>{t("rulePush.title")}</CardTitle>
      </CardHeader>
      <CardContent>
        <form onSubmit={handleSubmit} className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="client">{t("rules.client")}</Label>
            <Input id="client" value={client} onChange={(e) => setClient(e.target.value)} required />
          </div>
          <div className="grid grid-cols-2 gap-3">
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
              <div className="grid grid-cols-2 gap-3">
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
                  <div key={idx} className="grid grid-cols-[1fr_120px_72px_auto] gap-2 items-center">
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
                    <span className="text-sm text-muted-foreground">
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
              <Button type="button" variant="outline" size="sm" onClick={addTarget}>
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
          {error && <p className="text-sm text-destructive">{error}</p>}
          <div className="flex gap-2">
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
