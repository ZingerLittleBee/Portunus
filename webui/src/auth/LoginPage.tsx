import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Navigate, useLocation, useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";

import { apiFetch, ApiError } from "@/api/client";
import { clearToken, getToken, setToken } from "@/auth/token-store";
import { ME_QUERY_KEY } from "@/auth/AuthGate";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import type { Identity } from "@/lib/permissions";

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
  const [bearer, setBearer] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  if (getToken()) {
    return <Navigate to="/" replace />;
  }

  const search = new URLSearchParams(location.search);
  const expired = search.get("reason") === "session_expired";
  const next = safeNext(search.get("next"));

  async function handleSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setError(null);
    if (!bearer.trim()) {
      setError(t("login.tokenRequired"));
      return;
    }
    setBusy(true);
    setToken(bearer.trim());
    try {
      const id = await apiFetch<Identity>("/v1/users/me");
      queryClient.setQueryData(ME_QUERY_KEY, id);
      navigate(next, { replace: true });
    } catch (err) {
      clearToken();
      const code = err instanceof ApiError ? err.code : "unknown";
      setError(t("login.invalidToken", { code }));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex min-h-screen items-center justify-center bg-background p-4">
      <Card className="w-full max-w-md">
        <CardHeader>
          <CardTitle>{t("login.title")}</CardTitle>
          <CardDescription>{t("login.subtitle")}</CardDescription>
        </CardHeader>
        <CardContent>
          {expired && (
            <p className="mb-4 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-sm">
              {t("login.sessionExpired")}
            </p>
          )}
          <form onSubmit={handleSubmit} className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="bearer">{t("login.bearerLabel")}</Label>
              <Input
                id="bearer"
                type="password"
                autoComplete="off"
                spellCheck={false}
                value={bearer}
                onChange={(e) => setBearer(e.target.value)}
                placeholder="T006-…"
                disabled={busy}
                autoFocus
              />
            </div>
            {error && <p className="text-sm text-destructive">{error}</p>}
            <Button type="submit" className="w-full" disabled={busy}>
              {busy ? t("login.signingIn") : t("login.signIn")}
            </Button>
          </form>
        </CardContent>
      </Card>
    </div>
  );
}
