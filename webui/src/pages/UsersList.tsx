import { useState, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate } from "react-router-dom";
import { Plus } from "lucide-react";

import { useUsersList } from "@/api/users";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Badge } from "@/components/ui/badge";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { DataTable, type Column } from "@/components/DataTable";
import { EmptyState } from "@/components/EmptyState";
import { UserCreateForm } from "@/components/UserCreateForm";
import type { UserView } from "@/api/types";

export function UsersList() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [filter, setFilter] = useState("");
  const [createOpen, setCreateOpen] = useState(false);
  const { data, isLoading, error } = useUsersList();

  const filtered = useMemo(() => {
    const rows = data ?? [];
    if (!filter.trim()) return rows;
    const f = filter.trim().toLowerCase();
    return rows.filter(
      (u) => u.user_id.toLowerCase().includes(f) || u.display_name.toLowerCase().includes(f),
    );
  }, [data, filter]);

  const columns: Column<UserView>[] = [
    {
      key: "user_id",
      header: t("users.id"),
      render: (u) => (
        <Link to={`/users/${u.user_id}`} className="font-mono text-primary hover:underline">
          {u.user_id}
        </Link>
      ),
      sortable: true,
      sortValue: (u) => u.user_id,
    },
    {
      key: "display_name",
      header: t("users.displayName"),
      render: (u) => u.display_name,
      sortable: true,
      sortValue: (u) => u.display_name,
    },
    {
      key: "role",
      header: t("users.role"),
      render: (u) => (
        <Badge variant={u.role === "superadmin" ? "default" : "secondary"}>{u.role}</Badge>
      ),
      width: "120px",
    },
    {
      key: "grants",
      header: t("users.grants"),
      render: (u) => u.grant_count,
      width: "100px",
    },
  ];

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <h1 className="text-2xl font-semibold">{t("users.title")}</h1>
        <Dialog open={createOpen} onOpenChange={setCreateOpen}>
          <DialogTrigger asChild>
            <Button className="w-full sm:w-auto">
              <Plus className="mr-1 h-4 w-4" />
              {t("users.newUser")}
            </Button>
          </DialogTrigger>
          <DialogContent className="max-h-[90vh] overflow-y-auto">
            <DialogHeader>
              <DialogTitle>{t("userCreate.title")}</DialogTitle>
            </DialogHeader>
            <UserCreateForm
              onSuccess={(userId) => {
                setCreateOpen(false);
                navigate(`/users/${userId}`);
              }}
              onCancel={() => setCreateOpen(false)}
            />
          </DialogContent>
        </Dialog>
      </div>
      {error && <p className="text-sm text-destructive">{(error as Error).message}</p>}
      <DataTable
        rows={filtered}
        columns={columns}
        rowKey={(u) => u.user_id}
        onRowClick={(u) => navigate(`/users/${u.user_id}`)}
        toolbar={
          <Input
            placeholder={t("users.filterPlaceholder")}
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            className="w-full sm:max-w-xs"
          />
        }
        emptyState={
          isLoading ? (
            t("table.loading")
          ) : (
            <EmptyState title={t("users.emptyTitle")} description={t("users.emptyBody")} />
          )
        }
        ariaLabel={t("users.title")}
      />
    </div>
  );
}
