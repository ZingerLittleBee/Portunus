import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Navigate, useLocation } from "react-router-dom";

import { UNAUTHORIZED_EVENT } from "@/api/client";
import { fetchIdentity, ME_QUERY_KEY } from "@/auth/identity";
import { clearLegacyToken } from "@/auth/token-store";
import { isSuperadmin, type Role } from "@/lib/permissions";
import { PermissionDenied } from "@/components/PermissionDenied";

function isMeQueryKey(queryKey: readonly unknown[]): boolean {
  return queryKey.length === ME_QUERY_KEY.length && queryKey.every((part, idx) => part === ME_QUERY_KEY[idx]);
}

interface AuthGateProps {
  /** Required role; omit for "any authenticated user". */
  requiredRole?: Role;
  children: React.ReactNode;
}

export function AuthGate({ requiredRole, children }: AuthGateProps) {
  const queryClient = useQueryClient();
  const location = useLocation();
  const [sessionInvalidated, setSessionInvalidated] = useState(false);

  useEffect(() => {
    const onUnauth = (event: Event) => {
      clearLegacyToken();
      const unauthPath = (event as CustomEvent<{ path?: string }>).detail?.path;
      if (unauthPath === "/v1/users/me") {
        queryClient.removeQueries({
          predicate: (query) => !isMeQueryKey(query.queryKey),
        });
        return;
      }
      queryClient.clear();
      setSessionInvalidated(true);
    };
    window.addEventListener(UNAUTHORIZED_EVENT, onUnauth);
    return () => window.removeEventListener(UNAUTHORIZED_EVENT, onUnauth);
  }, [queryClient]);

  const { data: identity, isLoading, isError } = useQuery({
    queryKey: ME_QUERY_KEY,
    queryFn: fetchIdentity,
    enabled: !sessionInvalidated,
    retry: false,
    staleTime: 60_000,
  });

  if (sessionInvalidated) {
    const next = encodeURIComponent(location.pathname + location.search);
    return <Navigate to={`/login?reason=session_expired&next=${next}`} replace />;
  }

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

  if (requiredRole === "superadmin" && !isSuperadmin(identity)) {
    return <PermissionDenied />;
  }

  return <>{children}</>;
}
