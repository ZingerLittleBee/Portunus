import { useTranslation } from "react-i18next";
import { Moon, Sun, Monitor } from "lucide-react";
import type { ThemeChoice } from "@/theme/theme-context";
import { useTheme } from "@/theme/useTheme";
import { Button } from "@/components/ui/button";

export function ThemeToggle() {
  const { t } = useTranslation();
  const { theme, setTheme } = useTheme();

  const options: { value: ThemeChoice; label: string; Icon: typeof Sun }[] = [
    { value: "light", label: t("theme.light"), Icon: Sun },
    { value: "dark", label: t("theme.dark"), Icon: Moon },
    { value: "system", label: t("theme.system"), Icon: Monitor },
  ];

  return (
    <div className="inline-flex rounded-md border bg-background p-1">
      {options.map((opt) => (
        <Button
          key={opt.value}
          type="button"
          variant={theme === opt.value ? "secondary" : "ghost"}
          size="sm"
          aria-pressed={theme === opt.value}
          onClick={() => setTheme(opt.value)}
        >
          <opt.Icon className="mr-1 h-4 w-4" />
          {opt.label}
        </Button>
      ))}
    </div>
  );
}
