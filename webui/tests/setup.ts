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
