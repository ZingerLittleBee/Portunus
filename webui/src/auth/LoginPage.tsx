import { useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle } from "lucide-react";
import { useLocation, useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { useForm } from "react-hook-form";
import { z } from "zod";

import { ApiError } from "@/api/client";
import { changeOwnPassword, login } from "@/api/auth";
import { fetchIdentity, ME_QUERY_KEY } from "@/auth/identity";
import { clearLegacyToken } from "@/auth/token-store";
import { zResolver } from "@/lib/zod-resolver";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { FieldGroup } from "@/components/ui/field";
import { FormTextField } from "@/components/form/fields";

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

/// Required password-change step, rendered only after a temporary-password
/// login. Lives in its own component so its `useForm` initialises fresh on
/// mount — co-locating it with the login form's `useForm` and transitioning
/// via the login `handleSubmit` drops the first value written to the newly
/// mounted controls (a React 18 + RHF state-flush race).
function ChangePasswordForm({
  currentPassword,
  onChanged,
}: {
  currentPassword: string;
  onChanged: () => Promise<void>;
}) {
  const { t } = useTranslation();
  const [error, setError] = useState<string | null>(null);

  const schema = z
    .object({
      newPassword: z.string().min(1, t("login.newPasswordRequired")),
      newPasswordConfirm: z.string().min(1, t("login.newPasswordRequired")),
    })
    .refine((d) => d.newPassword === d.newPasswordConfirm, {
      message: t("login.passwordMismatch"),
      path: ["newPasswordConfirm"],
    });
  const form = useForm<z.infer<typeof schema>>({
    resolver: zResolver<z.infer<typeof schema>>(schema),
    defaultValues: { newPassword: "", newPasswordConfirm: "" },
  });

  async function onSubmit(values: z.infer<typeof schema>) {
    setError(null);
    try {
      await changeOwnPassword({
        current_password: currentPassword,
        new_password: values.newPassword,
        new_password_confirm: values.newPasswordConfirm,
      });
      await onChanged();
    } catch (err) {
      const code = err instanceof ApiError ? err.code : "unknown";
      setError(t("login.passwordChangeFailed", { code }));
    }
  }

  const busy = form.formState.isSubmitting;
  return (
    <form onSubmit={form.handleSubmit(onSubmit)}>
      <FieldGroup>
        <FormTextField
          control={form.control}
          name="newPassword"
          type="password"
          autoComplete="new-password"
          autoFocus
          label={t("login.newPasswordLabel")}
          disabled={busy}
        />
        <FormTextField
          control={form.control}
          name="newPasswordConfirm"
          type="password"
          autoComplete="new-password"
          label={t("login.newPasswordConfirmLabel")}
          disabled={busy}
        />
        {error && (
          <Alert variant="destructive">
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}
        <Button type="submit" className="w-full" disabled={busy}>
          {busy ? t("login.savingPassword") : t("login.savePassword")}
        </Button>
      </FieldGroup>
    </form>
  );
}

export function LoginPage() {
  const { t } = useTranslation();
  const location = useLocation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [mustChangePassword, setMustChangePassword] = useState(false);
  // The change-password step needs the password just used to log in.
  const [currentPassword, setCurrentPassword] = useState("");
  const [error, setError] = useState<string | null>(null);

  const loginSchema = z.object({
    userId: z.string().trim().min(1, t("login.credentialsRequired")),
    password: z.string().min(1, t("login.credentialsRequired")),
  });
  const loginForm = useForm<z.infer<typeof loginSchema>>({
    resolver: zResolver<z.infer<typeof loginSchema>>(loginSchema),
    defaultValues: { userId: "", password: "" },
  });

  const search = new URLSearchParams(location.search);
  const expired = search.get("reason") === "session_expired";
  const next = safeNext(search.get("next"));

  async function finishLogin() {
    const id = await fetchIdentity();
    queryClient.setQueryData(ME_QUERY_KEY, id);
    navigate(next, { replace: true });
  }

  async function onLogin(values: z.infer<typeof loginSchema>) {
    setError(null);
    try {
      clearLegacyToken();
      const response = await login({ user_id: values.userId.trim(), password: values.password });
      if (response.password_change_required) {
        setCurrentPassword(values.password);
        setMustChangePassword(true);
        return;
      }
      await finishLogin();
    } catch (err) {
      const code = err instanceof ApiError ? err.code : "unknown";
      setError(t("login.invalidCredentials", { code }));
    }
  }

  const busy = loginForm.formState.isSubmitting;

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
            <Alert className="mb-4 border-amber-500/50 [&>svg]:text-amber-500">
              <AlertTriangle />
              <AlertDescription className="text-amber-700 dark:text-amber-400">
                {t("login.sessionExpired")}
              </AlertDescription>
            </Alert>
          )}
          {mustChangePassword ? (
            <ChangePasswordForm currentPassword={currentPassword} onChanged={finishLogin} />
          ) : (
            <form onSubmit={loginForm.handleSubmit(onLogin)}>
              <FieldGroup>
                <FormTextField
                  control={loginForm.control}
                  name="userId"
                  autoComplete="username"
                  spellCheck={false}
                  autoFocus
                  label={t("login.userIdLabel")}
                  disabled={busy}
                />
                <FormTextField
                  control={loginForm.control}
                  name="password"
                  type="password"
                  autoComplete="current-password"
                  label={t("login.passwordLabel")}
                  disabled={busy}
                />
                {error && (
                  <Alert variant="destructive">
                    <AlertDescription>{error}</AlertDescription>
                  </Alert>
                )}
                <Button type="submit" className="w-full" disabled={busy}>
                  {busy ? t("login.signingIn") : t("login.signIn")}
                </Button>
              </FieldGroup>
            </form>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
