import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { KeyRound, Trash2, RotateCcw } from "lucide-react";

import { useUser, useDeleteUser, useResetUserPassword } from "@/api/users";
import {
  credentialsKey,
  useCredentialsList,
  useIssueCredential,
  useRevokeCredential,
  useRotateCredential,
} from "@/api/credentials";
import { useGrantsList, useRevokeGrant } from "@/api/grants";
import { ME_QUERY_KEY, fetchIdentity } from "@/auth/AuthGate";
import { canSeeUserDetail, type Identity } from "@/lib/permissions";
import { PermissionDenied } from "@/components/PermissionDenied";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Separator } from "@/components/ui/separator";
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

function UserDetailInner({ userId, identity }: InnerProps) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const qc = useQueryClient();

  const user = useUser(userId);
  const credentials = useCredentialsList(userId);
  const grants = useGrantsList(userId);
  const issue = useIssueCredential(userId);
  const revokeCred = useRevokeCredential(userId);
  const revokeGrant = useRevokeGrant();
  const deleteUser = useDeleteUser();
  const resetPassword = useResetUserPassword(userId);

  const [issuedToken, setIssuedToken] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [rotateTarget, setRotateTarget] = useState<string | null>(null);
  const [resetOpen, setResetOpen] = useState(false);
  const [newPassword, setNewPassword] = useState("");
  const [temporaryPassword, setTemporaryPassword] = useState(true);
  const [keepApiTokens, setKeepApiTokens] = useState(false);
  const [resetError, setResetError] = useState<string | null>(null);

  const rotate = useRotateCredential(userId, rotateTarget ?? "");

  const isSelf = identity?.user_id === userId;

  return (
    <div className="space-y-6">
      <div className="flex items-start justify-between">
        <div>
          <h1 className="text-2xl font-semibold">
            {user.data?.display_name ?? userId}
            <span className="ml-2 font-mono text-sm text-muted-foreground">{userId}</span>
          </h1>
          {user.data && (
            <Badge className="mt-2" variant={user.data.role === "superadmin" ? "default" : "secondary"}>
              {user.data.role}
            </Badge>
          )}
        </div>
        {identity?.role === "superadmin" && (
          <div className="flex gap-2">
            <Button variant="outline" onClick={() => setResetOpen(true)}>
              {t("userDetail.resetPassword")}
            </Button>
            {!isSelf && (
              <Button variant="destructive" onClick={() => setConfirmDelete(true)}>
                <Trash2 className="mr-1 h-4 w-4" />
                {t("userDetail.delete")}
              </Button>
            )}
          </div>
        )}
      </div>

      <Card>
        <CardHeader className="flex-row items-center justify-between">
          <CardTitle className="flex items-center gap-2">
            <KeyRound className="h-4 w-4" /> {t("userDetail.credentials")}
          </CardTitle>
          <Button
            size="sm"
            onClick={async () => {
              const res = await issue.mutateAsync({});
              setIssuedToken(res.token);
            }}
            disabled={issue.isPending}
          >
            {t("userDetail.issueCredential")}
          </Button>
        </CardHeader>
        <CardContent className="space-y-2">
          {credentials.data && credentials.data.length > 0 ? (
            credentials.data.map((c) => (
              <div
                key={c.credential_id}
                className="flex items-center gap-3 rounded-md border p-3 text-sm"
              >
                <div className="flex-1">
                  <div className="font-mono text-xs">{c.credential_id}</div>
                  <div className="text-muted-foreground">
                    {c.label ?? "—"} · {c.status}
                  </div>
                </div>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={c.status !== "active"}
                  onClick={() => setRotateTarget(c.credential_id)}
                >
                  <RotateCcw className="mr-1 h-3 w-3" />
                  {t("userDetail.rotate")}
                </Button>
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={c.status !== "active"}
                  onClick={() => revokeCred.mutate(c.credential_id)}
                >
                  {t("userDetail.revoke")}
                </Button>
              </div>
            ))
          ) : (
            <p className="text-sm text-muted-foreground">{t("userDetail.noCredentials")}</p>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>{t("userDetail.grants")}</CardTitle>
        </CardHeader>
        <CardContent className="space-y-2">
          {grants.data && grants.data.length > 0 ? (
            grants.data.map((g) => (
              <div key={g.grant_id} className="flex items-center gap-3 rounded-md border p-3 text-sm">
                <div className="flex-1 font-mono">
                  {g.client}:{g.listen_port_start}–{g.listen_port_end} ({g.protocols.join("/")})
                </div>
                {identity?.role === "superadmin" && (
                  <Button variant="ghost" size="sm" onClick={() => revokeGrant.mutate(g.grant_id)}>
                    {t("userDetail.revokeGrant")}
                  </Button>
                )}
              </div>
            ))
          ) : (
            <p className="text-sm text-muted-foreground">{t("userDetail.noGrants")}</p>
          )}
        </CardContent>
      </Card>

      <Separator />

      <ConfirmDialog
        open={confirmDelete}
        onOpenChange={setConfirmDelete}
        destructive
        title={t("userDetail.deleteTitle")}
        description={t("userDetail.deleteBody", { id: userId })}
        dependents={[
          ...((credentials.data ?? []).map((c) => `credential ${c.credential_id}`)),
          ...((grants.data ?? []).map((g) => `grant ${g.grant_id}`)),
        ]}
        busy={deleteUser.isPending}
        onConfirm={async () => {
          await deleteUser.mutateAsync(userId);
          setConfirmDelete(false);
          navigate("/users");
        }}
      />

      <TokenRevealModal
        open={!!issuedToken}
        onOpenChange={async (open) => {
          if (!open) {
            setIssuedToken(null);
          }
        }}
        token={issuedToken ?? ""}
      />

      {rotateTarget && (
        <ConfirmDialog
          open={!!rotateTarget}
          onOpenChange={(open) => !open && setRotateTarget(null)}
          title={t("userDetail.rotateTitle")}
          description={t("userDetail.rotateBody")}
          confirmLabel={t("userDetail.rotateConfirm")}
          busy={rotate.isPending}
          onConfirm={async () => {
            const res = await rotate.mutateAsync({});
            await qc.invalidateQueries({ queryKey: credentialsKey(userId) });
            setRotateTarget(null);
            setIssuedToken(res.token);
          }}
        />
      )}

      <ConfirmDialog
        open={resetOpen}
        onOpenChange={(open) => {
          setResetOpen(open);
          if (!open) {
            setNewPassword("");
            setTemporaryPassword(true);
            setKeepApiTokens(false);
            setResetError(null);
          }
        }}
        title={t("userDetail.resetPasswordTitle")}
        description={t("userDetail.resetPasswordBody", { id: userId })}
        confirmLabel={t("userDetail.resetPasswordConfirm")}
        busy={resetPassword.isPending}
        onConfirm={async () => {
          setResetError(null);
          try {
            const res = await resetPassword.mutateAsync({
              ...(newPassword ? { new_password: newPassword } : {}),
              temporary_password: temporaryPassword,
              keep_api_tokens: keepApiTokens,
            });
            setResetOpen(false);
            if (res.temporary_password) {
              setIssuedToken(res.temporary_password);
            }
          } catch (err) {
            setResetError(err instanceof Error ? err.message : String(err));
          }
        }}
      >
        <div className="space-y-3">
          <div className="space-y-2">
            <Label htmlFor="reset-password">{t("userDetail.newPasswordOptional")}</Label>
            <Input
              id="reset-password"
              type="password"
              autoComplete="new-password"
              value={newPassword}
              onChange={(e) => setNewPassword(e.target.value)}
              placeholder={t("userDetail.generateTemporary")}
            />
          </div>
          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              checked={temporaryPassword}
              onChange={(e) => setTemporaryPassword(e.target.checked)}
            />
            {t("userDetail.requirePasswordChange")}
          </label>
          <label className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              checked={keepApiTokens}
              onChange={(e) => setKeepApiTokens(e.target.checked)}
            />
            {t("userDetail.keepApiTokens")}
          </label>
          {resetError && <p className="text-sm text-destructive">{resetError}</p>}
        </div>
      </ConfirmDialog>
    </div>
  );
}
