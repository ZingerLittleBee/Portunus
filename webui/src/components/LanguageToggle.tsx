import { useTranslation } from "react-i18next";
import { Languages } from "lucide-react";
import { Button } from "@/components/ui/button";
import { setLanguage, type Language, SUPPORTED_LANGUAGES } from "@/i18n";

export function LanguageToggle() {
  const { i18n, t } = useTranslation();
  const current = (i18n.resolvedLanguage ?? i18n.language) as Language;

  return (
    <div className="inline-flex items-center gap-1 rounded-md border bg-background p-1">
      <Languages className="ml-1 h-4 w-4 text-muted-foreground" aria-hidden />
      {SUPPORTED_LANGUAGES.map((lang) => (
        <Button
          key={lang}
          type="button"
          variant={current.startsWith(lang) ? "secondary" : "ghost"}
          size="sm"
          aria-pressed={current.startsWith(lang)}
          onClick={() => setLanguage(lang)}
        >
          {t(`language.${lang}`)}
        </Button>
      ))}
    </div>
  );
}
