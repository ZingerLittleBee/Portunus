import { lazy, Suspense, useEffect } from "react";
import { Navigate, Route, Routes, useLocation } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";

import { getAuthStatus } from "@/api/auth";
import { AuthGate } from "@/auth/AuthGate";
import { LoginPage } from "@/auth/LoginPage";
import { OnboardingPage } from "@/auth/OnboardingPage";
import { clearLegacyToken } from "@/auth/token-store";
import { Nav } from "@/components/Nav";
import { ErrorBanner } from "@/components/ErrorBanner";
import { Dashboard } from "@/pages/Dashboard";
import { NotFound } from "@/pages/NotFound";
import { PermissionDenied } from "@/components/PermissionDenied";

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

function Shell({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex min-h-screen flex-col">
      <Nav />
      <ErrorBanner />
      <main className="container mx-auto flex-1 p-6">
        <Suspense fallback={<div className="text-muted-foreground">Loading…</div>}>{children}</Suspense>
      </main>
    </div>
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
