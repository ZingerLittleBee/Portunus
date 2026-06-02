import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Clock, Copy, Terminal } from "lucide-react";

import { cn } from "@/lib/cn";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
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
    <div className="relative min-w-0">
      <pre
        data-testid={testId}
        className="overflow-hidden whitespace-pre-wrap break-all rounded-md bg-muted p-3 pr-24 font-mono text-xs leading-relaxed"
      >
        {command}
      </pre>
      <Button
        variant="outline"
        size="sm"
        onClick={copy}
        className="absolute right-1.5 top-1.5 h-7 px-2"
      >
        {copied ? <Check className="mr-1 h-3.5 w-3.5" /> : <Copy className="mr-1 h-3.5 w-3.5" />}
        {copied ? t("clientProvision.guide.copied") : t("clientProvision.guide.copy")}
      </Button>
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
        <li key={s.key} className="min-w-0 space-y-2">
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
  framed = true,
}: {
  enrollment: ClientEnrollmentResponse;
  mode: Mode;
  /** Wrap in a Card (standalone panel). Set false when already inside a
   * dialog or another card so we don't nest framed surfaces. */
  framed?: boolean;
}) {
  const { t } = useTranslation();
  const { expired, remaining } = useCountdown(enrollment.expires_at);
  const reenroll = mode === "reenroll";

  // Both deploy forms install via `install.sh`. The binary form lays down
  // the release binary + a hardened systemd/OpenRC service (the default);
  // Docker runs the published image directly (install.sh's `--deploy docker`
  // is server/standalone-only — it emits a server-shaped compose).
  const binarySteps: Step[] = [
    {
      key: "binary-install",
      title: t("clientProvision.guide.stepInstall"),
      command: `curl -fsSL ${INSTALL_URL} | sh -s -- client`,
    },
    {
      key: "binary-enroll",
      title: t("clientProvision.guide.stepEnrollSystemd"),
      command: `${enrollment.command} --out ./client.bundle.json
sudo install -o root -g portunus-client -m 0640 ./client.bundle.json /etc/portunus/client.bundle.json`,
    },
    {
      key: "binary-enable",
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

  const visibleBinary = reenroll ? binarySteps.slice(1) : binarySteps;
  const binaryStart = reenroll ? 2 : 1;

  const header = (
    <div className="flex flex-wrap items-center justify-between gap-2">
      <span className="flex items-center gap-2 font-semibold">
        <Terminal className="h-5 w-5 shrink-0" />
        {t("clientProvision.guide.heading", { name: enrollment.client_name })}
      </span>
      <span
        className={cn(
          "flex items-center gap-1 text-sm",
          expired ? "text-destructive" : "text-muted-foreground",
        )}
      >
        <Clock className="h-4 w-4 shrink-0" />
        {expired
          ? t("clientProvision.guide.expired")
          : t("clientProvision.guide.expiresIn", { remaining })}
      </span>
    </div>
  );

  const body = (
    <>
      {reenroll && (
        <p className="text-xs text-muted-foreground">
          {t("clientProvision.guide.skipNote", { step: 2 })}
        </p>
      )}
      <Tabs defaultValue="binary" className="min-w-0">
        <TabsList>
          <TabsTrigger value="binary">{t("clientProvision.guide.tabBinary")}</TabsTrigger>
          <TabsTrigger value="docker">{t("clientProvision.guide.tabDocker")}</TabsTrigger>
        </TabsList>
        <TabsContent value="binary" className="pt-4">
          <StepList steps={visibleBinary} startIndex={binaryStart} />
        </TabsContent>
        <TabsContent value="docker" className="space-y-3 pt-4">
          <p className="text-xs text-muted-foreground">
            {t("clientProvision.guide.dockerNote")}
          </p>
          <StepList steps={dockerSteps} startIndex={1} />
        </TabsContent>
      </Tabs>
    </>
  );

  if (!framed) {
    return (
      <div className="flex min-w-0 flex-col gap-4">
        {header}
        {body}
      </div>
    );
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>{header}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">{body}</CardContent>
    </Card>
  );
}
