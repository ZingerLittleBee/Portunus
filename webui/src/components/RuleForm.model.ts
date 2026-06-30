import { z } from "zod";

import type { PushRuleBody, Target } from "@/api/types";
import { EMPTY_RATE_LIMIT_FORM, formStateToRateLimit } from "@/components/RateLimitForm.helpers";

export const PROXY_PROTOCOL_NONE = "__none";

export const EMPTY_DISABLED_CLIENTS = new Set<string>();

const isPort = (value: string) =>
  /^\d{1,5}$/.test(value) && Number(value) >= 1 && Number(value) <= 65535;

type RuleFormTranslate = (key: string) => string;

const rateLimitSchema = z.object({
  bandwidth_in_bps: z.string(),
  bandwidth_out_bps: z.string(),
  new_connections_per_sec: z.string(),
  concurrent_connections: z.string(),
  bandwidth_in_burst: z.string(),
  bandwidth_out_burst: z.string(),
  new_connections_burst: z.string(),
});

export function createRuleFormSchema(t: RuleFormTranslate) {
  return z
    .object({
      client: z.string().trim().min(1, t("rulePush.requiredField")),
      listenStart: z.string().refine(isPort, t("rulePush.invalidPort")),
      listenEnd: z.string().refine((value) => value === "" || isPort(value), t("rulePush.invalidPort")),
      mode: z.enum(["single", "multi"]),
      target: z.string(),
      targetStart: z.string(),
      targetEnd: z.string(),
      targets: z.array(
        z.object({
          host: z.string(),
          port: z.string(),
          proxyProtocol: z.enum(["", "v1", "v2"]),
        }),
      ),
      healthCheckInterval: z.string(),
      protocol: z.enum(["tcp", "udp"]),
      sniPattern: z.string(),
      rateLimit: rateLimitSchema,
    })
    .superRefine((values, ctx) => {
      if (values.mode === "single") {
        if (!values.target.trim()) {
          ctx.addIssue({ code: "custom", path: ["target"], message: t("rulePush.requiredField") });
        }
        if (!isPort(values.targetStart)) {
          ctx.addIssue({ code: "custom", path: ["targetStart"], message: t("rulePush.invalidPort") });
        }
        if (values.targetEnd !== "" && !isPort(values.targetEnd)) {
          ctx.addIssue({ code: "custom", path: ["targetEnd"], message: t("rulePush.invalidPort") });
        }
        if (
          values.targetEnd !== "" &&
          isPort(values.targetEnd) &&
          formStateToRateLimit(values.rateLimit)
        ) {
          ctx.addIssue({
            code: "custom",
            path: ["targetEnd"],
            message: t("rulePush.rateLimitRangeConflict"),
          });
        }
        return;
      }

      values.targets.forEach((row, index) => {
        if (!row.host.trim()) {
          ctx.addIssue({
            code: "custom",
            path: ["targets", index, "host"],
            message: t("rulePush.requiredField"),
          });
        }
        if (!isPort(row.port)) {
          ctx.addIssue({
            code: "custom",
            path: ["targets", index, "port"],
            message: t("rulePush.invalidPort"),
          });
        }
      });
      if (values.healthCheckInterval !== "") {
        const interval = Number(values.healthCheckInterval);
        if (!Number.isInteger(interval) || interval < 1 || interval > 3600) {
          ctx.addIssue({
            code: "custom",
            path: ["healthCheckInterval"],
            message: t("rulePush.invalidHealthCheckInterval"),
          });
        }
      }
    });
}

export type RuleFormValues = z.infer<ReturnType<typeof createRuleFormSchema>>;

export interface RuleFormClientLite {
  client_id: string;
  client_name: string;
  connected: boolean;
}

export function createRuleFormDefaultValues(): RuleFormValues {
  return {
    client: "",
    listenStart: "30000",
    listenEnd: "",
    mode: "single",
    target: "127.0.0.1",
    targetStart: "9000",
    targetEnd: "",
    targets: [
      { host: "127.0.0.1", port: "9000", proxyProtocol: "" },
      { host: "127.0.0.1", port: "9001", proxyProtocol: "" },
    ],
    healthCheckInterval: "",
    protocol: "tcp",
    sniPattern: "",
    rateLimit: { ...EMPTY_RATE_LIMIT_FORM },
  };
}

export function buildPushRuleBody(
  values: RuleFormValues,
  clients: RuleFormClientLite[],
  sniEligible: boolean,
): PushRuleBody {
  const trimmedSni = values.sniPattern.trim();
  const rateLimit = formStateToRateLimit(values.rateLimit);
  const clientName =
    clients.find((client) => client.client_id === values.client)?.client_name ?? values.client;
  const baseBody: PushRuleBody = {
    client: clientName,
    listen_port: Number(values.listenStart),
    ...(values.listenEnd ? { listen_port_end: Number(values.listenEnd) } : {}),
    protocol: values.protocol,
    ...(sniEligible && trimmedSni ? { sni_pattern: trimmedSni } : {}),
    ...(rateLimit ? { rate_limit: rateLimit } : {}),
  };

  if (values.mode === "multi") {
    const targets: Target[] = values.targets.map((row, priority) => ({
      host: row.host,
      port: Number(row.port),
      priority,
      ...(values.protocol === "tcp" && row.proxyProtocol
        ? { proxy_protocol: row.proxyProtocol }
        : {}),
    }));
    return {
      ...baseBody,
      targets,
      ...(values.healthCheckInterval
        ? { health_check_interval_secs: Number(values.healthCheckInterval) }
        : {}),
    };
  }

  if (rateLimit) {
    return {
      ...baseBody,
      targets: [{ host: values.target, port: Number(values.targetStart), priority: 0 }],
    };
  }

  return {
    ...baseBody,
    target_host: values.target,
    target_port: Number(values.targetStart),
    ...(values.targetEnd ? { target_port_end: Number(values.targetEnd) } : {}),
  };
}
