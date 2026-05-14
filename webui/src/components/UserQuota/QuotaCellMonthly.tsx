// 013-traffic-quotas G1: read-only cell showing `monthly_bytes` and the
// next reset date. "— (unlimited)" when no quota is attached.

import { useTranslation } from "react-i18next";

import type { MonthlyQuotaView } from "@/api/types";
import { formatBytes } from "@/lib/format";

interface Props {
  quota: MonthlyQuotaView | undefined;
}

function formatResetDate(unixSec: number): string {
  return new Date(unixSec * 1000).toLocaleDateString();
}

export function QuotaCellMonthly({ quota }: Props) {
  const { t } = useTranslation();
  if (!quota) {
    return <span className="text-muted-foreground">— ({t("userQuota.form.unlimited")})</span>;
  }
  return (
    <div className="flex flex-col text-sm">
      <span className="font-mono">{formatBytes(quota.monthly_bytes)}</span>
      <span className="text-xs text-muted-foreground">
        {t("userQuota.resetsOn", {
          date: formatResetDate(quota.current_period_ends_at),
        })}
      </span>
    </div>
  );
}
