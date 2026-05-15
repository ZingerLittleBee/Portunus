// webui/src/pages/Dashboard.tsx
import { useQuery } from "@tanstack/react-query";

import { ME_QUERY_KEY, fetchIdentity } from "@/auth/AuthGate";
import { Skeleton } from "@/components/ui/skeleton";
import { SuperadminDashboard } from "@/pages/dashboard/SuperadminDashboard";
import { TenantDashboard } from "@/pages/dashboard/TenantDashboard";

export function Dashboard() {
  const { data: identity, isLoading } = useQuery({
    queryKey: ME_QUERY_KEY,
    queryFn: fetchIdentity,
    staleTime: 60_000,
  });

  if (isLoading || !identity) {
    return <Skeleton className="h-24 w-full" />;
  }

  return identity.role === "superadmin" ? <SuperadminDashboard /> : <TenantDashboard />;
}
