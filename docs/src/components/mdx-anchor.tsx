import defaultMdxComponents from "fumadocs-ui/mdx";
import type { ComponentProps, ComponentType } from "react";

const DefaultAnchor = defaultMdxComponents.a as ComponentType<
  ComponentProps<"a">
>;

export function MdxAnchor({
  children,
  href = "",
  ...props
}: ComponentProps<"a">) {
  const hasFragment =
    href.startsWith("#") || (href.startsWith("/") && href.includes("#"));

  if (hasFragment) {
    return (
      <a href={href} {...props}>
        {children}
      </a>
    );
  }

  return (
    <DefaultAnchor href={href} {...props}>
      {children}
    </DefaultAnchor>
  );
}
