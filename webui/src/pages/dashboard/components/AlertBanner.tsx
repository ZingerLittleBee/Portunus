import { AlertTriangle } from "lucide-react";

export interface AlertBannerProps {
  /// If empty, the banner renders nothing — callers don't need to guard.
  issues: string[];
}

export function AlertBanner({ issues }: AlertBannerProps) {
  if (issues.length === 0) return null;

  const keyedIssues: Array<{ key: string; message: string; separator: boolean }> = [];
  const seen = new Map<string, number>();
  for (const message of issues) {
    const occurrence = (seen.get(message) ?? 0) + 1;
    seen.set(message, occurrence);
    keyedIssues.push({
      key: `${message}:${occurrence}`,
      message,
      separator: keyedIssues.length > 0,
    });
  }

  return (
    <div
      role="alert"
      className="flex items-center gap-3 rounded-md border border-amber-300 bg-amber-50 px-4 py-2 text-sm text-amber-900 dark:border-amber-700 dark:bg-amber-950/50 dark:text-amber-200"
    >
      <AlertTriangle className="h-4 w-4 shrink-0" />
      <div className="flex flex-wrap gap-x-3 gap-y-1">
        {keyedIssues.map((issue) => (
          <span key={issue.key} className="flex items-center gap-2">
            {issue.separator && (
              <span aria-hidden="true" className="opacity-60">
                ·
              </span>
            )}
            {issue.message}
          </span>
        ))}
      </div>
    </div>
  );
}
