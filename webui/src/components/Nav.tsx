import { useTranslation } from "react-i18next";
import { NavLink } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";

import { ME_QUERY_KEY, fetchIdentity } from "@/auth/AuthGate";
import { canSeeAuditLog, canSeeUsersList } from "@/lib/permissions";
import { cn } from "@/lib/cn";
import { ThemeToggle } from "@/components/ThemeToggle";
import { LanguageToggle } from "@/components/LanguageToggle";
import { Button } from "@/components/ui/button";
import { clearToken } from "@/auth/token-store";

interface NavItem {
  to: string;
  i18nKey: string;
  visible: (id: ReturnType<typeof useIdentity>) => boolean;
}

const ITEMS: NavItem[] = [
  { to: "/", i18nKey: "nav.dashboard", visible: () => true },
  { to: "/users", i18nKey: "nav.users", visible: canSeeUsersList },
  { to: "/grants", i18nKey: "nav.grants", visible: () => true },
  { to: "/rules", i18nKey: "nav.rules", visible: () => true },
  { to: "/clients", i18nKey: "nav.clients", visible: () => true },
  { to: "/audit", i18nKey: "nav.audit", visible: canSeeAuditLog },
  { to: "/metrics", i18nKey: "nav.metrics", visible: () => true },
  { to: "/settings", i18nKey: "nav.settings", visible: () => true },
];

function useIdentity() {
  const { data } = useQuery({
    queryKey: ME_QUERY_KEY,
    queryFn: fetchIdentity,
    staleTime: 60_000,
  });
  return data;
}

export function Nav() {
  const { t } = useTranslation();
  const identity = useIdentity();

  return (
    <header className="sticky top-0 z-30 flex h-14 items-center border-b bg-background/80 px-4 backdrop-blur">
      <span className="mr-6 font-semibold">{t("appTitle")}</span>
      <nav className="flex flex-1 items-center gap-1 overflow-x-auto" aria-label="Primary">
        {ITEMS.filter((it) => it.visible(identity)).map((it) => (
          <NavLink
            key={it.to}
            to={it.to}
            end={it.to === "/"}
            className={({ isActive }) =>
              cn(
                "rounded-md px-3 py-1.5 text-sm font-medium transition-colors",
                isActive
                  ? "bg-secondary text-secondary-foreground"
                  : "text-muted-foreground hover:bg-muted hover:text-foreground",
              )
            }
          >
            {t(it.i18nKey)}
          </NavLink>
        ))}
      </nav>
      <div className="flex items-center gap-2">
        <ThemeToggle />
        <LanguageToggle />
        <Button
          variant="outline"
          size="sm"
          onClick={() => {
            clearToken();
            window.location.href = "/login";
          }}
        >
          {t("nav.signOut")}
        </Button>
      </div>
    </header>
  );
}
