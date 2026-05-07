import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";

/// Single global banner that surfaces TanStack Query failures so they
/// don't disappear silently. Subscribes to the cache's `errorCount`
/// via the `query` listener — fires on first error, clears on next
/// successful refetch. Used at the top of `App.tsx` for visibility.
export function ErrorBanner() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [error, setError] = useState<string | null>(null);
  const [dismissed, setDismissed] = useState(false);

  useEffect(() => {
    const cache = queryClient.getQueryCache();
    return cache.subscribe((event) => {
      if (event.type === "updated") {
        const q = event.query;
        if (q.state.status === "error" && q.state.error instanceof Error) {
          setError(q.state.error.message);
          setDismissed(false);
        } else if (q.state.status === "success") {
          setError(null);
        }
      }
    });
  }, [queryClient]);

  if (!error || dismissed) return null;

  return (
    <div className="flex items-center gap-2 border-b border-destructive/40 bg-destructive/10 px-4 py-2 text-sm">
      <AlertTriangle className="h-4 w-4 shrink-0" />
      <span className="flex-1 truncate">
        {t("errorBanner.label")}: {error}
      </span>
      <Button
        variant="ghost"
        size="icon"
        aria-label={t("errorBanner.dismiss")}
        onClick={() => setDismissed(true)}
      >
        <X className="h-4 w-4" />
      </Button>
    </div>
  );
}
