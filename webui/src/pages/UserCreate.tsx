import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";

import { useCreateUser } from "@/api/users";
import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

export function UserCreate() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const create = useCreateUser();
  const [userId, setUserId] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [initialPassword, setInitialPassword] = useState("");
  const [forcePasswordChange, setForcePasswordChange] = useState(true);
  const [error, setError] = useState<string | null>(null);

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
