import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";
import { Download, Copy, Check, Eye, EyeOff } from "lucide-react";

import { useProvisionClient } from "@/api/clients";
import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import type { CredentialBundle } from "@/api/types";

export function ClientProvision() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const provision = useProvisionClient();
  const [name, setName] = useState("");
  const [bundle, setBundle] = useState<CredentialBundle | null>(null);
  const [revealed, setRevealed] = useState(false);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function handleSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setError(null);
    try {
      const res = await provision.mutateAsync({ name });
      setBundle(res);
    } catch (err) {
      setError(err instanceof ApiError ? `${err.code}: ${err.message}` : (err as Error).message);
    }
  }

  function downloadBundle() {
    if (!bundle) return;
    const blob = new Blob([JSON.stringify(bundle, null, 2)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${bundle.client_name}.bundle.json`;
    document.body.appendChild(a);
    a.click();
    a.remove();
    URL.revokeObjectURL(url);
  }

  async function copyBundle() {
    if (!bundle) return;
    try {
      await navigator.clipboard.writeText(JSON.stringify(bundle, null, 2));
      setCopied(true);
      setTimeout(() => setCopied(false), 2_000);
    } catch {
      /* ignore */
    }
  }

  return (
    <div className="max-w-2xl space-y-6">
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
                disabled={!!bundle}
              />
            </div>
            {error && <p className="text-sm text-destructive">{error}</p>}
            {!bundle && (
              <div className="flex gap-2">
                <Button type="submit" disabled={provision.isPending}>
                  {provision.isPending ? t("confirm.busy") : t("clientProvision.submit")}
                </Button>
                <Button type="button" variant="outline" onClick={() => navigate(-1)}>
                  {t("confirm.cancel")}
                </Button>
              </div>
            )}
          </form>
        </CardContent>
      </Card>

      {bundle && (
        <Card>
          <CardHeader>
            <CardTitle>{t("clientProvision.bundleHeading")}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            <p className="text-sm text-muted-foreground">{t("clientProvision.bundleHint")}</p>
            <div className="flex gap-2">
              <Button onClick={downloadBundle}>
                <Download className="mr-1 h-4 w-4" />
                {t("clientProvision.download")}
              </Button>
              <Button variant="outline" onClick={copyBundle}>
                {copied ? <Check className="mr-1 h-4 w-4" /> : <Copy className="mr-1 h-4 w-4" />}
                {copied ? t("tokenReveal.copied") : t("clientProvision.copy")}
              </Button>
              <Button variant="ghost" onClick={() => setRevealed((r) => !r)}>
                {revealed ? <EyeOff className="mr-1 h-4 w-4" /> : <Eye className="mr-1 h-4 w-4" />}
                {revealed ? t("clientProvision.hide") : t("clientProvision.reveal")}
              </Button>
            </div>
            {revealed && (
              <pre className="overflow-x-auto rounded-md bg-muted p-3 text-xs">
                {JSON.stringify(bundle, null, 2)}
              </pre>
            )}
            <Button variant="link" onClick={() => navigate("/clients")}>
              {t("clientProvision.backToList")}
            </Button>
          </CardContent>
        </Card>
      )}
    </div>
  );
}
