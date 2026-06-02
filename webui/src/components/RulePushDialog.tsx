import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";
import { Plus } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { RuleForm } from "@/components/RuleForm";

/**
 * "Push rule" entry point rendered as a modal dialog instead of a
 * dedicated route. The dialog is widened to fit the multi-target form
 * and scrolls vertically so tall forms never overflow the viewport.
 * The standalone `/rules/new` route still mounts the same `RuleForm`
 * for deep links.
 */
export function RulePushDialog() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button className="w-full sm:w-auto">
          <Plus className="mr-1 h-4 w-4" />
          {t("rules.newRule")}
        </Button>
      </DialogTrigger>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t("rulePush.title")}</DialogTitle>
        </DialogHeader>
        <RuleForm
          onSuccess={(ruleId) => {
            setOpen(false);
            navigate(`/rules/${ruleId}`);
          }}
          onCancel={() => setOpen(false)}
        />
      </DialogContent>
    </Dialog>
  );
}
