import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";
import { ChevronDown, ChevronRight } from "lucide-react";
import { toast } from "sonner";

import { useCreateUser } from "@/api/users";
import { useCreateAccessEntry } from "@/api/access-entries";
import { useClientsList } from "@/api/clients";
import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { UserQuotaForm, type UserQuotaFormSubmitValue } from "@/components/UserQuota/UserQuotaForm";

export function UserCreate() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const create = useCreateUser();
  const [userId, setUserId] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [initialPassword, setInitialPassword] = useState("");
  const [forcePasswordChange, setForcePasswordChange] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [showInitialQuota, setShowInitialQuota] = useState(false);
  const [pendingQuota, setPendingQuota] = useState<UserQuotaFormSubmitValue | null>(null);
  const clientsQ = useClientsList();
  const clientLites = (clientsQ.data ?? []).map((c) => ({
    client_name: c.client_name,
    connected: c.connected,
  }));
  const createEntry = useCreateAccessEntry(userId);

  async function handleSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setError(null);
    if (!/^[a-z][a-z0-9-_]*$/.test(userId)) {
      setError(t("userCreate.invalidId"));
      return;
    }
    try {
      const res = await create.mutateAsync({
        user_id: userId,
        display_name: displayName,
        ...(initialPassword
          ? {
              initial_password: initialPassword,
              password_change_required: forcePasswordChange,
            }
          : {}),
      });
      if (pendingQuota && res.user_id) {
        try {
          await createEntry.mutateAsync({
            user_id: res.user_id,
            client_name: pendingQuota.client_name,
            listen_port_start: pendingQuota.listen_port_start,
            listen_port_end: pendingQuota.listen_port_end,
            protocols: pendingQuota.protocols,
            ...(pendingQuota.cap !== undefined ? { cap: pendingQuota.cap } : {}),
          });
          toast.success(t("userQuota.toast.created", { client: pendingQuota.client_name }));
        } catch (err) {
          const msg = err instanceof ApiError ? `${err.code}: ${err.message}` : (err as Error).message;
          toast.warning(`${t("userQuota.toast.createFailed")}: ${msg}`);
        }
      }
      navigate(`/users/${res.user_id}`);
    } catch (err) {
      const msg = err instanceof ApiError ? `${err.code}: ${err.message}` : (err as Error).message;
      setError(msg);
    }
  }

  return (
    <Card className="max-w-xl">
      <CardHeader>
        <CardTitle>{t("userCreate.title")}</CardTitle>
      </CardHeader>
      <CardContent>
        <form onSubmit={handleSubmit} className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="user_id">{t("users.id")}</Label>
            <Input
              id="user_id"
              value={userId}
              onChange={(e) => setUserId(e.target.value)}
              placeholder="alice"
              autoComplete="off"
              required
            />
            <p className="text-xs text-muted-foreground">{t("userCreate.idHint")}</p>
          </div>
          <div className="space-y-2">
            <Label htmlFor="display_name">{t("users.displayName")}</Label>
            <Input
              id="display_name"
              value={displayName}
              onChange={(e) => setDisplayName(e.target.value)}
              placeholder="Alice — payments"
              required
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="initial_password">{t("userCreate.initialPassword")}</Label>
            <Input
              id="initial_password"
              type="password"
              autoComplete="new-password"
              value={initialPassword}
              onChange={(e) => setInitialPassword(e.target.value)}
            />
            <p className="text-xs text-muted-foreground">{t("userCreate.initialPasswordHint")}</p>
          </div>
          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              checked={forcePasswordChange}
              onChange={(e) => setForcePasswordChange(e.target.checked)}
              disabled={!initialPassword}
            />
            {t("userCreate.forcePasswordChange")}
          </label>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <div className="border-t pt-4">
            <button
              type="button"
              className="flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
              onClick={() => setShowInitialQuota((v) => !v)}
            >
              {showInitialQuota ? <ChevronDown className="h-4 w-4" /> : <ChevronRight className="h-4 w-4" />}
              {t("userCreate.initialQuotaToggle")}
            </button>
            {showInitialQuota && (
              <div className="mt-3">
                <UserQuotaForm
                  clients={clientLites}
                  disabledClientNames={new Set()}
                  onSubmit={(v) => {
                    setPendingQuota(v);
                  }}
                  onCancel={() => {
                    setShowInitialQuota(false);
                    setPendingQuota(null);
                  }}
                />
                {pendingQuota && (
                  <p className="text-xs text-muted-foreground mt-2">
                    {t("userCreate.initialQuotaPending", { client: pendingQuota.client_name })}
                  </p>
                )}
              </div>
            )}
          </div>
          <div className="flex gap-2">
            <Button type="submit" disabled={create.isPending}>
              {create.isPending ? t("confirm.busy") : t("userCreate.submit")}
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
