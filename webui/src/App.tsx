import { lazy, Suspense } from "react";
import { Route, Routes } from "react-router-dom";

import { AuthGate } from "@/auth/AuthGate";
import { LoginPage } from "@/auth/LoginPage";
import { Nav } from "@/components/Nav";
import { ErrorBanner } from "@/components/ErrorBanner";
import { Dashboard } from "@/pages/Dashboard";
import { NotFound } from "@/pages/NotFound";
import { PermissionDenied } from "@/components/PermissionDenied";

// Lazy-load page modules so the initial route bundle stays small.
const UsersList = lazy(() => import("@/pages/UsersList").then((m) => ({ default: m.UsersList })));
const UserCreate = lazy(() => import("@/pages/UserCreate").then((m) => ({ default: m.UserCreate })));
const UserDetail = lazy(() => import("@/pages/UserDetail").then((m) => ({ default: m.UserDetail })));
const GrantsList = lazy(() => import("@/pages/GrantsList").then((m) => ({ default: m.GrantsList })));
const GrantCreate = lazy(() => import("@/pages/GrantCreate").then((m) => ({ default: m.GrantCreate })));
const RulesList = lazy(() => import("@/pages/RulesList").then((m) => ({ default: m.RulesList })));
const RulePush = lazy(() => import("@/pages/RulePush").then((m) => ({ default: m.RulePush })));
const RuleDetail = lazy(() => import("@/pages/RuleDetail").then((m) => ({ default: m.RuleDetail })));
const ClientsList = lazy(() => import("@/pages/ClientsList").then((m) => ({ default: m.ClientsList })));
const ClientProvision = lazy(() =>
  import("@/pages/ClientProvision").then((m) => ({ default: m.ClientProvision })),
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

export function App() {
  return (
    <Routes>
      <Route path="/login" element={<LoginPage />} />
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
      <Route
        path="/grants"
        element={
          <AuthGate>
            <Shell>
              <GrantsList />
            </Shell>
          </AuthGate>
        }
      />
      <Route
        path="/grants/new"
        element={
          <AuthGate role="superadmin">
            <Shell>
              <GrantCreate />
            </Shell>
          </AuthGate>
        }
      />
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
  );
}
