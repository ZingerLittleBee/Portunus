import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Copy, Check } from "lucide-react";

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";

interface TokenRevealModalProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /// One-shot secret. Shown once, scrubbed when the modal closes.
  token: string;
  title?: string;
  description?: string;
}

export function TokenRevealModal({
  open,
  onOpenChange,
  token,
  title,
  description,
}: TokenRevealModalProps) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);
  const tokenRef = useRef<HTMLPreElement>(null);

  useEffect(() => {
    if (!open) {
      setCopied(false);
      // Scrub DOM text on close — mirrors SC-006 token-leak budget.
      if (tokenRef.current) tokenRef.current.textContent = "";
    }
  }, [open]);

  async function copy() {
    try {
      await navigator.clipboard.writeText(token);
      setCopied(true);
      setTimeout(() => setCopied(false), 2_000);
    } catch {
      // Older browsers / permissions denied — fall back to selecting the text.
      if (tokenRef.current) {
        const range = document.createRange();
        range.selectNodeContents(tokenRef.current);
        const sel = window.getSelection();
        sel?.removeAllRanges();
        sel?.addRange(range);
      }
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{title ?? t("tokenReveal.title")}</DialogTitle>
          <DialogDescription>{description ?? t("tokenReveal.description")}</DialogDescription>
        </DialogHeader>
        <pre
          ref={tokenRef}
          className="select-all overflow-x-auto rounded-md bg-muted p-3 text-xs"
          aria-label={t("tokenReveal.tokenLabel")}
        >
          {token}
        </pre>
        <DialogFooter>
          <Button variant="outline" onClick={copy}>
            {copied ? <Check className="mr-2 h-4 w-4" /> : <Copy className="mr-2 h-4 w-4" />}
            {copied ? t("tokenReveal.copied") : t("tokenReveal.copy")}
          </Button>
          <Button onClick={() => onOpenChange(false)}>{t("tokenReveal.dismiss")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
