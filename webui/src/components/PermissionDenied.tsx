import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";

import { Button } from "@/components/ui/button";

/// 006-management-web-ui T043/T044: shown when an `<AuthGate>` rejects
/// the caller's role. Renders BEFORE any TanStack Query for the gated
/// subtree, so no API request fires. The placeholder NEVER mentions
/// the resource id or content (avoids leaking existence information).
export function PermissionDenied() {
  const { t } = useTranslation();
  return (
    <div className="flex min-h-[60vh] flex-col items-center justify-center gap-3 p-8 text-center">
      <h1 className="text-2xl font-semibold">{t("permissionDenied.title")}</h1>
      <p className="max-w-md text-sm text-muted-foreground">
        {t("permissionDenied.body")}
      </p>
      <Button asChild variant="outline">
        <Link to="/">{t("permissionDenied.back")}</Link>
      </Button>
    </div>
  );
}
