// 013-traffic-quotas G1: cell showing `current_period_bytes_used` /
// `monthly_bytes` as a horizontal progress bar + numeric overlay.

import { useTranslation } from "react-i18next";

import type { MonthlyQuotaView } from "@/api/types";
import { formatBytes } from "@/lib/format";

interface Props {
  quota: MonthlyQuotaView | undefined;
}

export function QuotaCellPeriodProgress({ quota }: Props) {
  const { t } = useTranslation();
  if (!quota) return <span className="text-muted-foreground">—</span>;
  const monthly = quota.monthly_bytes > 0 ? quota.monthly_bytes : 1;
  const pct = Math.min(100, Math.round((quota.current_period_bytes_used / monthly) * 100));
  const isExhausted = quota.exhausted;
  const barColor = isExhausted ? "bg-destructive" : "bg-primary";
  return (
    <div className="flex flex-col gap-1 min-w-[140px]">
      <div className="text-xs font-mono">
        {formatBytes(quota.current_period_bytes_used)} ({pct}%)
        {isExhausted ? (
          <span className="ml-2 text-destructive font-medium">
            {t("userQuota.exhausted")}
          </span>
        ) : null}
      </div>
      <div className="h-1.5 w-full overflow-hidden rounded bg-muted">
        <div
          className={`h-full ${barColor} transition-all`}
          style={{ width: `${pct}%` }}
        />
      </div>
    </div>
  );
}
