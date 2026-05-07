import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";

import { useCreateGrant } from "@/api/grants";
import { useUsersList } from "@/api/users";
import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

export function GrantCreate() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const create = useCreateGrant();
  const users = useUsersList();
  const [userId, setUserId] = useState("");
  const [client, setClient] = useState("*");
  const [start, setStart] = useState("30000");
  const [end, setEnd] = useState("30100");
  const [tcp, setTcp] = useState(true);
  const [udp, setUdp] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function handleSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setError(null);
    if (!tcp && !udp) {
      setError(t("grantCreate.protocolRequired"));
      return;
    }
    const protocols: ("tcp" | "udp")[] = [];
    if (tcp) protocols.push("tcp");
    if (udp) protocols.push("udp");
    try {
      await create.mutateAsync({
        user_id: userId,
        client,
        listen_port_start: Number(start),
        listen_port_end: Number(end),
        protocols,
      });
      navigate(`/grants?user_id=${encodeURIComponent(userId)}`);
    } catch (err) {
      setError(err instanceof ApiError ? `${err.code}: ${err.message}` : (err as Error).message);
    }
  }

  return (
    <Card className="max-w-2xl">
      <CardHeader>
        <CardTitle>{t("grantCreate.title")}</CardTitle>
      </CardHeader>
      <CardContent>
        <form onSubmit={handleSubmit} className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="user_id">{t("grants.user")}</Label>
            <select
              id="user_id"
              required
              className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
              value={userId}
              onChange={(e) => setUserId(e.target.value)}
            >
              <option value="" disabled>
                {t("grantCreate.selectUser")}
              </option>
              {(users.data ?? [])
                .filter((u) => u.role !== "superadmin")
                .map((u) => (
                  <option key={u.user_id} value={u.user_id}>
                    {u.user_id} — {u.display_name}
                  </option>
                ))}
            </select>
          </div>
          <div className="space-y-2">
            <Label htmlFor="client">{t("grants.client")}</Label>
            <Input
              id="client"
              value={client}
              onChange={(e) => setClient(e.target.value)}
              placeholder="* or client-a"
            />
          </div>
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-2">
              <Label htmlFor="start">{t("grantCreate.startPort")}</Label>
              <Input id="start" type="number" min={1} max={65535} value={start} onChange={(e) => setStart(e.target.value)} />
            </div>
            <div className="space-y-2">
              <Label htmlFor="end">{t("grantCreate.endPort")}</Label>
              <Input id="end" type="number" min={1} max={65535} value={end} onChange={(e) => setEnd(e.target.value)} />
            </div>
          </div>
          <div className="space-y-2">
            <Label>{t("grants.protocols")}</Label>
            <div className="flex items-center gap-4 text-sm">
              <label className="flex items-center gap-2">
                <input type="checkbox" checked={tcp} onChange={(e) => setTcp(e.target.checked)} />
                TCP
              </label>
              <label className="flex items-center gap-2">
                <input type="checkbox" checked={udp} onChange={(e) => setUdp(e.target.checked)} />
                UDP
              </label>
            </div>
          </div>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <div className="flex gap-2">
            <Button type="submit" disabled={create.isPending}>
              {create.isPending ? t("confirm.busy") : t("grantCreate.submit")}
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
