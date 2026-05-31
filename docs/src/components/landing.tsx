import { Link } from "@tanstack/react-router";
import { HomeLayout } from "fumadocs-ui/layouts/home";
import {
  Activity,
  ArrowRight,
  Cpu,
  Gauge,
  Network,
  ShieldCheck,
  SlidersHorizontal,
  Terminal,
} from "lucide-react";
import type { ComponentType } from "react";
import { baseOptions } from "@/lib/layout.shared";

type Locale = "en" | "zh";

type Capability = {
  icon: ComponentType<{ className?: string }>;
  title: string;
  text: string;
};

const meterSegments = [
  "01",
  "02",
  "03",
  "04",
  "05",
  "06",
  "07",
  "08",
  "09",
  "10",
];

const copy = {
  en: {
    title: "Portunus",
    headlineLead: "Forward ports,",
    headlineAccent: "without giving up control.",
    hero: "Run lightweight clients on the hosts that own your public ports. Push TCP and UDP forwarding rules from one server. Get permissions, rate limits, metrics, and audit out of the box, without reading, modifying, or decrypting the bytes that flow through.",
    primary: "Read the docs",
    secondary: "Performance report",
    proof: "Linux · Rust · single binary deploy",
    mockTitle: "Portunus / dashboard",
    mockBadge: "real-time",
    rulesHeading: "rules",
    metricsHeading: "throughput",
    rules: [
      { id: "01", proto: "TCP", listen: "*:8443", target: "svc.io:443" },
      { id: "02", proto: "UDP", listen: "*:53000", target: "10.0.4.7:53000" },
      {
        id: "03",
        proto: "TCP",
        listen: "*:9000-9009",
        target: "edge.lan:9000",
      },
    ],
    metricRows: [
      { label: "rule #01", bars: 8, value: "3.2 Gbit/s" },
      { label: "rule #02", bars: 5, value: "640 Mbit/s" },
      { label: "rule #03", bars: 3, value: "180 Mbit/s" },
    ],
    statusOk: "OK",
    statusActive: "ACTIVE",
    rangesTitle: "Performance you can plan around.",
    rangesIntro:
      "On Linux, plain TCP rules with no bandwidth cap use a kernel splice fast path. On the bench host, single-flow throughput doubles from 9.9 Gbit/s to 21.9 Gbit/s, and across the offered-load sweep the proxied flow stays within 95-109 % of both direct iperf3 and iptables REDIRECT through 20 Gbit/s — the v0.11 saturation point disappears for uncapped TCP.",
    ranges: [
      [
        "100M – 10G",
        "Hits the offered rate end-to-end. Indistinguishable from a direct iperf3 baseline.",
      ],
      [
        "12.5G – 20G",
        "With the splice fast path, the proxied flow stays within iperf3 noise of direct loopback and iptables REDIRECT.",
      ],
      [
        "Rate-limited rules",
        "Bandwidth-capped rules stay on the canonical userspace path — byte-identical metrics, counters, and audit.",
      ],
    ],
    capabilitiesTitle:
      "Everything a team needs around the rule, not just the rule.",
    capabilitiesIntro:
      "Rule push, permissions, rate limits, metrics, and audit are first-class on the server, so you do not stitch them together yourself.",
    capabilities: [
      {
        icon: Network,
        title: "Rules",
        text: "Forward TCP or UDP, single ports or ranges, to IP or DNS targets. Route HTTPS by hostname without decrypting the connection.",
      },
      {
        icon: ShieldCheck,
        title: "Access",
        text: "Each user can be limited to specific clients, protocols, and port ranges. Rules are owned, and ownership is enforced on the server.",
      },
      {
        icon: SlidersHorizontal,
        title: "Limits",
        text: "Cap bandwidth, new connections per second, and concurrent connections per rule or per user, so one edge host can be shared safely.",
      },
      {
        icon: Activity,
        title: "Visibility",
        text: "Prometheus metrics, structured logs, an audit trail, an embedded SQLite store, and built-in backup / restore.",
      },
    ],
    topologyTitle: "One server. Many edge clients.",
    topologyIntro:
      "You talk to the server from CLI, API, or the Web UI. The server pushes signed rule bundles to the clients that own the public ports. Your traffic flows directly through the client, never through the server.",
    nodeOperator: "You",
    nodeServer: "Server",
    nodeServerSub: "rules + access",
    nodeClient: "Edge Client",
    nodeTarget: "Target",
    edgeControl: "rules",
    edgeData: "your traffic",
    workflowTitle: "Four steps from install to live traffic.",
    workflow: [
      "Generate a one-time client enrollment command from the server.",
      "Run the client on the host that owns your public ports.",
      "Add forwarding rules from the CLI, HTTP API, or Web UI.",
      "Watch traffic, quotas, and rejects in real time, without touching user data.",
    ],
    finalTitle: "Read the docs. Run the benchmark. Decide.",
    finalText:
      "The performance report ships with the exact commands, raw numbers, and caveats so you can compare against your own bandwidth target before adopting it.",
    finalCta: "Open documentation",
  },
  zh: {
    title: "Portunus",
    headlineLead: "转发端口——",
    headlineAccent: "但不放弃控制。",
    hero: "在拥有公网端口的主机上运行轻量 Client，从一个 Server 下发 TCP / UDP 转发规则。权限、限速、指标、审计开箱即用——业务流量不被读、不被改、不被解密。",
    primary: "阅读文档",
    secondary: "性能报告",
    proof: "Linux · Rust · 单二进制部署",
    mockTitle: "Portunus / 仪表盘",
    mockBadge: "实时",
    rulesHeading: "规则",
    metricsHeading: "吞吐",
    rules: [
      { id: "01", proto: "TCP", listen: "*:8443", target: "svc.io:443" },
      { id: "02", proto: "UDP", listen: "*:53000", target: "10.0.4.7:53000" },
      {
        id: "03",
        proto: "TCP",
        listen: "*:9000-9009",
        target: "edge.lan:9000",
      },
    ],
    metricRows: [
      { label: "规则 #01", bars: 8, value: "3.2 Gbit/s" },
      { label: "规则 #02", bars: 5, value: "640 Mbit/s" },
      { label: "规则 #03", bars: 3, value: "180 Mbit/s" },
    ],
    statusOk: "OK",
    statusActive: "运行中",
    rangesTitle: "性能可预期，便于容量规划。",
    rangesIntro:
      "Linux 上不限带宽的 TCP 规则会走内核 splice 快路径。测试机上单流吞吐从 9.9 Gbit/s 翻倍到 21.9 Gbit/s；在 100 Mbit/s 到 20 Gbit/s 的压测区间内，代理路径始终保持在 direct iperf3 和 iptables REDIRECT 的 95-109 %——v0.11 报告里的早期饱和拐点，对不限带宽的 TCP 已不复存在。",
    ranges: [
      ["100M – 10G", "端到端跑满目标速率，和 direct iperf3 实测几乎没有差距。"],
      [
        "12.5G – 20G",
        "开启 splice 快路径后，代理路径与 direct loopback、iptables REDIRECT 的差距只在测量噪声范围内。",
      ],
      [
        "带限速的规则",
        "限带宽的规则仍走用户态通道——指标、计数器、审计都做到字节级一致。",
      ],
    ],
    capabilitiesTitle: "不只是转发规则，还有团队真正用得上的一切。",
    capabilitiesIntro:
      "规则下发、权限、限速、指标、审计，在 Server 上都是原生能力，不用你自己东拼西凑。",
    capabilities: [
      {
        icon: Network,
        title: "规则",
        text: "转发 TCP 或 UDP，支持单端口、端口范围、IP 或 DNS 目标。HTTPS 可按域名分流，全程不解密连接。",
      },
      {
        icon: ShieldCheck,
        title: "权限",
        text: "可以把每个用户限制在指定的 Client、协议和端口范围内。规则归属明确，并由 Server 强制校验。",
      },
      {
        icon: SlidersHorizontal,
        title: "限额",
        text: "按规则或按用户限制带宽、新连接速率和并发连接数，让一台边缘机能被多方安全共享。",
      },
      {
        icon: Activity,
        title: "可观测",
        text: "Prometheus 指标、结构化日志、审计记录、内置 SQLite，以及备份 / 恢复。",
      },
    ],
    topologyTitle: "一个 Server，多个边缘 Client。",
    topologyIntro:
      "你通过 CLI、API 或 Web UI 与 Server 对话。Server 把签名过的规则下发给拥有公网端口的 Client。业务流量直接经 Client 转发——永远不经过 Server。",
    nodeOperator: "你",
    nodeServer: "Server",
    nodeServerSub: "规则 + 权限",
    nodeClient: "边缘 Client",
    nodeTarget: "目标",
    edgeControl: "规则",
    edgeData: "业务流量",
    workflowTitle: "从安装到上线，只需四步。",
    workflow: [
      "在 Server 上生成一次性的 Client 注册命令。",
      "在拥有公网端口的机器上运行 Client。",
      "通过 CLI、HTTP API 或 Web UI 添加转发规则。",
      "实时查看流量、配额和拒绝事件——全程不触碰用户数据。",
    ],
    finalTitle: "看文档，跑实测，再决定。",
    finalText:
      "性能报告附带完整命令、原始数据和注意事项，方便你按自己的带宽目标先验证再上线。",
    finalCta: "打开文档",
  },
} as const;

export function LandingPage({ locale }: { locale: Locale }) {
  const t = copy[locale];

  return (
    <HomeLayout {...baseOptions(locale)}>
      <main className="fr-landing min-h-screen overflow-hidden antialiased">
        {/* ──────────────────────────  HERO  ────────────────────────── */}
        <section className="fr-hero relative isolate border-b border-white/10">
          {/* dot-grid + radial glow background */}
          <div
            aria-hidden
            className="fr-dotgrid pointer-events-none absolute inset-0"
          />
          <div
            aria-hidden
            className="pointer-events-none absolute inset-x-0 top-0 -z-10 h-[80%]"
            style={{
              background:
                "radial-gradient(60% 50% at 50% 0%, rgba(200,243,111,0.18), transparent 60%), radial-gradient(40% 40% at 80% 20%, rgba(0,210,255,0.10), transparent 60%)",
            }}
          />

          <div className="mx-auto grid max-w-7xl gap-16 px-6 pb-24 pt-12 lg:grid-cols-[1.1fr_0.9fr] lg:items-center lg:px-10 lg:pb-32 lg:pt-20">
            <div className="fr-hero-copy">
              <span className="fr-pill">
                <span className="size-1.5 rounded-full bg-[#c8f36f]" />
                {t.proof}
              </span>
              <h1 className="mt-6 text-balance text-5xl font-semibold tracking-tight md:text-7xl lg:text-[5.5rem] lg:leading-[0.95]">
                {t.headlineLead}
                <br />
                <span className="fr-gradient-text">{t.headlineAccent}</span>
              </h1>
              <p className="mt-7 max-w-xl text-base leading-7 text-white/65 md:text-lg md:leading-8">
                {t.hero}
              </p>
              <div className="mt-9 flex flex-col gap-3 sm:flex-row">
                <Link
                  to="/$lang/docs/$"
                  params={{ lang: locale, _splat: "" }}
                  className="fr-cta-primary"
                >
                  {t.primary}
                  <ArrowRight className="size-4" />
                </Link>
                <Link
                  to="/$lang/docs/$"
                  params={{
                    lang: locale,
                    _splat: "getting-started/performance",
                  }}
                  className="fr-cta-secondary"
                >
                  <Gauge className="size-4" />
                  {t.secondary}
                </Link>
              </div>
            </div>

            {/* HTML/CSS product mock */}
            <ProductMock t={t} />
          </div>

          {/* ranges strip */}
          <div className="border-t border-white/10">
            <div className="mx-auto grid max-w-7xl px-6 lg:grid-cols-3 lg:px-10">
              {t.ranges.map(([range, text], i) => (
                <div
                  key={range}
                  className={`px-2 py-7 ${i > 0 ? "lg:border-l lg:border-white/10 lg:pl-8" : "lg:pr-8"}`}
                >
                  <div className="font-mono text-xl tracking-tight text-white">
                    {range}
                  </div>
                  <div className="mt-2 max-w-sm text-sm leading-6 text-white/55">
                    {text}
                  </div>
                </div>
              ))}
            </div>
          </div>
        </section>

        {/* ──────────────────────────  CAPABILITIES  ────────────────────────── */}
        <section className="border-b border-white/10 bg-black px-6 py-24 lg:px-10 lg:py-32">
          <div className="mx-auto max-w-7xl">
            <div className="grid gap-10 lg:grid-cols-[1fr_1.3fr] lg:items-end">
              <h2 className="text-balance text-3xl font-semibold tracking-tight md:text-5xl">
                {t.capabilitiesTitle}
              </h2>
              <p className="max-w-2xl text-base leading-7 text-white/55 md:text-lg">
                {t.capabilitiesIntro}
              </p>
            </div>
            <div className="mt-16 grid gap-px overflow-hidden rounded-xl border border-white/10 bg-white/5 sm:grid-cols-2">
              {t.capabilities.map((item: Capability) => (
                <div
                  key={item.title}
                  className="group relative bg-black p-7 transition hover:bg-[#0a0a0a]"
                >
                  <div className="mb-5 inline-flex size-10 items-center justify-center rounded-lg border border-white/10 bg-white/[0.03] text-[#c8f36f] transition group-hover:border-[#c8f36f]/40">
                    <item.icon className="size-5" />
                  </div>
                  <h3 className="text-xl font-semibold text-white">
                    {item.title}
                  </h3>
                  <p className="mt-2 max-w-md text-sm leading-7 text-white/55">
                    {item.text}
                  </p>
                </div>
              ))}
            </div>
          </div>
        </section>

        {/* ──────────────────────────  TOPOLOGY  ────────────────────────── */}
        <section className="relative border-b border-white/10 bg-[#050505] px-6 py-24 lg:px-10 lg:py-32">
          <div
            aria-hidden
            className="pointer-events-none absolute inset-0 -z-10"
            style={{
              background:
                "radial-gradient(50% 50% at 50% 50%, rgba(0,210,255,0.06), transparent 60%)",
            }}
          />
          <div className="mx-auto max-w-7xl">
            <div className="grid gap-10 lg:grid-cols-[1fr_1.3fr] lg:items-end">
              <h2 className="text-balance text-3xl font-semibold tracking-tight md:text-5xl">
                {t.topologyTitle}
              </h2>
              <p className="max-w-2xl text-base leading-7 text-white/55 md:text-lg">
                {t.topologyIntro}
              </p>
            </div>
            <div className="fr-topology-panel mt-16 overflow-hidden rounded-xl border border-white/10 bg-black/60 p-8 backdrop-blur md:p-12">
              <Topology t={t} />
            </div>
          </div>
        </section>

        {/* ──────────────────────────  PERFORMANCE BAND  ────────────────────────── */}
        <section className="border-b border-white/10 bg-black px-6 py-24 lg:px-10 lg:py-32">
          <div className="mx-auto grid max-w-7xl gap-12 lg:grid-cols-[0.9fr_1.1fr]">
            <div>
              <h2 className="text-balance text-3xl font-semibold tracking-tight md:text-5xl">
                {t.rangesTitle}
              </h2>
              <p className="mt-6 max-w-xl text-base leading-7 text-white/55 md:text-lg">
                {t.rangesIntro}
              </p>
              <Link
                to="/$lang/docs/$"
                params={{ lang: locale, _splat: "getting-started/performance" }}
                className="mt-8 inline-flex items-center gap-2 text-sm font-medium text-[#c8f36f] transition hover:text-[#dcff8e]"
              >
                {t.secondary}
                <ArrowRight className="size-4" />
              </Link>
            </div>
            <div className="grid content-start gap-0 border-t border-white/10">
              {t.ranges.map(([range, text]) => (
                <div
                  key={range}
                  className="fr-metric-row grid gap-4 border-b border-white/10 py-6 md:grid-cols-[10rem_1fr]"
                >
                  <div className="font-mono text-lg text-[#c8f36f]">
                    {range}
                  </div>
                  <div className="text-sm leading-7 text-white/65">{text}</div>
                </div>
              ))}
            </div>
          </div>
        </section>

        {/* ──────────────────────────  WORKFLOW  ────────────────────────── */}
        <section className="border-b border-white/10 bg-black px-6 py-24 lg:px-10 lg:py-32">
          <div className="mx-auto grid max-w-7xl gap-12 lg:grid-cols-[0.8fr_1.2fr]">
            <h2 className="text-balance text-3xl font-semibold tracking-tight md:text-5xl">
              {t.workflowTitle}
            </h2>
            <ol className="grid gap-0 border-t border-white/10">
              {t.workflow.map((step, index) => (
                <li
                  key={step}
                  className="grid grid-cols-[3.5rem_1fr] items-start border-b border-white/10 py-6"
                >
                  <span className="font-mono text-sm text-[#c8f36f]">
                    {String(index + 1).padStart(2, "0")}
                  </span>
                  <span className="text-base leading-7 text-white/85 md:text-lg">
                    {step}
                  </span>
                </li>
              ))}
            </ol>
          </div>
        </section>

        {/* ──────────────────────────  FINAL CTA  ────────────────────────── */}
        <section className="relative overflow-hidden bg-black px-6 py-24 lg:px-10 lg:py-28">
          <div
            aria-hidden
            className="pointer-events-none absolute inset-0 -z-10"
            style={{
              background:
                "radial-gradient(60% 60% at 50% 100%, rgba(200,243,111,0.16), transparent 60%)",
            }}
          />
          <div className="mx-auto flex max-w-7xl flex-col gap-8 md:flex-row md:items-end md:justify-between">
            <div>
              <h2 className="max-w-3xl text-balance text-3xl font-semibold tracking-tight md:text-5xl">
                {t.finalTitle}
              </h2>
              <p className="mt-5 max-w-2xl text-base leading-7 text-white/55 md:text-lg">
                {t.finalText}
              </p>
            </div>
            <Link
              to="/$lang/docs/$"
              params={{ lang: locale, _splat: "" }}
              className="fr-cta-primary shrink-0"
            >
              {t.finalCta}
              <ArrowRight className="size-4" />
            </Link>
          </div>
        </section>
      </main>
    </HomeLayout>
  );
}

/* ──────────────────────────────────────────────────────────────
   Pure-HTML/CSS product mock.
   Renders a fake product panel: window chrome, rules
   table, throughput bars. No raster assets.
   ────────────────────────────────────────────────────────────── */
function ProductMock({ t }: { t: (typeof copy)["en"] | (typeof copy)["zh"] }) {
  return (
    <div className="fr-product-plane relative">
      {/* gradient halo behind the card */}
      <div
        aria-hidden
        className="pointer-events-none absolute -inset-4 -z-10 rounded-3xl"
        style={{
          background:
            "linear-gradient(135deg, rgba(200,243,111,0.18), rgba(0,210,255,0.10) 60%, transparent)",
          filter: "blur(40px)",
        }}
      />
      <div className="overflow-hidden rounded-xl border border-white/15 bg-[#0a0a0a]/80 shadow-[0_30px_120px_-20px_rgba(200,243,111,0.18)] backdrop-blur">
        {/* window chrome */}
        <div className="flex items-center gap-2 border-b border-white/8 bg-black/40 px-4 py-3">
          <span className="size-2.5 rounded-full bg-[#ff5f57]" />
          <span className="size-2.5 rounded-full bg-[#febc2e]" />
          <span className="size-2.5 rounded-full bg-[#28c840]" />
          <span className="ml-3 font-mono text-xs text-white/40">
            {t.mockTitle}
          </span>
          <span className="ml-auto inline-flex items-center gap-1.5 rounded-full border border-[#c8f36f]/30 bg-[#c8f36f]/10 px-2 py-0.5 text-[10px] font-medium uppercase tracking-wider text-[#c8f36f]">
            <span className="size-1.5 animate-pulse rounded-full bg-[#c8f36f]" />
            {t.mockBadge}
          </span>
        </div>

        <div className="space-y-6 p-5 md:p-6">
          {/* rules block */}
          <div>
            <div className="mb-3 flex items-center justify-between">
              <span className="font-mono text-[10px] uppercase tracking-wider text-white/40">
                {t.rulesHeading}
              </span>
              <Terminal className="size-3.5 text-white/30" />
            </div>
            <div className="space-y-2">
              {t.rules.map((r) => (
                <div
                  key={r.id}
                  className="grid grid-cols-[2rem_2.5rem_1fr_auto] items-center gap-3 rounded-md border border-white/8 bg-white/[0.02] px-3 py-2.5 font-mono text-xs text-white/80"
                >
                  <span className="text-white/35">{r.id}</span>
                  <span
                    className={`rounded px-1.5 py-0.5 text-[10px] font-medium ${r.proto === "TCP" ? "bg-[#c8f36f]/15 text-[#c8f36f]" : "bg-[#00d2ff]/15 text-[#00d2ff]"}`}
                  >
                    {r.proto}
                  </span>
                  <span className="truncate">
                    <span className="text-white">{r.listen}</span>
                    <span className="text-white/30"> → </span>
                    <span className="text-white/65">{r.target}</span>
                  </span>
                  <span className="text-[10px] uppercase tracking-wider text-[#c8f36f]/80">
                    {t.statusActive}
                  </span>
                </div>
              ))}
            </div>
          </div>

          {/* metrics block */}
          <div>
            <div className="mb-3 flex items-center justify-between">
              <span className="font-mono text-[10px] uppercase tracking-wider text-white/40">
                {t.metricsHeading}
              </span>
              <Cpu className="size-3.5 text-white/30" />
            </div>
            <div className="space-y-2.5">
              {t.metricRows.map((m) => (
                <div
                  key={m.label}
                  className="grid grid-cols-[5rem_1fr_5rem] items-center gap-3 font-mono text-xs"
                >
                  <span className="text-white/55">{m.label}</span>
                  <div className="flex h-2 items-center gap-1">
                    {meterSegments.map((segment, i) => (
                      <span
                        key={`${m.label}-${segment}`}
                        className={`h-full flex-1 rounded-sm ${i < m.bars ? "bg-gradient-to-r from-[#c8f36f] to-[#00d2ff]" : "bg-white/8"}`}
                      />
                    ))}
                  </div>
                  <span className="text-right text-white/85">{m.value}</span>
                </div>
              ))}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

/* ──────────────────────────────────────────────────────────────
   SVG topology: operator -> server -> client(s) -> target.
   Pure SVG, themed for the dark canvas, no raster assets.
   ────────────────────────────────────────────────────────────── */
function Topology({ t }: { t: (typeof copy)["en"] | (typeof copy)["zh"] }) {
  return (
    <svg
      viewBox="0 0 1000 320"
      className="w-full"
      role="img"
      aria-label="Portunus topology"
    >
      <defs>
        <linearGradient id="fr-edge-control" x1="0" x2="1" y1="0" y2="0">
          <stop offset="0%" stopColor="#c8f36f" stopOpacity="0.7" />
          <stop offset="100%" stopColor="#c8f36f" stopOpacity="0.2" />
        </linearGradient>
        <linearGradient id="fr-edge-data" x1="0" x2="1" y1="0" y2="0">
          <stop offset="0%" stopColor="#00d2ff" stopOpacity="0.65" />
          <stop offset="100%" stopColor="#00d2ff" stopOpacity="0.2" />
        </linearGradient>
        <marker
          id="fr-arrow-control"
          viewBox="0 0 10 10"
          refX="8"
          refY="5"
          markerWidth="6"
          markerHeight="6"
          orient="auto-start-reverse"
        >
          <path d="M0,0 L10,5 L0,10 Z" fill="#c8f36f" opacity="0.65" />
        </marker>
        <marker
          id="fr-arrow-data"
          viewBox="0 0 10 10"
          refX="8"
          refY="5"
          markerWidth="6"
          markerHeight="6"
          orient="auto-start-reverse"
        >
          <path d="M0,0 L10,5 L0,10 Z" fill="#00d2ff" opacity="0.65" />
        </marker>
      </defs>

      {/* rule edges */}
      <line
        x1="170"
        y1="160"
        x2="320"
        y2="160"
        stroke="url(#fr-edge-control)"
        strokeWidth="1.5"
        strokeDasharray="4 4"
        markerEnd="url(#fr-arrow-control)"
      />
      <line
        x1="480"
        y1="120"
        x2="660"
        y2="80"
        stroke="url(#fr-edge-control)"
        strokeWidth="1.5"
        strokeDasharray="4 4"
        markerEnd="url(#fr-arrow-control)"
      />
      <line
        x1="480"
        y1="160"
        x2="660"
        y2="160"
        stroke="url(#fr-edge-control)"
        strokeWidth="1.5"
        strokeDasharray="4 4"
        markerEnd="url(#fr-arrow-control)"
      />
      <line
        x1="480"
        y1="200"
        x2="660"
        y2="240"
        stroke="url(#fr-edge-control)"
        strokeWidth="1.5"
        strokeDasharray="4 4"
        markerEnd="url(#fr-arrow-control)"
      />

      {/* traffic edges */}
      <line
        x1="820"
        y1="80"
        x2="930"
        y2="80"
        stroke="url(#fr-edge-data)"
        strokeWidth="2"
        markerEnd="url(#fr-arrow-data)"
      />
      <line
        x1="820"
        y1="160"
        x2="930"
        y2="160"
        stroke="url(#fr-edge-data)"
        strokeWidth="2"
        markerEnd="url(#fr-arrow-data)"
      />
      <line
        x1="820"
        y1="240"
        x2="930"
        y2="240"
        stroke="url(#fr-edge-data)"
        strokeWidth="2"
        markerEnd="url(#fr-arrow-data)"
      />

      {/* nodes */}
      <Node x={70} y={160} label={t.nodeOperator} sub="CLI · API · UI" />
      <Node
        x={400}
        y={160}
        label={t.nodeServer}
        sub={t.nodeServerSub}
        highlight
      />
      <Node x={740} y={80} label={t.nodeClient} sub="edge-1" />
      <Node x={740} y={160} label={t.nodeClient} sub="edge-2" />
      <Node x={740} y={240} label={t.nodeClient} sub="edge-3" />
      <Node x={965} y={80} label={t.nodeTarget} muted />
      <Node x={965} y={160} label={t.nodeTarget} muted />
      <Node x={965} y={240} label={t.nodeTarget} muted />

      {/* legend */}
      <g transform="translate(70, 290)">
        <line
          x1="0"
          y1="0"
          x2="24"
          y2="0"
          stroke="#c8f36f"
          strokeWidth="1.5"
          strokeDasharray="4 4"
        />
        <text
          x="32"
          y="4"
          fill="#c8f36f"
          fontSize="11"
          fontFamily="ui-monospace,monospace"
        >
          {t.edgeControl}
        </text>
        <line
          x1="120"
          y1="0"
          x2="144"
          y2="0"
          stroke="#00d2ff"
          strokeWidth="2"
        />
        <text
          x="152"
          y="4"
          fill="#00d2ff"
          fontSize="11"
          fontFamily="ui-monospace,monospace"
        >
          {t.edgeData}
        </text>
      </g>
    </svg>
  );
}

function Node({
  x,
  y,
  label,
  sub,
  highlight,
  muted,
}: {
  x: number;
  y: number;
  label: string;
  sub?: string;
  highlight?: boolean;
  muted?: boolean;
}) {
  const stroke = highlight
    ? "#c8f36f"
    : muted
      ? "rgba(255,255,255,0.18)"
      : "rgba(255,255,255,0.35)";
  const fill = highlight ? "rgba(200,243,111,0.08)" : "rgba(255,255,255,0.04)";
  const textColor = muted ? "rgba(255,255,255,0.55)" : "#fff";
  return (
    <g transform={`translate(${x - 60}, ${y - 24})`}>
      <rect
        width="120"
        height="48"
        rx="8"
        fill={fill}
        stroke={stroke}
        strokeWidth="1"
      />
      <text
        x="60"
        y={sub ? 22 : 30}
        fill={textColor}
        fontSize="13"
        fontWeight="600"
        textAnchor="middle"
      >
        {label}
      </text>
      {sub && (
        <text
          x="60"
          y="36"
          fill="rgba(255,255,255,0.45)"
          fontSize="10"
          fontFamily="ui-monospace,monospace"
          textAnchor="middle"
        >
          {sub}
        </text>
      )}
    </g>
  );
}
