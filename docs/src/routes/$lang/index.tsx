import { createFileRoute, Link } from '@tanstack/react-router';
import { HomeLayout } from 'fumadocs-ui/layouts/home';
import { baseOptions } from '@/lib/layout.shared';

export const Route = createFileRoute('/$lang/')({
  component: Home,
});

const copy = {
  en: {
    title: 'High-performance TCP & UDP forwarding',
    desc: 'A Rust port-forwarding service with a control-plane server, edge clients, multi-tenant RBAC, TLS SNI routing, rate limiting, and a web UI. Single-binary deployment, zero runtime dependencies.',
    cta: 'Get Started',
    secondary: 'Read the Docs',
  },
  zh: {
    title: '高性能 TCP / UDP 端口转发',
    desc: '一个用 Rust 编写的端口转发服务，包含控制面 Server、边缘 Client、多租户 RBAC、TLS SNI 路由、限流与管理 Web UI。单二进制部署，无运行时依赖。',
    cta: '快速开始',
    secondary: '阅读文档',
  },
} as const;

function Home() {
  const { lang } = Route.useParams();
  const t = copy[lang === 'zh' ? 'zh' : 'en'];

  return (
    <HomeLayout {...baseOptions(lang)}>
      <div className="flex flex-col items-center justify-center text-center flex-1 px-6 py-20 gap-6">
        <h1 className="font-bold text-4xl md:text-5xl tracking-tight max-w-3xl">
          {t.title}
        </h1>
        <p className="text-fd-muted-foreground text-lg max-w-2xl">{t.desc}</p>
        <div className="flex flex-row gap-3 mt-4">
          <Link
            to="/$lang/docs/$"
            params={{ lang, _splat: 'getting-started/installation' }}
            className="px-4 py-2 rounded-lg bg-fd-primary text-fd-primary-foreground font-medium text-sm"
          >
            {t.cta}
          </Link>
          <Link
            to="/$lang/docs/$"
            params={{ lang, _splat: '' }}
            className="px-4 py-2 rounded-lg border border-fd-border font-medium text-sm"
          >
            {t.secondary}
          </Link>
        </div>
      </div>
    </HomeLayout>
  );
}
