import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";

import { ApiError } from "@/api/client";
import { login, onboard } from "@/api/auth";
import { fetchIdentity, ME_QUERY_KEY } from "@/auth/AuthGate";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

export function OnboardingPage() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [setupToken, setSetupToken] = useState("");
  const [userId, setUserId] = useState("admin");
  const [displayName, setDisplayName] = useState("Administrator");
  const [password, setPassword] = useState("");
  const [passwordConfirm, setPasswordConfirm] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function handleSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setError(null);
    if (!setupToken.trim() || !userId.trim() || !displayName.trim() || !password) {
      setError(t("onboarding.required"));
      return;
    }
    if (password !== passwordConfirm) {
      setError(t("onboarding.passwordMismatch"));
      return;
    }
    setBusy(true);
    try {
      await onboard({
        setup_token: setupToken.trim(),
        user_id: userId.trim(),
        display_name: displayName.trim(),
        password,
        password_confirm: passwordConfirm,
      });
      await login({ user_id: userId.trim(), password });
      const identity = await fetchIdentity();
      queryClient.setQueryData(["auth", "status"], { onboarding_required: false });
      queryClient.setQueryData(ME_QUERY_KEY, identity);
      navigate("/", { replace: true });
    } catch (err) {
      const code = err instanceof ApiError ? err.code : "unknown";
      setError(t("onboarding.failed", { code }));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex min-h-screen items-center justify-center bg-background p-4">
      <Card className="w-full max-w-lg">
        <CardHeader>
          <CardTitle>{t("onboarding.title")}</CardTitle>
          <CardDescription>{t("onboarding.subtitle")}</CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={handleSubmit} className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="setup-token">{t("onboarding.setupTokenLabel")}</Label>
              <Input
                id="setup-token"
                type="password"
                autoComplete="off"
                value={setupToken}
                onChange={(e) => setSetupToken(e.target.value)}
                disabled={busy}
                autoFocus
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="onboarding-user-id">{t("onboarding.userIdLabel")}</Label>
              <Input
                id="onboarding-user-id"
                autoComplete="username"
                value={userId}
                onChange={(e) => setUserId(e.target.value)}
                disabled={busy}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="display-name">{t("onboarding.displayNameLabel")}</Label>
              <Input
                id="display-name"
                value={displayName}
                onChange={(e) => setDisplayName(e.target.value)}
                disabled={busy}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="onboarding-password">{t("onboarding.passwordLabel")}</Label>
              <Input
                id="onboarding-password"
                type="password"
                autoComplete="new-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                disabled={busy}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="onboarding-password-confirm">
                {t("onboarding.passwordConfirmLabel")}
              </Label>
              <Input
                id="onboarding-password-confirm"
                type="password"
                autoComplete="new-password"
                value={passwordConfirm}
                onChange={(e) => setPasswordConfirm(e.target.value)}
                disabled={busy}
              />
            </div>
            {error && <p className="text-sm text-destructive">{error}</p>}
            <Button type="submit" className="w-full" disabled={busy}>
              {busy ? t("onboarding.creating") : t("onboarding.create")}
            </Button>
          </form>
        </CardContent>
      </Card>
    </div>
  );
}
