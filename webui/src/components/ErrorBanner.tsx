import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, X } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
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
    <div className="px-4 pt-4">
      <Alert variant="destructive" className="flex items-start gap-3 pr-2">
        <AlertTriangle className="h-4 w-4" />
        <div className="min-w-0 flex-1">
          <AlertTitle>{t("errorBanner.label")}</AlertTitle>
          <AlertDescription className="truncate">{error}</AlertDescription>
        </div>
        <Button
          variant="ghost"
          size="icon"
          className="-mt-1 -mr-1 size-7"
          aria-label={t("errorBanner.dismiss")}
          onClick={() => setDismissed(true)}
        >
          <X className="h-4 w-4" />
        </Button>
      </Alert>
    </div>
  );
}
