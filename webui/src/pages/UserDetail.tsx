import { useReducer } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { Trash2 } from "lucide-react";

import { useUser, useDeleteUser, useResetUserPassword } from "@/api/users";
import { useAccessEntries } from "@/api/access-entries";
import { useClientsList } from "@/api/clients";
import { useUserQuotas, usePatchQuota } from "@/api/quotas";
import { UserQuotaTable } from "@/components/UserQuota/UserQuotaTable";
import { ExhaustedBanner } from "@/components/Traffic/ExhaustedBanner";
import { TrafficPanel } from "@/components/Traffic/TrafficPanel";
import { ME_QUERY_KEY, fetchIdentity } from "@/auth/identity";
import { canSeeUserDetail, type Identity } from "@/lib/permissions";
import { PermissionDenied } from "@/components/PermissionDenied";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Field, FieldContent, FieldGroup, FieldLabel } from "@/components/ui/field";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { TokenRevealModal } from "@/components/TokenRevealModal";

export function UserDetail() {
  const params = useParams<{ userId: string }>();
  const userId = params.userId ?? "";
  const { data: identity } = useQuery({
    queryKey: ME_QUERY_KEY,
    queryFn: fetchIdentity,
    staleTime: 60_000,
  });

  // T046: client-side gate — render PermissionDenied BEFORE firing the
  // user-detail GET if the caller is a non-superadmin viewing someone else.
  if (!canSeeUserDetail(identity, userId)) {
    return <PermissionDenied />;
  }
  return <UserDetailInner userId={userId} identity={identity ?? null} />;
}

interface InnerProps {
  userId: string;
  identity: Identity | null;
}

interface UserDetailUiState {
  issuedToken: string | null;
  confirmDelete: boolean;
  resetOpen: boolean;
  newPassword: string;
  temporaryPassword: boolean;
  resetError: string | null;
}

type UserDetailUiAction =
  | { type: "issued-token"; token: string | null }
  | { type: "confirm-delete"; open: boolean }
  | { type: "reset-open"; open: boolean }
  | { type: "new-password"; value: string }
  | { type: "temporary-password"; value: boolean }
  | { type: "reset-error"; message: string | null };

const initialUserDetailUiState: UserDetailUiState = {
  issuedToken: null,
  confirmDelete: false,
  resetOpen: false,
  newPassword: "",
  temporaryPassword: true,
  resetError: null,
};

function userDetailUiReducer(
  state: UserDetailUiState,
  action: UserDetailUiAction,
): UserDetailUiState {
  switch (action.type) {
    case "issued-token":
      return { ...state, issuedToken: action.token };
    case "confirm-delete":
      return { ...state, confirmDelete: action.open };
    case "reset-open":
      return action.open
        ? { ...state, resetOpen: true }
        : {
            ...state,
            resetOpen: false,
            newPassword: "",
            temporaryPassword: true,
            resetError: null,
          };
    case "new-password":
      return { ...state, newPassword: action.value };
    case "temporary-password":
      return { ...state, temporaryPassword: action.value };
    case "reset-error":
      return { ...state, resetError: action.message };
  }
}

function UserDetailInner({ userId, identity }: InnerProps) {
  const { t } = useTranslation();
  const navigate = useNavigate();

  const user = useUser(userId);
  const accessEntries = useAccessEntries(userId);
  const userQuotas = useUserQuotas(userId);
  const patchQuota = usePatchQuota(userId);
  const exhaustedQuotas = (userQuotas.data ?? []).filter((q) => q.exhausted);
  const clientsQ = useClientsList();
  // 015-client-stable-id (US3): carry the stable id so quota/cap edits can
  // address the still-name-displaying client by id in the URL.
  const clientLites = (clientsQ.data ?? []).map((c) => ({
    client_id: c.client_id,
    client_name: c.client_name,
    connected: c.connected,
  }));
  // Resolve display name → stable id for quota mutations whose source row
  // (MonthlyQuotaView) only carries the name. First match wins on a
  // duplicate display name.
  const clientIdByName = new Map<string, string>();
  for (const c of clientsQ.data ?? []) {
    // TODO(015): ambiguous display name — first provisioned client wins.
    if (!clientIdByName.has(c.client_name)) {
      clientIdByName.set(c.client_name, c.client_id);
    }
  }
  const isSuperadmin = identity?.role === "superadmin";
  const deleteUser = useDeleteUser();
  const resetPassword = useResetUserPassword(userId);

  const [ui, dispatchUi] = useReducer(userDetailUiReducer, initialUserDetailUiState);

  const isSelf = identity?.user_id === userId;

  return (
    <div className="flex flex-col gap-6">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0">
          <h1 className="text-2xl font-semibold">
            {user.data?.display_name ?? userId}
            <span className="ml-2 break-all font-mono text-sm text-muted-foreground">{userId}</span>
          </h1>
          {user.data && (
            <Badge className="mt-2" variant={user.data.role === "superadmin" ? "default" : "secondary"}>
              {user.data.role}
            </Badge>
          )}
        </div>
        {identity?.role === "superadmin" && (
          <div className="flex flex-col gap-2 sm:flex-row">
            <Button variant="outline" onClick={() => dispatchUi({ type: "reset-open", open: true })}>
              {t("userDetail.resetPassword")}
            </Button>
            {!isSelf && (
              <Button variant="destructive" onClick={() => dispatchUi({ type: "confirm-delete", open: true })}>
                <Trash2 className="mr-1 h-4 w-4" />
                {t("userDetail.delete")}
              </Button>
            )}
          </div>
        )}
      </div>

      <ExhaustedBanner
        exhausted={exhaustedQuotas}
        onClearUsage={
          isSuperadmin
            ? (q) =>
                patchQuota.mutate({
                  client_id: clientIdByName.get(q.client_name) ?? "",
                  body: { clear_period_usage: true },
                })
            : undefined
        }
      />

      <section className="flex flex-col gap-3">
        <h2 className="text-base font-semibold">{t("userQuota.sectionTitle")}</h2>
        {accessEntries.isLoading ? (
          <p className="text-sm text-muted-foreground">{t("confirm.busy")}</p>
        ) : (
          <UserQuotaTable
            userId={userId}
            entries={accessEntries.data ?? []}
            clients={clientLites}
            readOnly={!isSuperadmin}
          />
        )}
      </section>

      <section className="flex flex-col gap-3">
        <h2 className="text-base font-semibold">{t("traffic.tab")}</h2>
        <TrafficPanel userId={userId} framed={false} />
      </section>

      <Separator />

      <ConfirmDialog
        open={ui.confirmDelete}
        onOpenChange={(open) => dispatchUi({ type: "confirm-delete", open })}
        destructive
        title={t("userDetail.deleteTitle")}
        description={t("userDetail.deleteBody", { id: userId })}
        dependents={[
          ...((accessEntries.data ?? []).map((e) => `quota ${e.client_name}`)),
        ]}
        busy={deleteUser.isPending}
        onConfirm={async () => {
          await deleteUser.mutateAsync(userId);
          dispatchUi({ type: "confirm-delete", open: false });
          navigate("/users");
        }}
      />

      <TokenRevealModal
        open={!!ui.issuedToken}
        onOpenChange={async (open) => {
          if (!open) {
            dispatchUi({ type: "issued-token", token: null });
          }
        }}
        token={ui.issuedToken ?? ""}
        title={t("tokenReveal.passwordTitle")}
        description={t("tokenReveal.passwordDescription")}
      />

      <ConfirmDialog
        open={ui.resetOpen}
        onOpenChange={(open) => dispatchUi({ type: "reset-open", open })}
        title={t("userDetail.resetPasswordTitle")}
        description={t("userDetail.resetPasswordBody", { id: userId })}
        confirmLabel={t("userDetail.resetPasswordConfirm")}
        busy={resetPassword.isPending}
        onConfirm={async () => {
          dispatchUi({ type: "reset-error", message: null });
          const explicitPassword = ui.newPassword.length > 0;
          try {
            const res = await resetPassword.mutateAsync({
              ...(explicitPassword ? { new_password: ui.newPassword } : {}),
              temporary_password: explicitPassword ? ui.temporaryPassword : true,
            });
            dispatchUi({ type: "reset-open", open: false });
            if (res.temporary_password) {
              dispatchUi({ type: "issued-token", token: res.temporary_password });
            }
          } catch (err) {
            dispatchUi({
              type: "reset-error",
              message: err instanceof Error ? err.message : String(err),
            });
          }
        }}
      >
        <FieldGroup>
          <Field>
            <FieldLabel htmlFor="reset-password">{t("userDetail.newPasswordOptional")}</FieldLabel>
            <Input
              id="reset-password"
              type="password"
              autoComplete="new-password"
              value={ui.newPassword}
              onChange={(e) => dispatchUi({ type: "new-password", value: e.target.value })}
              placeholder={t("userDetail.generateTemporary")}
            />
          </Field>
          <Field orientation="horizontal">
            <Checkbox
              id="require-password-change"
              checked={ui.newPassword.length === 0 || ui.temporaryPassword}
              disabled={ui.newPassword.length === 0}
              onCheckedChange={(checked) => dispatchUi({ type: "temporary-password", value: checked === true })}
            />
            <FieldContent>
              <FieldLabel htmlFor="require-password-change" className="font-normal">
                {t("userDetail.requirePasswordChange")}
              </FieldLabel>
            </FieldContent>
          </Field>
          {ui.resetError && (
            <Alert variant="destructive">
              <AlertDescription>{ui.resetError}</AlertDescription>
            </Alert>
          )}
        </FieldGroup>
      </ConfirmDialog>
    </div>
  );
}
