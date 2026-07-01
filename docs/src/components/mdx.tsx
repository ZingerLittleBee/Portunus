import { Accordion, Accordions } from "fumadocs-ui/components/accordion";
import { Tab, Tabs } from "fumadocs-ui/components/tabs";
import defaultMdxComponents from "fumadocs-ui/mdx";
import type { MDXComponents } from "mdx/types";
import type { ComponentProps } from "react";
import { MdxAnchor } from "@/components/mdx-anchor";
import { MdxHeading } from "@/components/mdx-heading";

export function getMDXComponents(components?: MDXComponents) {
  return {
    ...defaultMdxComponents,
    Accordion,
    Accordions,
    Tab,
    Tabs,
    a: MdxAnchor,
    h1: (p: ComponentProps<"h1">) => <MdxHeading as="h1" {...p} />,
    h2: (p: ComponentProps<"h2">) => <MdxHeading as="h2" {...p} />,
    h3: (p: ComponentProps<"h3">) => <MdxHeading as="h3" {...p} />,
    h4: (p: ComponentProps<"h4">) => <MdxHeading as="h4" {...p} />,
    h5: (p: ComponentProps<"h5">) => <MdxHeading as="h5" {...p} />,
    h6: (p: ComponentProps<"h6">) => <MdxHeading as="h6" {...p} />,
    ...components,
  } satisfies MDXComponents;
}

declare global {
  type MDXProvidedComponents = ReturnType<typeof getMDXComponents>;
}
