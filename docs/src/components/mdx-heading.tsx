import { buttonVariants } from "fumadocs-ui/components/ui/button";
import { useCopyButton } from "fumadocs-ui/utils/use-copy-button";
import { CopyCheckIcon, LinkIcon } from "lucide-react";
import type { ComponentProps, ElementType } from "react";
import { cn } from "@/lib/cn";
import { siteUrl } from "@/lib/shared";

type HeadingTag = "h1" | "h2" | "h3" | "h4" | "h5" | "h6";

export function MdxHeading({
  as,
  ...props
}: { as: HeadingTag } & ComponentProps<"h1">) {
  const As = as as ElementType;
  const [isChecked, onCopy] = useCopyButton(() => {
    if (!props.id) return;
    const path = typeof window !== "undefined" ? window.location.pathname : "";
    return navigator.clipboard.writeText(`${siteUrl}${path}#${props.id}`);
  });

  if (!props.id) return <As {...props} />;

  const { className, children, ...rest } = props;
  return (
    <As
      {...rest}
      className={cn(
        "group/heading flex scroll-m-28 flex-row items-center gap-1",
        className,
      )}
    >
      <a data-card="" href={`#${props.id}`}>
        {children}
      </a>
      <button
        type="button"
        aria-label="Copy Anchor Link"
        className={cn(
          buttonVariants({ variant: "ghost", size: "icon-xs" }),
          "not-prose shrink-0 text-fd-muted-foreground opacity-0 transition-opacity group-hover/heading:opacity-100",
        )}
        onClick={onCopy}
      >
        {isChecked ? <CopyCheckIcon /> : <LinkIcon />}
      </button>
    </As>
  );
}
