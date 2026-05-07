import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";

import { usePushRule } from "@/api/rules";
import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

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

  async function handleSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setError(null);
    try {
      const res = await push.mutateAsync({
        client,
        listen_port: Number(listenStart),
        ...(listenEnd ? { listen_port_end: Number(listenEnd) } : {}),
        target_host: target,
        target_port: Number(targetStart),
        ...(targetEnd ? { target_port_end: Number(targetEnd) } : {}),
        protocol,
      });
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
