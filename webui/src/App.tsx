import { lazy, Suspense, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { Navigate, Route, Routes, useLocation } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";

import { getAuthStatus } from "@/api/auth";
import { AuthGate } from "@/auth/AuthGate";
import { LoginPage } from "@/auth/LoginPage";
import { OnboardingPage } from "@/auth/OnboardingPage";
import { clearLegacyToken } from "@/auth/token-store";
import { AppSidebar } from "@/components/AppSidebar";
import { ErrorBanner } from "@/components/ErrorBanner";
import { Dashboard } from "@/pages/Dashboard";
import { NotFound } from "@/pages/NotFound";
import { PermissionDenied } from "@/components/PermissionDenied";
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbList,
  BreadcrumbPage,
} from "@/components/ui/breadcrumb";
import { Separator } from "@/components/ui/separator";
import {
  SidebarInset,
  SidebarProvider,
  SidebarTrigger,
} from "@/components/ui/sidebar";

// Lazy-load page modules so the initial route bundle stays small.
const UsersList = lazy(() => import("@/pages/UsersList").then((m) => ({ default: m.UsersList })));
const UserCreate = lazy(() => import("@/pages/UserCreate").then((m) => ({ default: m.UserCreate })));
const UserDetail = lazy(() => import("@/pages/UserDetail").then((m) => ({ default: m.UserDetail })));
const RulesList = lazy(() => import("@/pages/RulesList").then((m) => ({ default: m.RulesList })));
const RulePush = lazy(() => import("@/pages/RulePush").then((m) => ({ default: m.RulePush })));
const RuleDetail = lazy(() => import("@/pages/RuleDetail").then((m) => ({ default: m.RuleDetail })));
const ClientsList = lazy(() => import("@/pages/ClientsList").then((m) => ({ default: m.ClientsList })));
const ClientProvision = lazy(() =>
  import("@/pages/ClientProvision").then((m) => ({ default: m.ClientProvision })),
);
const ClientDetail = lazy(() =>
  import("@/pages/ClientDetail").then((m) => ({ default: m.ClientDetail })),
);
const AuditLog = lazy(() => import("@/pages/AuditLog").then((m) => ({ default: m.AuditLog })));
const Metrics = lazy(() => import("@/pages/Metrics").then((m) => ({ default: m.Metrics })));
const Settings = lazy(() => import("@/pages/Settings").then((m) => ({ default: m.Settings })));

const ROUTE_TITLES: Array<{ match: (p: string) => boolean; key: string }> = [
  { match: (p) => p === "/", key: "nav.dashboard" },
  { match: (p) => p === "/users/new", key: "nav.users" },
  { match: (p) => p.startsWith("/users"), key: "nav.users" },
  { match: (p) => p === "/rules/new", key: "nav.rules" },
  { match: (p) => p.startsWith("/rules"), key: "nav.rules" },
  { match: (p) => p === "/clients/new", key: "nav.clients" },
  { match: (p) => p.startsWith("/clients"), key: "nav.clients" },
  { match: (p) => p.startsWith("/audit"), key: "nav.audit" },
  { match: (p) => p.startsWith("/metrics"), key: "nav.metrics" },
  { match: (p) => p.startsWith("/settings"), key: "nav.settings" },
];

function PageBreadcrumb() {
  const { t } = useTranslation();
  const location = useLocation();
  const entry = ROUTE_TITLES.find((r) => r.match(location.pathname));
  const label = entry ? t(entry.key) : t("appTitle");
  return (
    <Breadcrumb>
      <BreadcrumbList>
        <BreadcrumbItem>
          <BreadcrumbPage>{label}</BreadcrumbPage>
        </BreadcrumbItem>
      </BreadcrumbList>
    </Breadcrumb>
  );
}

function Shell({ children }: { children: React.ReactNode }) {
  return (
    <SidebarProvider>
      <AppSidebar />
      <SidebarInset>
        <header className="sticky top-0 z-30 flex h-14 shrink-0 items-center gap-2 border-b bg-background/80 px-3 backdrop-blur sm:px-4">
          <SidebarTrigger className="-ml-1" />
          <Separator orientation="vertical" className="mr-2 hidden h-4 sm:block" />
          <PageBreadcrumb />
        </header>
        <ErrorBanner />
        <main className="flex min-w-0 flex-1 flex-col gap-4 p-4 sm:p-6">
          <Suspense fallback={<div className="text-muted-foreground">Loading…</div>}>
            {children}
          </Suspense>
        </main>
      </SidebarInset>
    </SidebarProvider>
  );
}

function AuthStatusGate({ children }: { children: React.ReactNode }) {
  const location = useLocation();
  const { data, isLoading } = useQuery({
    queryKey: ["auth", "status"],
    queryFn: getAuthStatus,
    retry: false,
    staleTime: 30_000,
  });

  if (isLoading || !data) {
    if (!isLoading) {
      return <>{children}</>;
    }
    return (
      <div className="flex min-h-screen items-center justify-center text-muted-foreground">
        Loading…
      </div>
    );
  }
  if (data.onboarding_required && location.pathname !== "/onboarding") {
    return <Navigate to="/onboarding" replace />;
  }
  if (!data.onboarding_required && location.pathname === "/onboarding") {
    return <Navigate to="/login" replace />;
  }
  if (location.pathname === "/login") {
    return <>{children}</>;
  }
  return <>{children}</>;
}

export function App() {
  useEffect(() => {
    clearLegacyToken();
  }, []);

  return (
    <AuthStatusGate>
      <Routes>
        <Route path="/login" element={<LoginPage />} />
        <Route path="/onboarding" element={<OnboardingPage />} />
        <Route
          path="/"
          element={
            <AuthGate>
              <Shell>
                <Dashboard />
              </Shell>
            </AuthGate>
          }
        />
      <Route
        path="/users"
        element={
          <AuthGate role="superadmin">
            <Shell>
              <UsersList />
            </Shell>
          </AuthGate>
        }
      />
      <Route
        path="/users/new"
        element={
          <AuthGate role="superadmin">
            <Shell>
              <UserCreate />
            </Shell>
          </AuthGate>
        }
      />
      <Route
        path="/users/:userId/*"
        element={
          <AuthGate>
            <Shell>
              <UserDetail />
            </Shell>
          </AuthGate>
        }
      />
      <Route path="/grants" element={<Navigate to="/users" replace />} />
      <Route path="/grants/new" element={<Navigate to="/users" replace />} />
      <Route
        path="/rules"
        element={
          <AuthGate>
            <Shell>
              <RulesList />
            </Shell>
          </AuthGate>
        }
      />
      <Route
        path="/rules/new"
        element={
          <AuthGate>
            <Shell>
              <RulePush />
            </Shell>
          </AuthGate>
        }
      />
      <Route
        path="/rules/:ruleId"
        element={
          <AuthGate>
            <Shell>
              <RuleDetail />
            </Shell>
          </AuthGate>
        }
      />
      <Route
        path="/clients"
        element={
          <AuthGate>
            <Shell>
              <ClientsList />
            </Shell>
          </AuthGate>
        }
      />
      <Route
        path="/clients/new"
        element={
          <AuthGate role="superadmin">
            <Shell>
              <ClientProvision />
            </Shell>
          </AuthGate>
        }
      />
      <Route
        path="/clients/:clientName"
        element={
          <AuthGate>
            <Shell>
              <ClientDetail />
            </Shell>
          </AuthGate>
        }
      />
      <Route
        path="/audit"
        element={
          <AuthGate role="superadmin">
            <Shell>
              <AuditLog />
            </Shell>
          </AuthGate>
        }
      />
      <Route
        path="/metrics"
        element={
          <AuthGate>
            <Shell>
              <Metrics />
            </Shell>
          </AuthGate>
        }
      />
      <Route
        path="/settings"
        element={
          <AuthGate>
            <Shell>
              <Settings />
            </Shell>
          </AuthGate>
        }
      />
        <Route path="/forbidden" element={<PermissionDenied />} />
        <Route path="*" element={<NotFound />} />
      </Routes>
    </AuthStatusGate>
  );
}
