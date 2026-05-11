import { useEffect } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Navigate, useLocation } from "react-router-dom";

import { apiFetch, UNAUTHORIZED_EVENT } from "@/api/client";
import { clearLegacyToken } from "@/auth/token-store";
import { isSuperadmin, type Identity, type Role } from "@/lib/permissions";
import { PermissionDenied } from "@/components/PermissionDenied";

export const ME_QUERY_KEY = ["users", "me"] as const;

export function fetchIdentity(): Promise<Identity> {
  return apiFetch<Identity>("/v1/users/me");
}

interface AuthGateProps {
  /** Required role; omit for "any authenticated user". */
  role?: Role;
  children: React.ReactNode;
}

export function AuthGate({ role, children }: AuthGateProps) {
  const queryClient = useQueryClient();
  const location = useLocation();

  useEffect(() => {
    const onUnauth = () => {
      clearLegacyToken();
      queryClient.clear();
    };
    window.addEventListener(UNAUTHORIZED_EVENT, onUnauth);
    return () => window.removeEventListener(UNAUTHORIZED_EVENT, onUnauth);
  }, [queryClient]);

  const { data: identity, isLoading, isError } = useQuery({
    queryKey: ME_QUERY_KEY,
    queryFn: fetchIdentity,
    retry: false,
    staleTime: 60_000,
  });

  if (isLoading || (!identity && !isError)) {
    return (
      <div className="flex min-h-screen items-center justify-center text-muted-foreground">
        Loading…
      </div>
    );
  }

  if (isError || !identity) {
    const next = encodeURIComponent(location.pathname + location.search);
    return <Navigate to={`/login?reason=session_expired&next=${next}`} replace />;
  }

  if (role === "superadmin" && !isSuperadmin(identity)) {
    return <PermissionDenied />;
  }

  return <>{children}</>;
}
