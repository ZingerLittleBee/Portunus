// Vitest global setup for happy-dom environment.
//
// happy-dom defines navigator.clipboard as a non-writable getter on the
// prototype chain. Object.assign() (used in unit tests) needs an own writable
// property on the navigator instance so that per-test overrides work.  We
// pre-define clipboard here so that Object.assign({ clipboard: vi.fn() })
// succeeds in beforeEach blocks.
Object.defineProperty(navigator, "clipboard", {
  value: { writeText: () => Promise.resolve(), readText: () => Promise.resolve("") },
  configurable: true,
  writable: true,
});

// happy-dom has no layout engine, so `offsetHeight`/`offsetWidth` are always 0.
// @tanstack/react-virtual (virtual-core 3.17+) sizes its scroll viewport from
// `element.offsetHeight`, so a 0 height makes it render no rows and breaks
// virtualized-list tests (e.g. DataTable). Report a fixed non-zero box so the
// virtualizer has a viewport to fill.
for (const [dim, size] of [
  ["offsetHeight", 800],
  ["offsetWidth", 1000],
] as const) {
  Object.defineProperty(HTMLElement.prototype, dim, {
    configurable: true,
    get() {
      return size;
    },
  });
}
