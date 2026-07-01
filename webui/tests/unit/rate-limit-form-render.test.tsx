/// 011-rate-limiting-qos T036: render-level test for RateLimitForm.
/// Asserts the four cap inputs are visible by default and that the
/// three burst overrides remain hidden behind the "Advanced"
/// disclosure until expanded — preserves the disclosure invariant
/// from data-model.md §1.1.

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

afterEach(cleanup);

import "@/i18n";
import { RateLimitForm } from "@/components/RateLimitForm";
import {
  EMPTY_RATE_LIMIT_FORM,
  type RateLimitFormState,
} from "@/components/RateLimitForm.helpers";

function Harness({ initial = EMPTY_RATE_LIMIT_FORM }: { initial?: RateLimitFormState }) {
  // Render-only harness — exercises the visual contract; the parent
  // component is responsible for state in production usage.
  return <RateLimitForm state={initial} onChange={() => undefined} />;
}

describe("RateLimitForm", () => {
  it("renders all four cap inputs visibly by default", () => {
    render(<Harness />);
    expect(screen.getByLabelText(/Bandwidth in/i)).toBeDefined();
    expect(screen.getByLabelText(/Bandwidth out/i)).toBeDefined();
    expect(screen.getByLabelText(/New connections \/ sec/i)).toBeDefined();
    expect(screen.getByLabelText(/Concurrent connections/i)).toBeDefined();
  });

  it("hides burst overrides behind the Advanced disclosure on a fresh form", () => {
    render(<Harness />);
    expect(screen.queryByLabelText(/Bandwidth in burst/i)).toBeNull();
    expect(screen.queryByLabelText(/Bandwidth out burst/i)).toBeNull();
    expect(screen.queryByLabelText(/New connections burst/i)).toBeNull();
  });

  it("reveals burst inputs after clicking the Advanced toggle", () => {
    render(<Harness />);
    fireEvent.click(screen.getByText(/Advanced \(burst overrides\)/i));
    expect(screen.getByLabelText(/Bandwidth in burst/i)).toBeDefined();
    expect(screen.getByLabelText(/Bandwidth out burst/i)).toBeDefined();
    expect(screen.getByLabelText(/New connections burst/i)).toBeDefined();
  });

  it("describes the same burst range enforced by the API", () => {
    render(<Harness />);
    fireEvent.click(screen.getByText(/Advanced \(burst overrides\)/i));
    expect(screen.getByText("Bursts must be between rate / 100 and rate × 60.")).toBeDefined();
  });

  it("auto-opens Advanced when prefilled state already carries a burst override", () => {
    // Editing a rule that already has overrides must not visually
    // drop them — the disclosure hydrates open from state, not from
    // a separate prop, so callers don't need to thread an extra flag.
    render(
      <Harness
        initial={{
          ...EMPTY_RATE_LIMIT_FORM,
          bandwidth_in_bps: "1000000",
          bandwidth_in_burst: "2000000",
        }}
      />,
    );
    expect(screen.getByLabelText(/Bandwidth in burst/i)).toBeDefined();
  });

  it("does NOT render a concurrent_connections_burst field (reserved per spec)", () => {
    render(<Harness />);
    fireEvent.click(screen.getByText(/Advanced \(burst overrides\)/i));
    // The reserved field is rejected server-side with
    // `validation.rate_limit_burst_unsupported`; surfacing it in
    // the UI would let operators submit invalid bodies. The
    // form deliberately omits it.
    expect(screen.queryByLabelText(/Concurrent connections burst/i)).toBeNull();
  });
});
