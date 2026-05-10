/// `sessionStorage`-backed token store.
///
/// Token is cleared on browser close (sessionStorage semantics) to bound
/// the theft window to a single browsing session. Spec FR-002 / SC-006.

const TOKEN_KEY = "portunus.token";
const CHANGED_EVENT = "portunus:token-changed";

export function getToken(): string | null {
  try {
    return window.sessionStorage.getItem(TOKEN_KEY);
  } catch {
    return null;
  }
}

export function setToken(token: string): void {
  try {
    window.sessionStorage.setItem(TOKEN_KEY, token);
    window.dispatchEvent(new CustomEvent(CHANGED_EVENT));
  } catch {
    // sessionStorage can throw in private mode / quota exceeded.
    // Silently ignore — auth-gate will surface the error on next 401.
  }
}

export function clearToken(): void {
  try {
    window.sessionStorage.removeItem(TOKEN_KEY);
    window.dispatchEvent(new CustomEvent(CHANGED_EVENT));
  } catch {
    /* ignore */
  }
}

/// Subscribe to in-tab token changes (login / logout). Cross-tab sync
/// via the native `storage` event is wired up too, but
/// `sessionStorage` is per-tab so cross-tab fires only on a hard
/// reload of an unrelated tab.
export function subscribe(cb: () => void): () => void {
  const onLocal = () => cb();
  const onStorage = (e: StorageEvent) => {
    if (e.key === TOKEN_KEY || e.key === null) cb();
  };
  window.addEventListener(CHANGED_EVENT, onLocal);
  window.addEventListener("storage", onStorage);
  return () => {
    window.removeEventListener(CHANGED_EVENT, onLocal);
    window.removeEventListener("storage", onStorage);
  };
}
