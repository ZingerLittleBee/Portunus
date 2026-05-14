import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";
import { ShieldAlert } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";

/// 006-management-web-ui T043/T044: shown when an `<AuthGate>` rejects
/// the caller's role. Renders BEFORE any TanStack Query for the gated
/// subtree, so no API request fires. The placeholder NEVER mentions
/// the resource id or content (avoids leaking existence information).
export function PermissionDenied() {
  const { t } = useTranslation();
  return (
    <div className="flex min-h-[60vh] items-center justify-center p-8">
      <Empty>
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <ShieldAlert aria-hidden />
          </EmptyMedia>
          <EmptyTitle>{t("permissionDenied.title")}</EmptyTitle>
          <EmptyDescription>{t("permissionDenied.body")}</EmptyDescription>
        </EmptyHeader>
        <EmptyContent>
          <Button asChild variant="outline">
            <Link to="/">{t("permissionDenied.back")}</Link>
          </Button>
        </EmptyContent>
      </Empty>
    </div>
  );
}
