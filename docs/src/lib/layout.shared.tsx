import { defineI18nUI } from "fumadocs-ui/i18n";
import type { BaseLayoutProps } from "fumadocs-ui/layouts/shared";
import { i18n } from "./i18n";
import { appName, gitConfig } from "./shared";

export const i18nUI = defineI18nUI(i18n, {
  translations: {
    en: {
      displayName: "English",
    },
    zh: {
      displayName: "简体中文",
      search: "搜索文档",
      previousPage: "上一页",
      nextPage: "下一页",
      toc: "目录",
      lastUpdate: "最后更新",
      chooseLanguage: "选择语言",
      chooseTheme: "主题",
      editOnGithub: "在 GitHub 上编辑",
    },
  },
});

export function baseOptions(locale?: string): BaseLayoutProps {
  const lang = locale === "zh" ? "zh" : "en";

  return {
    i18n,
    links: [
      {
        type: "main",
        text: lang === "zh" ? "文档" : "Docs",
        url: `/${lang}/docs`,
        active: "nested-url",
      },
    ],
    nav: {
      title: (
        <span className="fr-brand-mark">
          <span aria-hidden className="fr-brand-icon" />
          <span>{appName}</span>
        </span>
      ),
    },
    githubUrl: `https://github.com/${gitConfig.user}/${gitConfig.repo}`,
    ...(locale ? {} : {}),
  };
}
