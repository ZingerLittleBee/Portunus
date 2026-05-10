import { createFileRoute } from "@tanstack/react-router";
import { LandingPage } from "@/components/landing";

export const Route = createFileRoute("/")({
  component: Home,
});

function Home() {
  return <LandingPage locale="en" />;
}
