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

// 015-client-stable-id (US3): the combobox value is the stable client_id;
// the display label stays the friendly client_name. Disabled set is keyed
// by id so re-selecting an already-assigned client is blocked correctly
// even when two clients share a display name.
export interface ClientLite {
  client_id: string;
  client_name: string;
  connected: boolean;
}

interface Props {
  clients: ClientLite[];
  /// The selected client_id (or "" when nothing is picked).
  value: string;
  onChange: (nextClientId: string) => void;
  disabledClientIds: Set<string>;
  disabled?: boolean;
  popoverContainer?: HTMLElement | null | undefined;
}

export function ClientCombobox({
  clients,
  value,
  onChange,
  disabledClientIds,
  disabled,
  popoverContainer,
}: Props) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);

  const selectedName =
    clients.find((c) => c.client_id === value)?.client_name ?? "";

  function selectClient(clientId: string, isDisabled: boolean) {
    if (isDisabled) return;
    onChange(clientId);
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
          {selectedName || t("userQuota.combobox.placeholder")}
          <ChevronsUpDown className="ml-2 size-4 opacity-50" />
        </Button>
      </PopoverTrigger>
      <PopoverContent container={popoverContainer} className="w-[--radix-popover-trigger-width] p-0">
        <Command>
          <CommandInput placeholder={t("userQuota.combobox.search")} />
          <CommandList>
            <CommandEmpty>{t("userQuota.combobox.empty")}</CommandEmpty>
            <CommandGroup>
              {clients.map((c) => {
                const isDisabled = disabledClientIds.has(c.client_id);
                return (
                  <CommandItem
                    key={c.client_id}
                    // `value` drives the cmdk text filter — keep it the
                    // human-readable name so search-by-name still works.
                    value={c.client_name}
                    disabled={isDisabled}
                    onPointerDown={(event) => {
                      event.preventDefault();
                      selectClient(c.client_id, isDisabled);
                    }}
                    onSelect={() => selectClient(c.client_id, isDisabled)}
                  >
                    <Check
                      className={cn(
                        "mr-2 size-4",
                        value === c.client_id ? "opacity-100" : "opacity-0",
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
