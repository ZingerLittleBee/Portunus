import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Clock, Copy, Terminal } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import type { ClientEnrollmentResponse } from "@/api/types";

const INSTALL_URL =
  "https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh";
const IMAGE = "ghcr.io/zingerlittlebee/portunus-client";

type Mode = "provision" | "reenroll";

interface Step {
  key: string;
  title: string;
  command: string;
}

function useCountdown(expiresAt: string) {
  const target = useMemo(() => new Date(expiresAt).getTime(), [expiresAt]);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1_000);
    return () => clearInterval(id);
  }, []);
  const ms = target - now;
  const expired = ms <= 0;
  const total = Math.max(0, Math.floor(ms / 1_000));
  const mm = String(Math.floor(total / 60)).padStart(2, "0");
  const ss = String(total % 60).padStart(2, "0");
  return { expired, remaining: `${mm}:${ss}` };
}

function CommandBlock({ testId, command }: { testId: string; command: string }) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);
  async function copy() {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
      setTimeout(() => setCopied(false), 2_000);
    } catch {
      /* ignore */
    }
  }
  return (
    <div className="space-y-2">
      <div className="flex items-center justify-end">
        <Button variant="outline" size="sm" onClick={copy}>
          {copied ? <Check className="mr-1 h-4 w-4" /> : <Copy className="mr-1 h-4 w-4" />}
          {copied ? t("clientProvision.guide.copied") : t("clientProvision.guide.copy")}
        </Button>
      </div>
      <ScrollArea className="rounded-md bg-muted">
        <pre data-testid={testId} className="p-3 text-xs leading-relaxed">
          {command}
        </pre>
      </ScrollArea>
    </div>
  );
}

function StepList({
  steps,
  startIndex,
}: {
  steps: Step[];
  startIndex: number;
}) {
  return (
    <ol className="space-y-4">
      {steps.map((s, i) => (
        <li key={s.key} className="space-y-2">
          <Label>
            {startIndex + i}. {s.title}
          </Label>
          <CommandBlock testId={`guide-step-${s.key}`} command={s.command} />
        </li>
      ))}
    </ol>
  );
}

export function EnrollmentInstallGuide({
  enrollment,
  mode,
}: {
  enrollment: ClientEnrollmentResponse;
  mode: Mode;
}) {
  const { t } = useTranslation();
  const { expired, remaining } = useCountdown(enrollment.expires_at);
  const reenroll = mode === "reenroll";

  const installStep: Step = {
    key: "shell-install",
    title: t("clientProvision.guide.stepInstall"),
    command: `curl -fsSL ${INSTALL_URL} | sh -s -- client`,
  };
  const shellSteps: Step[] = [
    installStep,
    {
      key: "shell-enroll",
      title: t("clientProvision.guide.stepEnroll"),
      command: enrollment.command,
    },
    {
      key: "shell-run",
      title: t("clientProvision.guide.stepRun"),
      command: "portunus-client",
    },
  ];
  const systemdSteps: Step[] = [
    {
      key: "systemd-install",
      title: t("clientProvision.guide.stepInstall"),
      command: `curl -fsSL ${INSTALL_URL} | sudo sh -s -- client --systemd`,
    },
    {
      key: "systemd-enroll",
      title: t("clientProvision.guide.stepEnrollSystemd"),
      command: `${enrollment.command} --out ./client.bundle.json
sudo install -o root -g portunus-client -m 0640 ./client.bundle.json /etc/portunus/client.bundle.json`,
    },
    {
      key: "systemd-enable",
      title: t("clientProvision.guide.stepEnableSystemd"),
      command: "sudo systemctl enable --now portunus-client",
    },
  ];
  const dockerSteps: Step[] = [
    {
      key: "docker-enroll",
      title: t("clientProvision.guide.stepEnrollDocker"),
      command: `docker run --rm --user "$(id -u):$(id -g)" -v "$PWD:/work" ${IMAGE} enroll '${enrollment.uri}' --out /work/client.bundle.json`,
    },
    {
      key: "docker-run",
      title: t("clientProvision.guide.stepRunDocker"),
      command: `docker run -d --name portunus-client --network host --user "$(id -u):$(id -g)" -v "$PWD/client.bundle.json:/etc/portunus/client.bundle.json:ro" ${IMAGE}`,
    },
  ];

  const visibleShell = reenroll ? shellSteps.slice(1) : shellSteps;
  const shellStart = reenroll ? 2 : 1;

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center justify-between gap-2">
          <span className="flex items-center gap-2">
            <Terminal className="h-5 w-5" />
            {t("clientProvision.guide.heading", { name: enrollment.client_name })}
          </span>
          <span
            className={`flex items-center gap-1 text-sm ${expired ? "text-destructive" : "text-muted-foreground"}`}
          >
            <Clock className="h-4 w-4" />
            {expired
              ? t("clientProvision.guide.expired")
              : t("clientProvision.guide.expiresIn", { remaining })}
          </span>
        </CardTitle>
      </CardHeader>
      <CardContent>
        {reenroll && (
          <p className="mb-4 text-xs text-muted-foreground">
            {t("clientProvision.guide.skipNote", { step: 2 })}
          </p>
        )}
        <Tabs defaultValue="shell">
          <TabsList>
            <TabsTrigger value="shell">{t("clientProvision.guide.tabShell")}</TabsTrigger>
            <TabsTrigger value="systemd">{t("clientProvision.guide.tabSystemd")}</TabsTrigger>
            <TabsTrigger value="docker">{t("clientProvision.guide.tabDocker")}</TabsTrigger>
          </TabsList>
          <TabsContent value="shell" forceMount className="pt-4">
            <StepList steps={visibleShell} startIndex={shellStart} />
          </TabsContent>
          <TabsContent value="systemd" forceMount className="pt-4">
            <StepList steps={systemdSteps} startIndex={1} />
          </TabsContent>
          <TabsContent value="docker" forceMount className="space-y-3 pt-4">
            <p className="text-xs text-muted-foreground">
              {t("clientProvision.guide.dockerNote")}
            </p>
            <StepList steps={dockerSteps} startIndex={1} />
          </TabsContent>
        </Tabs>
      </CardContent>
    </Card>
  );
}
