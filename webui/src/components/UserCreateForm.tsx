import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useForm } from "react-hook-form";
import { z } from "zod";
import { ChevronDown, ChevronRight } from "lucide-react";
import { toast } from "sonner";

import { useCreateUser } from "@/api/users";
import { useCreateAccessEntry } from "@/api/access-entries";
import { useClientsList } from "@/api/clients";
import { formatApiError } from "@/api/client";
import { zResolver } from "@/lib/zod-resolver";
import { Button } from "@/components/ui/button";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { FieldGroup } from "@/components/ui/field";
import { FormTextField, FormCheckboxField } from "@/components/form/fields";
import { UserQuotaForm, type UserQuotaFormSubmitValue } from "@/components/UserQuota/UserQuotaForm";

interface UserCreateFormProps {
  /** Called after the user (and optional initial quota) is created. */
  onSuccess: (userId: string) => void;
  /** Called when the user dismisses the form without creating anything. */
  onCancel: () => void;
}

export function UserCreateForm({ onSuccess, onCancel }: UserCreateFormProps) {
  const { t } = useTranslation();
  const create = useCreateUser();
  const [error, setError] = useState<string | null>(null);

  const [showInitialQuota, setShowInitialQuota] = useState(false);
  const [pendingQuota, setPendingQuota] = useState<UserQuotaFormSubmitValue | null>(null);
  const clientsQ = useClientsList();
  const clientLites = (clientsQ.data ?? []).map((c) => ({
    client_id: c.client_id,
    client_name: c.client_name,
    connected: c.connected,
  }));

  const schema = z.object({
    user_id: z.string().regex(/^[a-z][a-z0-9-_]*$/, t("userCreate.invalidId")),
    display_name: z.string().min(1, t("userCreate.displayNameRequired")),
    initial_password: z.string().min(12, t("userCreate.passwordRequired")),
    force_password_change: z.boolean(),
  });
  const form = useForm<z.infer<typeof schema>>({
    resolver: zResolver<z.infer<typeof schema>>(schema),
    defaultValues: {
      user_id: "",
      display_name: "",
      initial_password: "",
      force_password_change: true,
    },
  });
  const userId = form.watch("user_id");
  const createEntry = useCreateAccessEntry(userId);

  async function onSubmit(values: z.infer<typeof schema>) {
    setError(null);
    try {
      const res = await create.mutateAsync({
        user_id: values.user_id,
        display_name: values.display_name,
        initial_password: values.initial_password,
        password_change_required: values.force_password_change,
      });
      if (pendingQuota && res.user_id) {
        try {
          await createEntry.mutateAsync({
            user_id: res.user_id,
            client_id: pendingQuota.client_id,
            client_name: pendingQuota.client_name,
            listen_port_start: pendingQuota.listen_port_start,
            listen_port_end: pendingQuota.listen_port_end,
            protocols: pendingQuota.protocols,
            ...(pendingQuota.cap !== undefined ? { cap: pendingQuota.cap } : {}),
          });
          toast.success(t("userQuota.toast.created", { client: pendingQuota.client_name }));
        } catch (err) {
          toast.warning(`${t("userQuota.toast.createFailed")}: ${formatApiError(err)}`);
        }
      }
      onSuccess(res.user_id);
    } catch (err) {
      setError(formatApiError(err));
    }
  }

  return (
    <form onSubmit={form.handleSubmit(onSubmit)}>
      <FieldGroup>
        <FormTextField
          control={form.control}
          name="user_id"
          label={t("users.id")}
          placeholder="alice"
          autoComplete="off"
          description={t("userCreate.idHint")}
          disabled={create.isPending}
        />
        <FormTextField
          control={form.control}
          name="display_name"
          label={t("users.displayName")}
          placeholder="Alice — payments"
          disabled={create.isPending}
        />
        <FormTextField
          control={form.control}
          name="initial_password"
          type="password"
          autoComplete="new-password"
          label={t("userCreate.initialPassword")}
          description={t("userCreate.initialPasswordHint")}
          disabled={create.isPending}
        />
        <FormCheckboxField
          control={form.control}
          name="force_password_change"
          label={t("userCreate.forcePasswordChange")}
          disabled={create.isPending}
        />
        {error && (
          <Alert variant="destructive">
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}
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
                disabledClientIds={new Set()}
                nested
                framed={false}
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
          <Button type="button" variant="outline" onClick={onCancel}>
            {t("confirm.cancel")}
          </Button>
        </div>
      </FieldGroup>
    </form>
  );
}
