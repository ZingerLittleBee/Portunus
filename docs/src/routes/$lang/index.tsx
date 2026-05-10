import { createFileRoute } from "@tanstack/react-router";
import { LandingPage } from "@/components/landing";

export const Route = createFileRoute("/$lang/")({
  component: Home,
});

function Home() {
  const { lang } = Route.useParams();
  return <LandingPage locale={lang === "zh" ? "zh" : "en"} />;
}
