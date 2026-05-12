import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Download, Copy, Check, Eye, EyeOff, AlertTriangle } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { ScrollArea } from "@/components/ui/scroll-area";
import type { CredentialBundle } from "@/api/types";

interface CredentialBundleCardProps {
  bundle: CredentialBundle;
  /// Adjusts the warning copy: `reissue` emphasises that the previous
  /// token has been invalidated and live clients will reconnect.
  intent?: "provision" | "reissue";
}

export function CredentialBundleCard({ bundle, intent = "provision" }: CredentialBundleCardProps) {
  const { t } = useTranslation();
  const [revealed, setRevealed] = useState(false);
  const [copied, setCopied] = useState(false);

  function downloadBundle() {
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
    try {
      await navigator.clipboard.writeText(JSON.stringify(bundle, null, 2));
      setCopied(true);
      setTimeout(() => setCopied(false), 2_000);
    } catch {
      /* ignore */
    }
  }

  const hintKey =
    intent === "reissue" ? "clientProvision.reissueHint" : "clientProvision.bundleHint";

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("clientProvision.bundleHeading")}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        <div className="flex items-start gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 p-3 text-sm">
          <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-amber-600 dark:text-amber-400" />
          <p>{t(hintKey)}</p>
        </div>
        <div className="flex flex-wrap gap-2">
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
          <ScrollArea className="max-h-96 rounded-md bg-muted">
            <pre className="p-3 text-xs leading-relaxed">{JSON.stringify(bundle, null, 2)}</pre>
          </ScrollArea>
        )}
      </CardContent>
    </Card>
  );
}
