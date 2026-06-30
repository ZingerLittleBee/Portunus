import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { useForm } from "react-hook-form";
import { z } from "zod";

import { zResolver } from "@/lib/zod-resolver";

import { ApiError } from "@/api/client";
import { login, onboard } from "@/api/auth";
import { fetchIdentity, ME_QUERY_KEY } from "@/auth/identity";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { FieldGroup } from "@/components/ui/field";
import { FormTextField } from "@/components/form/fields";

export function OnboardingPage() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [error, setError] = useState<string | null>(null);

  const schema = z
    .object({
      setupToken: z.string().trim().min(1, t("onboarding.required")),
      userId: z.string().trim().min(1, t("onboarding.required")),
      displayName: z.string().trim().min(1, t("onboarding.required")),
      password: z.string().min(1, t("onboarding.required")),
      passwordConfirm: z.string().min(1, t("onboarding.required")),
    })
    .refine((d) => d.password === d.passwordConfirm, {
      message: t("onboarding.passwordMismatch"),
      path: ["passwordConfirm"],
    });
  const form = useForm<z.infer<typeof schema>>({
    resolver: zResolver<z.infer<typeof schema>>(schema),
    defaultValues: {
      setupToken: "",
      userId: "admin",
      displayName: "Administrator",
      password: "",
      passwordConfirm: "",
    },
  });

  async function onSubmit(values: z.infer<typeof schema>) {
    setError(null);
    try {
      await onboard({
        setup_token: values.setupToken.trim(),
        user_id: values.userId.trim(),
        display_name: values.displayName.trim(),
        password: values.password,
        password_confirm: values.passwordConfirm,
      });
      await login({ user_id: values.userId.trim(), password: values.password });
      const identity = await fetchIdentity();
      queryClient.setQueryData(["auth", "status"], { onboarding_required: false });
      queryClient.setQueryData(ME_QUERY_KEY, identity);
      navigate("/", { replace: true });
    } catch (err) {
      const code = err instanceof ApiError ? err.code : "unknown";
      setError(t("onboarding.failed", { code }));
    }
  }

  const busy = form.formState.isSubmitting;

  return (
    <div className="flex min-h-screen items-center justify-center bg-background p-4">
      <Card className="w-full max-w-lg">
        <CardHeader>
          <CardTitle>{t("onboarding.title")}</CardTitle>
          <CardDescription>{t("onboarding.subtitle")}</CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={form.handleSubmit(onSubmit)}>
            <FieldGroup>
              <FormTextField
                control={form.control}
                name="setupToken"
                type="password"
                autoComplete="off"
                autoFocus
                label={t("onboarding.setupTokenLabel")}
                disabled={busy}
              />
              <FormTextField
                control={form.control}
                name="userId"
                autoComplete="username"
                label={t("onboarding.userIdLabel")}
                disabled={busy}
              />
              <FormTextField
                control={form.control}
                name="displayName"
                label={t("onboarding.displayNameLabel")}
                disabled={busy}
              />
              <FormTextField
                control={form.control}
                name="password"
                type="password"
                autoComplete="new-password"
                label={t("onboarding.passwordLabel")}
                disabled={busy}
              />
              <FormTextField
                control={form.control}
                name="passwordConfirm"
                type="password"
                autoComplete="new-password"
                label={t("onboarding.passwordConfirmLabel")}
                disabled={busy}
              />
              {error && (
                <Alert variant="destructive">
                  <AlertDescription>{error}</AlertDescription>
                </Alert>
              )}
              <Button type="submit" className="w-full" disabled={busy}>
                {busy ? t("onboarding.creating") : t("onboarding.create")}
              </Button>
            </FieldGroup>
          </form>
        </CardContent>
      </Card>
    </div>
  );
}
