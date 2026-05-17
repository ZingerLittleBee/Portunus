import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";
import { Check, Clock, Copy, Terminal } from "lucide-react";

import { useCreateClientEnrollment } from "@/api/clients";
import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { ScrollArea } from "@/components/ui/scroll-area";
import type { ClientEnrollmentResponse } from "@/api/types";

export function ClientProvision() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const enrollmentMutation = useCreateClientEnrollment();
  const [name, setName] = useState("");
  const [address, setAddress] = useState("");
  const [enrollment, setEnrollment] = useState<ClientEnrollmentResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function handleSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setError(null);
    try {
      const res = await enrollmentMutation.mutateAsync({ name, address });
      setEnrollment(res);
    } catch (err) {
      if (err instanceof ApiError) {
        setError(`${err.code}: ${err.message}`);
      } else if (err instanceof Error) {
        setError(err.message);
      } else {
        setError(String(err));
      }
    }
  }

  return (
    <div className="max-w-3xl space-y-6">
      <Card>
        <CardHeader>
          <CardTitle>{t("clientProvision.title")}</CardTitle>
        </CardHeader>
        <CardContent>
          <form onSubmit={handleSubmit} className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="name">{t("clients.name")}</Label>
              <Input
                id="name"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="client-a"
                required
                disabled={!!enrollment}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="address">{t("clientProvision.address")}</Label>
              <Input
                id="address"
                value={address}
                onChange={(e) => setAddress(e.target.value)}
                placeholder="68.77.201.69 or edge.example.com"
                required
                disabled={!!enrollment}
              />
              <p className="text-xs text-muted-foreground">
                {t("clientProvision.addressHint")}
              </p>
            </div>
            {error && <p className="text-sm text-destructive">{error}</p>}
            {!enrollment && (
              <div className="flex gap-2">
                <Button type="submit" disabled={enrollmentMutation.isPending}>
                  {enrollmentMutation.isPending ? t("confirm.busy") : t("clientProvision.submit")}
                </Button>
                <Button type="button" variant="outline" onClick={() => navigate(-1)}>
                  {t("confirm.cancel")}
                </Button>
              </div>
            )}
          </form>
        </CardContent>
      </Card>

      {enrollment && (
        <>
          <EnrollmentCommandCard enrollment={enrollment} />
          <Button variant="link" onClick={() => navigate("/clients")}>
            {t("clientProvision.backToList")}
          </Button>
        </>
      )}
    </div>
  );
}

function EnrollmentCommandCard({
  enrollment,
}: {
  enrollment: ClientEnrollmentResponse;
}) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);

  async function handleCopy() {
    try {
      await navigator.clipboard.writeText(enrollment.command);
      setCopied(true);
      setTimeout(() => setCopied(false), 2_000);
    } catch {
      /* ignore */
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Terminal className="h-5 w-5" />
          {t("clientProvision.enrollment.heading")}
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <p className="text-sm text-muted-foreground">
          {t("clientProvision.enrollment.hint", { name: enrollment.client_name })}
        </p>
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <Clock className="h-4 w-4" />
          <span>
            {t("clientProvision.enrollment.expiresAt", {
              expiresAt: new Date(enrollment.expires_at).toLocaleString(),
            })}
          </span>
        </div>
        <div className="space-y-2">
          <div className="flex items-center justify-between gap-3">
            <Label>{t("clientProvision.enrollment.commandLabel")}</Label>
            <Button variant="outline" size="sm" onClick={handleCopy}>
              {copied ? <Check className="mr-1 h-4 w-4" /> : <Copy className="mr-1 h-4 w-4" />}
              {copied ? t("clientProvision.enrollment.copied") : t("clientProvision.enrollment.copy")}
            </Button>
          </div>
          <ScrollArea className="rounded-md bg-muted">
            <pre className="p-3 text-xs leading-relaxed">{enrollment.command}</pre>
          </ScrollArea>
        </div>
      </CardContent>
    </Card>
  );
}
