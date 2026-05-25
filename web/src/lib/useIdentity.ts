// CX-81 B6: React hook for the customer identity surfaced at
// `GET /api/identity`.
//
// Used by:
//   - the Tauri Hub chat header (B3 reskin) -- to render
//     {{user_assistant_name}} in place of "Your assistant"
//   - the Tauri Hub sidebar (B7 wiki tab) -- to render
//     `firstNameToPediaLabel(user_first_name)` in place of "Wiki"
//   - the Settings → Identity editor (future, when added)
//
// Behaviour:
//   - Fetches once on mount, caches in module-level state.
//   - Returns `{ user_first_name: null, user_assistant_name: null,
//     loading: true }` while the fetch is in flight.
//   - Failure is silent (the API returns null fields → surfaces use the
//     "Personal wiki" / "Your assistant" fallback labels).
//   - Identity changes are rare; no automatic re-fetch / no polling. When
//     the Settings page adds an editor it can call `refreshIdentity()` to
//     force a re-fetch.

import { useEffect, useState } from 'react';
import { apiFetch } from './api';

export interface Identity {
  user_first_name: string | null;
  user_assistant_name: string | null;
}

// Module-level cache so multiple components sharing the hook do not each
// fire their own fetch. This is intentional -- identity is global, not
// per-component state.
let cached: Identity | null = null;
const listeners = new Set<(value: Identity) => void>();

async function loadIdentity(): Promise<Identity> {
  try {
    const body = await apiFetch<Identity>('/api/identity');
    return {
      user_first_name: body.user_first_name ?? null,
      user_assistant_name: body.user_assistant_name ?? null,
    };
  } catch {
    // Any failure (401, 5xx, network) collapses to "identity unset" so the
    // surfaces (chat header, sidebar) render the "Personal wiki" /
    // "Your assistant" fallback labels rather than an error state.
    return { user_first_name: null, user_assistant_name: null };
  }
}

/**
 * Force a re-fetch of `/api/identity`. Call after the customer edits
 * either name in the Settings page so the chat header + sidebar pick up
 * the new value without a full reload.
 */
export async function refreshIdentity(): Promise<void> {
  const next = await loadIdentity();
  cached = next;
  listeners.forEach((cb) => cb(next));
}

export interface UseIdentityResult extends Identity {
  loading: boolean;
}

export function useIdentity(): UseIdentityResult {
  const [identity, setIdentity] = useState<Identity | null>(cached);
  const [loading, setLoading] = useState<boolean>(cached == null);

  useEffect(() => {
    let alive = true;
    const onUpdate = (next: Identity) => {
      if (alive) setIdentity(next);
    };
    listeners.add(onUpdate);

    if (cached == null) {
      loadIdentity().then((value) => {
        if (!alive) return;
        cached = value;
        setIdentity(value);
        setLoading(false);
        listeners.forEach((cb) => cb(value));
      });
    } else {
      setLoading(false);
    }

    return () => {
      alive = false;
      listeners.delete(onUpdate);
    };
  }, []);

  return {
    user_first_name: identity?.user_first_name ?? null,
    user_assistant_name: identity?.user_assistant_name ?? null,
    loading,
  };
}
