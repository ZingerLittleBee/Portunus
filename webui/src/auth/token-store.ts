const LEGACY_TOKEN_KEY = "portunus.token";

export function clearLegacyToken(): void {
  try {
    window.sessionStorage.removeItem(LEGACY_TOKEN_KEY);
  } catch {
    /* ignore */
  }
}
