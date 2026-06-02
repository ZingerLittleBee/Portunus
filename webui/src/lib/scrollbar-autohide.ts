/**
 * Auto-hide scrollbars: reveal the thumb only while the user is actively
 * scrolling. CSS has no ":scrolling" selector, so we toggle `data-scrolling`
 * on <html> and let tokens.css fade the thumb in (and out, after a short
 * idle delay). The thumb is transparent at rest while the track width stays
 * fixed, so revealing it never reflows the layout.
 */

// How long the scrollbar lingers after the last scroll event, in ms.
const HIDE_DELAY_MS = 700;

/**
 * Start listening for scroll activity and toggling `data-scrolling` on the
 * document element. Returns a teardown function that removes the listener and
 * clears any pending timer.
 */
export function initScrollbarAutoHide(): () => void {
  const root = document.documentElement;
  let hideTimer: ReturnType<typeof window.setTimeout> | undefined;

  const handleScroll = () => {
    root.setAttribute("data-scrolling", "");
    if (hideTimer !== undefined) window.clearTimeout(hideTimer);
    hideTimer = window.setTimeout(() => {
      root.removeAttribute("data-scrolling");
    }, HIDE_DELAY_MS);
  };

  // capture:true catches scrolls on any nested overflow container, not just
  // the window; passive:true guarantees we never block scrolling.
  window.addEventListener("scroll", handleScroll, {
    capture: true,
    passive: true,
  });

  return () => {
    window.removeEventListener("scroll", handleScroll, { capture: true });
    if (hideTimer !== undefined) window.clearTimeout(hideTimer);
  };
}
