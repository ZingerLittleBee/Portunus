import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";
import { Download, Copy, Check, Eye, EyeOff } from "lucide-react";

import { useProvisionClient } from "@/api/clients";
import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
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
              <ScrollArea className="max-h-96 rounded-md bg-muted">
                <pre className="p-3 text-xs leading-relaxed">
                  {JSON.stringify(bundle, null, 2)}
                </pre>
              </ScrollArea>
            )}
            <Button variant="link" onClick={() => navigate("/clients")}>
              {t("clientProvision.backToList")}
            </Button>
          </CardContent>
        </Card>
      )}

      {bundle && <ClientInstallSteps bundle={bundle} />}
    </div>
  );
}

interface ClientInstallStepsProps {
  bundle: CredentialBundle;
}

// Base64-encode the bundle JSON. The base64 alphabet (A-Za-z0-9+/=) is
// safe to splice inside single-quoted shell args without any escaping.
function encodeBundle(bundle: CredentialBundle): string {
  const json = JSON.stringify(bundle, null, 2);
  const bytes = new TextEncoder().encode(json);
  let binary = "";
  for (let i = 0; i < bytes.byteLength; i += 1) {
    binary += String.fromCharCode(bytes[i] as number);
  }
  return btoa(binary);
}

function ClientInstallSteps({ bundle }: ClientInstallStepsProps) {
  const { t } = useTranslation();
  const b64 = useMemo(() => encodeBundle(bundle), [bundle]);
  const version = __APP_VERSION__;

  const dockerSteps: Step[] = [
    {
      title: t("clientProvision.install.docker.step1Title"),
      hint: t("clientProvision.install.docker.step1Hint"),
      code: `mkdir -p ./portunus
echo '${b64}' | base64 -d > ./portunus/bundle.json
chmod 600 ./portunus/bundle.json`,
    },
    {
      title: t("clientProvision.install.docker.step2Title"),
      hint: t("clientProvision.install.docker.step2Hint"),
      code: `docker run -d --name portunus-client \\
  --restart unless-stopped \\
  --network host \\
  -v "$(pwd)/portunus/bundle.json:/bundle.json:ro" \\
  ghcr.io/zingerlittlebee/portunus-client:v${version} \\
  --bundle /bundle.json`,
    },
    {
      title: t("clientProvision.install.docker.step3Title"),
      hint: t("clientProvision.install.docker.step3Hint"),
      code: `docker logs -f portunus-client`,
    },
  ];

  const linuxSteps: Step[] = [
    {
      title: t("clientProvision.install.linux.step1Title"),
      hint: t("clientProvision.install.linux.step1Hint"),
      code: `ARCH=$(uname -m); case "$ARCH" in aarch64|arm64) ARCH=aarch64 ;; *) ARCH=x86_64 ;; esac
VER=${version}
curl -fsSL -o /tmp/portunus.tar.gz \\
  "https://github.com/ZingerLittleBee/Portunus/releases/download/v\${VER}/portunus-\${VER}-\${ARCH}-unknown-linux-gnu.tar.gz"
tar -xzf /tmp/portunus.tar.gz -C /tmp
sudo install -m 0755 "/tmp/portunus-\${VER}-\${ARCH}-unknown-linux-gnu/portunus-client" /usr/local/bin/`,
    },
    {
      title: t("clientProvision.install.linux.step2Title"),
      hint: t("clientProvision.install.linux.step2Hint"),
      code: `sudo useradd --system --no-create-home --shell /usr/sbin/nologin portunus-client 2>/dev/null || true
sudo install -d -m 0750 /etc/portunus
echo '${b64}' | base64 -d | \\
  sudo install -o root -g portunus-client -m 0640 /dev/stdin /etc/portunus/client.bundle.json`,
    },
    {
      title: t("clientProvision.install.linux.step3Title"),
      hint: t("clientProvision.install.linux.step3Hint"),
      code: `sudo tee /etc/systemd/system/portunus-client.service >/dev/null <<'EOF'
[Unit]
Description=Portunus edge client
After=network-online.target
Wants=network-online.target

[Service]
Type=exec
User=portunus-client
Group=portunus-client
ExecStart=/usr/local/bin/portunus-client --bundle /etc/portunus/client.bundle.json
Restart=on-failure
RestartSec=5
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
EOF
sudo systemctl daemon-reload
sudo systemctl enable --now portunus-client`,
    },
    {
      title: t("clientProvision.install.linux.step4Title"),
      hint: t("clientProvision.install.linux.step4Hint"),
      code: `sudo systemctl status portunus-client
sudo journalctl -u portunus-client -f`,
    },
  ];

  const manualSteps: Step[] = [
    {
      title: t("clientProvision.install.manual.step1Title"),
      code: `echo '${b64}' | base64 -d > bundle.json`,
    },
    {
      title: t("clientProvision.install.manual.step2Title"),
      hint: t("clientProvision.install.manual.step2Hint"),
      code: `portunus-client --bundle bundle.json`,
    },
  ];

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("clientProvision.install.heading")}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <p className="text-sm text-muted-foreground">{t("clientProvision.install.hint")}</p>
        <Tabs defaultValue="docker">
          <TabsList>
            <TabsTrigger value="docker">{t("clientProvision.install.tabDocker")}</TabsTrigger>
            <TabsTrigger value="linux">{t("clientProvision.install.tabLinux")}</TabsTrigger>
            <TabsTrigger value="manual">{t("clientProvision.install.tabManual")}</TabsTrigger>
          </TabsList>
          <TabsContent value="docker">
            <StepList steps={dockerSteps} />
          </TabsContent>
          <TabsContent value="linux">
            <StepList steps={linuxSteps} />
          </TabsContent>
          <TabsContent value="manual">
            <StepList steps={manualSteps} />
          </TabsContent>
        </Tabs>
      </CardContent>
    </Card>
  );
}

interface Step {
  title: string;
  hint?: string;
  code: string;
}

function StepList({ steps }: { steps: Step[] }) {
  return (
    <ol className="space-y-4">
      {steps.map((step, idx) => (
        <StepItem key={idx} step={step} n={idx + 1} />
      ))}
    </ol>
  );
}

function StepItem({ step, n }: { step: Step; n: number }) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);

  async function handleCopy() {
    try {
      await navigator.clipboard.writeText(step.code);
      setCopied(true);
      setTimeout(() => setCopied(false), 2_000);
    } catch {
      /* ignore */
    }
  }

  return (
    <li className="space-y-2">
      <div className="flex items-baseline justify-between gap-2">
        <div>
          <span className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
            {t("clientProvision.install.stepLabel", { n })}
          </span>
          <p className="text-sm font-medium">{step.title}</p>
        </div>
        <Button variant="outline" size="sm" onClick={handleCopy}>
          {copied ? <Check className="mr-1 h-4 w-4" /> : <Copy className="mr-1 h-4 w-4" />}
          {copied ? t("clientProvision.install.copied") : t("clientProvision.install.copy")}
        </Button>
      </div>
      {step.hint && <p className="text-xs text-muted-foreground">{step.hint}</p>}
      <ScrollArea className="rounded-md bg-muted">
        <pre className="p-3 text-xs leading-relaxed">{step.code}</pre>
      </ScrollArea>
    </li>
  );
}
