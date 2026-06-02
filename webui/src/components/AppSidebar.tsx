import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { NavLink, useLocation, useNavigate } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Activity,
  ChevronsUpDown,
  Globe,
  LayoutDashboard,
  ListChecks,
  LogOut,
  type LucideIcon,
  Monitor,
  Moon,
  Network,
  Settings,
  Shield,
  Sun,
  Users,
  Waypoints,
} from "lucide-react";

import { logout } from "@/api/auth";
import { ME_QUERY_KEY, fetchIdentity } from "@/auth/AuthGate";
import { clearLegacyToken } from "@/auth/token-store";
import { canSeeAuditLog, canSeeUsersList, type Identity } from "@/lib/permissions";
import { setLanguage, SUPPORTED_LANGUAGES, type Language } from "@/i18n";
import { useTheme, type ThemeChoice } from "@/theme/ThemeProvider";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarRail,
  useSidebar,
} from "@/components/ui/sidebar";

interface NavItem {
  to: string;
  i18nKey: string;
  icon: LucideIcon;
  end?: boolean;
  visible: (id: Identity | null | undefined) => boolean;
}

const NAV_ITEMS: NavItem[] = [
  { to: "/", i18nKey: "nav.dashboard", icon: LayoutDashboard, end: true, visible: () => true },
  { to: "/users", i18nKey: "nav.users", icon: Users, visible: canSeeUsersList },
  { to: "/rules", i18nKey: "nav.rules", icon: ListChecks, visible: () => true },
  { to: "/clients", i18nKey: "nav.clients", icon: Network, visible: () => true },
  { to: "/audit", i18nKey: "nav.audit", icon: Shield, visible: canSeeAuditLog },
  { to: "/metrics", i18nKey: "nav.metrics", icon: Activity, visible: () => true },
  { to: "/settings", i18nKey: "nav.settings", icon: Settings, visible: () => true },
];

function useIdentity() {
  const { data } = useQuery({
    queryKey: ME_QUERY_KEY,
    queryFn: fetchIdentity,
    staleTime: 60_000,
  });
  return data;
}

function initialsOf(name: string): string {
  const trimmed = name.trim();
  if (!trimmed) return "?";
  const parts = trimmed.split(/\s+/);
  const head = parts[0]?.[0] ?? "";
  const tail = parts.length > 1 ? parts[parts.length - 1]?.[0] ?? "" : "";
  return (head + tail).toUpperCase() || trimmed[0]!.toUpperCase();
}

const THEME_ICON: Record<ThemeChoice, LucideIcon> = {
  light: Sun,
  dark: Moon,
  system: Monitor,
};

export function AppSidebar() {
  const { t } = useTranslation();
  const identity = useIdentity();
  const location = useLocation();
  const { isMobile, setOpenMobile } = useSidebar();

  useEffect(() => {
    if (isMobile) setOpenMobile(false);
  }, [isMobile, location.pathname, setOpenMobile]);

  return (
    <Sidebar collapsible="icon">
      <SidebarHeader>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton size="lg" asChild>
              <NavLink to="/" end>
                <div className="flex aspect-square size-8 items-center justify-center rounded-lg bg-primary">
                  <Waypoints className="size-4 text-primary-foreground" />
                </div>
                <div className="grid flex-1 text-left text-sm leading-tight">
                  <span className="truncate font-semibold">{t("appTitle")}</span>
                  <span className="truncate text-xs text-muted-foreground">
                    {t("nav.dashboard")}
                  </span>
                </div>
              </NavLink>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarHeader>

      <SidebarContent>
        <SidebarGroup>
          <SidebarGroupLabel>{t("nav.dashboard")}</SidebarGroupLabel>
          <SidebarGroupContent>
            <SidebarMenu>
              {NAV_ITEMS.filter((it) => it.visible(identity)).map((it) => (
                <NavItemLink key={it.to} item={it} />
              ))}
            </SidebarMenu>
          </SidebarGroupContent>
        </SidebarGroup>
      </SidebarContent>

      <SidebarFooter>
        <NavUser identity={identity} />
      </SidebarFooter>

      <SidebarRail />
    </Sidebar>
  );
}

function NavItemLink({ item }: { item: NavItem }) {
  const { t } = useTranslation();
  const location = useLocation();
  const isActive = item.end
    ? location.pathname === item.to
    : location.pathname === item.to || location.pathname.startsWith(`${item.to}/`);
  const Icon = item.icon;
  const label = t(item.i18nKey);
  return (
    <SidebarMenuItem>
      <SidebarMenuButton asChild isActive={isActive} tooltip={label}>
        <NavLink to={item.to} end={item.end ?? false}>
          <Icon />
          <span>{label}</span>
        </NavLink>
      </SidebarMenuButton>
    </SidebarMenuItem>
  );
}

function NavUser({ identity }: { identity: Identity | null | undefined }) {
  const { t, i18n } = useTranslation();
  const { theme, setTheme } = useTheme();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const { isMobile } = useSidebar();

  async function signOut() {
    try {
      await logout();
    } catch {
      // Local sign-out should still clear client state if the session is gone.
    } finally {
      clearLegacyToken();
      queryClient.clear();
      navigate("/login", { replace: true });
    }
  }

  const displayName = identity?.display_name ?? identity?.user_id ?? "—";
  const role = identity?.role ?? "";
  const initials = initialsOf(displayName);
  const currentLang = (i18n.resolvedLanguage ?? i18n.language) as Language;
  const ThemeIcon = THEME_ICON[theme];

  return (
    <SidebarMenu>
      <SidebarMenuItem>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <SidebarMenuButton
              size="lg"
              aria-label={t("nav.userMenu")}
              className="data-[state=open]:bg-sidebar-accent data-[state=open]:text-sidebar-accent-foreground"
            >
              <Avatar className="size-8 rounded-lg">
                <AvatarFallback className="rounded-lg">{initials}</AvatarFallback>
              </Avatar>
              <div className="grid flex-1 text-left text-sm leading-tight">
                <span className="truncate font-semibold">{displayName}</span>
                <span className="truncate text-xs text-muted-foreground">{role}</span>
              </div>
              <ChevronsUpDown className="ml-auto size-4" />
            </SidebarMenuButton>
          </DropdownMenuTrigger>
          <DropdownMenuContent
            className="w-[--radix-dropdown-menu-trigger-width] min-w-56 rounded-lg"
            side={isMobile ? "bottom" : "right"}
            align="end"
            sideOffset={4}
          >
            <DropdownMenuLabel className="p-0 font-normal">
              <div className="flex items-center gap-2 px-1 py-1.5 text-left text-sm">
                <Avatar className="size-8 rounded-lg">
                  <AvatarFallback className="rounded-lg">{initials}</AvatarFallback>
                </Avatar>
                <div className="grid flex-1 text-left text-sm leading-tight">
                  <span className="truncate font-semibold">{displayName}</span>
                  <span className="truncate text-xs text-muted-foreground">{role}</span>
                </div>
              </div>
            </DropdownMenuLabel>

            <DropdownMenuSeparator />

            <DropdownMenuGroup>
              <DropdownMenuSub>
                <DropdownMenuSubTrigger>
                  <ThemeIcon className="size-3.5" />
                  <span>{t(`theme.${theme}`)}</span>
                </DropdownMenuSubTrigger>
                <DropdownMenuSubContent>
                  {(["light", "dark", "system"] as ThemeChoice[]).map((opt) => {
                    const Icon = THEME_ICON[opt];
                    return (
                      <DropdownMenuCheckboxItem
                        key={opt}
                        checked={theme === opt}
                        onCheckedChange={(v) => v && setTheme(opt)}
                      >
                        <Icon className="size-3.5" />
                        <span>{t(`theme.${opt}`)}</span>
                      </DropdownMenuCheckboxItem>
                    );
                  })}
                </DropdownMenuSubContent>
              </DropdownMenuSub>

              <DropdownMenuSub>
                <DropdownMenuSubTrigger>
                  <Globe className="size-3.5" />
                  <span>{t(`language.${currentLang}`)}</span>
                </DropdownMenuSubTrigger>
                <DropdownMenuSubContent>
                  {SUPPORTED_LANGUAGES.map((lang) => (
                    <DropdownMenuCheckboxItem
                      key={lang}
                      checked={currentLang.startsWith(lang)}
                      onCheckedChange={(v) => v && setLanguage(lang)}
                    >
                      <span>{t(`language.${lang}`)}</span>
                    </DropdownMenuCheckboxItem>
                  ))}
                </DropdownMenuSubContent>
              </DropdownMenuSub>
            </DropdownMenuGroup>

            <DropdownMenuSeparator />

            <DropdownMenuItem onSelect={() => void signOut()}>
              <LogOut className="size-3.5" />
              <span>{t("nav.signOut")}</span>
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </SidebarMenuItem>
    </SidebarMenu>
  );
}
