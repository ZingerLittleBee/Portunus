import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import "@/i18n";
import { EnrollmentInstallGuide } from "@/components/EnrollmentInstallGuide";
import type { ClientEnrollmentResponse } from "@/api/types";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

beforeEach(() => {
  Object.assign(navigator, {
    clipboard: { writeText: vi.fn().mockResolvedValue(undefined) },
  });
});

function mk(overrides: Partial<ClientEnrollmentResponse> = {}): ClientEnrollmentResponse {
  return {
    client_name: "edge-01",
    expires_at: new Date(Date.now() + 600_000).toISOString(),
    command: "portunus-client enroll 'portunus://host:7443/enroll?code=abc'",
    uri: "portunus://host:7443/enroll?code=abc",
    ...overrides,
  };
}

describe("EnrollmentInstallGuide", () => {
  it("renders the binary and docker tabs and the enroll command verbatim", () => {
    render(<EnrollmentInstallGuide enrollment={mk()} mode="provision" />);
    expect(screen.getByRole("tab", { name: "Binary" })).toBeDefined();
    expect(screen.getByRole("tab", { name: "Docker" })).toBeDefined();
    expect(screen.queryByRole("tab", { name: "systemd" })).toBeNull();
    const enroll = screen.getByTestId("guide-step-binary-enroll").textContent ?? "";
    expect(enroll).toContain("portunus-client enroll 'portunus://host:7443/enroll?code=abc'");
  });

  it("uses the bare uri (not the wrapped command) in the Docker tab", async () => {
    render(<EnrollmentInstallGuide enrollment={mk()} mode="provision" />);
    await userEvent.click(screen.getByRole("tab", { name: "Docker" }));
    const docker = screen.getByTestId("guide-step-docker-enroll").textContent ?? "";
    expect(docker).toContain("enroll 'portunus://host:7443/enroll?code=abc'");
    expect(docker).toContain('--user "$(id -u):$(id -g)"');
    expect(docker).not.toContain("portunus-client enroll 'portunus-client");
  });

  it("shows a live countdown that reaches the expired state", () => {
    vi.useFakeTimers();
    const past = new Date(Date.now() - 1_000).toISOString();
    render(<EnrollmentInstallGuide enrollment={mk({ expires_at: past })} mode="provision" />);
    vi.advanceTimersByTime(1_100);
    expect(screen.getByText(/expired/i)).toBeDefined();
  });

  it("collapses the install step in reenroll mode", () => {
    render(<EnrollmentInstallGuide enrollment={mk()} mode="reenroll" />);
    expect(screen.getByText(/Already installed on this host/i)).toBeDefined();
  });

  it("copies a step command to the clipboard", () => {
    render(<EnrollmentInstallGuide enrollment={mk()} mode="provision" />);
    const firstCopy = screen.getAllByRole("button", { name: /copy/i })[0] as HTMLElement;
    fireEvent.click(firstCopy);
    expect((navigator.clipboard as Clipboard).writeText).toHaveBeenCalled();
  });
});
