import { Toaster as Sonner, type ToasterProps } from "sonner"

import { useTheme } from "@/theme/ThemeProvider"

const Toaster = ({ ...props }: ToasterProps) => {
  const { effective } = useTheme()

  return (
    <Sonner
      theme={effective}
      className="toaster group"
      toastOptions={{
        classNames: {
          toast:
            "group toast group-[.toaster]:bg-background group-[.toaster]:text-foreground group-[.toaster]:border-border group-[.toaster]:shadow-lg",
          description: "group-[.toast]:text-muted-foreground",
          actionButton:
            "group-[.toast]:bg-primary group-[.toast]:text-primary-foreground",
          cancelButton:
            "group-[.toast]:bg-muted group-[.toast]:text-muted-foreground",
        },
      }}
      {...props}
    />
  )
}

export { Toaster }
