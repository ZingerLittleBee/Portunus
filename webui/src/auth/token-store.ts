const LEGACY_TOKEN_KEY = "portunus.token";
const CHANGED_EVENT = "portunus:token-changed";

export function getToken(): string | null {
  try {
    return window.sessionStorage.getItem(LEGACY_TOKEN_KEY);
  } catch {
    return null;
  }
}

export function setToken(token: string): void {
  try {
    window.sessionStorage.setItem(LEGACY_TOKEN_KEY, token);
    window.dispatchEvent(new CustomEvent(CHANGED_EVENT));
  } catch {
    // sessionStorage can throw in private mode / quota exceeded.
    // Silently ignore — auth-gate will surface the error on next 401.
  }
}

export function clearToken(): void {
  clearLegacyToken();
}

export function clearLegacyToken(): void {
  try {
    window.sessionStorage.removeItem(LEGACY_TOKEN_KEY);
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
    if (e.key === LEGACY_TOKEN_KEY || e.key === null) cb();
  };
  window.addEventListener(CHANGED_EVENT, onLocal);
  window.addEventListener("storage", onStorage);
  return () => {
    window.removeEventListener(CHANGED_EVENT, onLocal);
    window.removeEventListener("storage", onStorage);
  };
}
