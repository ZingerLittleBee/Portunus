import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useLocation, useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";

import { ApiError } from "@/api/client";
import { changeOwnPassword, login } from "@/api/auth";
import { fetchIdentity, ME_QUERY_KEY } from "@/auth/AuthGate";
import { clearLegacyToken } from "@/auth/token-store";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

function safeNext(raw: string | null): string {
  if (!raw) return "/";
  try {
    const decoded = decodeURIComponent(raw);
    if (decoded.startsWith("/") && !decoded.startsWith("//")) return decoded;
  } catch {
    /* fall through */
  }
  return "/";
}

export function LoginPage() {
  const { t } = useTranslation();
  const location = useLocation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [userId, setUserId] = useState("");
  const [password, setPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [newPasswordConfirm, setNewPasswordConfirm] = useState("");
  const [mustChangePassword, setMustChangePassword] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const search = new URLSearchParams(location.search);
  const expired = search.get("reason") === "session_expired";
  const next = safeNext(search.get("next"));

  async function finishLogin() {
    const id = await fetchIdentity();
    queryClient.setQueryData(ME_QUERY_KEY, id);
    navigate(next, { replace: true });
  }

  async function handleSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setError(null);
    if (!userId.trim() || !password) {
      setError(t("login.credentialsRequired"));
      return;
    }
    setBusy(true);
    try {
      clearLegacyToken();
      const response = await login({ user_id: userId.trim(), password });
      if (response.password_change_required) {
        setMustChangePassword(true);
        return;
      }
      await finishLogin();
    } catch (err) {
      const code = err instanceof ApiError ? err.code : "unknown";
      setError(t("login.invalidCredentials", { code }));
    } finally {
      setBusy(false);
    }
  }

  async function handlePasswordChange(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setError(null);
    if (!newPassword || !newPasswordConfirm) {
      setError(t("login.newPasswordRequired"));
      return;
    }
    if (newPassword !== newPasswordConfirm) {
      setError(t("login.passwordMismatch"));
      return;
    }
    setBusy(true);
    try {
      await changeOwnPassword({
        current_password: password,
        new_password: newPassword,
        new_password_confirm: newPasswordConfirm,
      });
      await finishLogin();
    } catch (err) {
      const code = err instanceof ApiError ? err.code : "unknown";
      setError(t("login.passwordChangeFailed", { code }));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex min-h-screen items-center justify-center bg-background p-4">
      <Card className="w-full max-w-md">
        <CardHeader>
          <CardTitle>{mustChangePassword ? t("login.changeTitle") : t("login.title")}</CardTitle>
          <CardDescription>
            {mustChangePassword ? t("login.changeSubtitle") : t("login.subtitle")}
          </CardDescription>
        </CardHeader>
        <CardContent>
          {expired && !mustChangePassword && (
            <p className="mb-4 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-sm">
              {t("login.sessionExpired")}
            </p>
          )}
          {mustChangePassword ? (
            <form onSubmit={handlePasswordChange} className="space-y-4">
              <div className="space-y-2">
                <Label htmlFor="new-password">{t("login.newPasswordLabel")}</Label>
                <Input
                  id="new-password"
                  type="password"
                  autoComplete="new-password"
                  value={newPassword}
                  onChange={(e) => setNewPassword(e.target.value)}
                  disabled={busy}
                  autoFocus
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="new-password-confirm">{t("login.newPasswordConfirmLabel")}</Label>
                <Input
                  id="new-password-confirm"
                  type="password"
                  autoComplete="new-password"
                  value={newPasswordConfirm}
                  onChange={(e) => setNewPasswordConfirm(e.target.value)}
                  disabled={busy}
                />
              </div>
              {error && <p className="text-sm text-destructive">{error}</p>}
              <Button type="submit" className="w-full" disabled={busy}>
                {busy ? t("login.savingPassword") : t("login.savePassword")}
              </Button>
            </form>
          ) : (
            <form onSubmit={handleSubmit} className="space-y-4">
              <div className="space-y-2">
                <Label htmlFor="user-id">{t("login.userIdLabel")}</Label>
                <Input
                  id="user-id"
                  autoComplete="username"
                  spellCheck={false}
                  value={userId}
                  onChange={(e) => setUserId(e.target.value)}
                  disabled={busy}
                  autoFocus
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="password">{t("login.passwordLabel")}</Label>
                <Input
                  id="password"
                  type="password"
                  autoComplete="current-password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  disabled={busy}
                />
              </div>
              {error && <p className="text-sm text-destructive">{error}</p>}
              <Button type="submit" className="w-full" disabled={busy}>
                {busy ? t("login.signingIn") : t("login.signIn")}
              </Button>
            </form>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
