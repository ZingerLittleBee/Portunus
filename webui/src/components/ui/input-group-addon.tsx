import * as React from "react"
import type { VariantProps } from "class-variance-authority"

import { inputGroupAddonVariants } from "@/components/ui/input-group-variants"
import { cn } from "@/lib/cn"

function InputGroupAddon({
  className,
  align = "inline-start",
  ...props
}: React.ComponentProps<"div"> & VariantProps<typeof inputGroupAddonVariants>) {
  return (
    <div
      data-slot="input-group-addon"
      data-align={align}
      className={cn(inputGroupAddonVariants({ align }), className)}
      onPointerDown={(event) => {
        const target = event.target
        if (!(target instanceof HTMLElement)) {
          return
        }
        if (target.closest("button")) {
          return
        }
        event.currentTarget.parentElement?.querySelector("input")?.focus()
      }}
      {...props}
    />
  )
}

export { InputGroupAddon }
