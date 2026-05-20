// Tauri detection utilities for the Ostler Hub desktop shell.
//
// The window globals are still named with the legacy `__ZEROCLAW_*` prefix
// because the gateway injects them by that name; renaming requires a
// coordinated change on the gateway template side.

declare global {
  interface Window {
    __TAURI__?: unknown;
    __ZEROCLAW_GATEWAY__?: string;
  }
}

/** Returns true when running inside a Tauri WebView. */
export const isTauri = (): boolean => '__TAURI__' in window;

/** Gateway base URL when running inside Tauri (defaults to localhost). */
export const tauriGatewayUrl = (): string =>
  window.__ZEROCLAW_GATEWAY__ ?? 'http://127.0.0.1:42617';
