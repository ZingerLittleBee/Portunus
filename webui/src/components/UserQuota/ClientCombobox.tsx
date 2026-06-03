// webui/src/components/UserQuota/ClientCombobox.tsx
import { Check, ChevronsUpDown } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/cn";

export interface ClientLite {
  client_name: string;
  connected: boolean;
}

interface Props {
  clients: ClientLite[];
  value: string;
  onChange: (next: string) => void;
  disabledClientNames: Set<string>;
  disabled?: boolean;
  popoverContainer?: HTMLElement | null | undefined;
}

export function ClientCombobox({
  clients,
  value,
  onChange,
  disabledClientNames,
  disabled,
  popoverContainer,
}: Props) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);

  function selectClient(clientName: string, isDisabled: boolean) {
    if (isDisabled) return;
    onChange(clientName);
    setOpen(false);
  }

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          variant="outline"
          role="combobox"
          aria-label={t("userQuota.form.client")}
          aria-expanded={open}
          disabled={disabled}
          className="w-full justify-between"
        >
          {value || t("userQuota.combobox.placeholder")}
          <ChevronsUpDown className="ml-2 size-4 opacity-50" />
        </Button>
      </PopoverTrigger>
      <PopoverContent container={popoverContainer} className="w-(--radix-popover-trigger-width) p-0">
        <Command>
          <CommandInput placeholder={t("userQuota.combobox.search")} />
          <CommandList>
            <CommandEmpty>{t("userQuota.combobox.empty")}</CommandEmpty>
            <CommandGroup>
              {clients.map((c) => {
                const isDisabled = disabledClientNames.has(c.client_name);
                return (
                  <CommandItem
                    key={c.client_name}
                    value={c.client_name}
                    disabled={isDisabled}
                    onPointerDown={(event) => {
                      event.preventDefault();
                      selectClient(c.client_name, isDisabled);
                    }}
                    onSelect={() => selectClient(c.client_name, isDisabled)}
                  >
                    <Check
                      className={cn(
                        "mr-2 size-4",
                        value === c.client_name ? "opacity-100" : "opacity-0",
                      )}
                    />
                    <span className={cn("flex-1 font-mono", !c.connected && "opacity-60")}>
                      {c.client_name}
                    </span>
                    {!c.connected && (
                      <span className="ml-2 text-xs text-muted-foreground">
                        {t("userQuota.combobox.offline")}
                      </span>
                    )}
                    {isDisabled && (
                      <span className="ml-2 text-xs text-muted-foreground">
                        {t("userQuota.combobox.alreadyAssigned")}
                      </span>
                    )}
                  </CommandItem>
                );
              })}
            </CommandGroup>
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  );
}
