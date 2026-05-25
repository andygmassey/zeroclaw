import { useState, useEffect, useCallback, useRef } from 'react';
import { BookOpen, RefreshCw } from 'lucide-react';
import { t } from '@/lib/i18n';

// The customer's local MkDocs Material wiki is served from a sibling
// container/process on loopback port 8044. The Hub embeds it as an
// interior page via <iframe> — the customer never opens a separate
// browser to see their wiki.
const WIKI_ORIGIN = 'http://127.0.0.1:8044';
const WIKI_URL = `${WIKI_ORIGIN}/`;

// How long to wait before showing the loading-state text. Below this
// threshold we hide the spinner to avoid flashing it on fast loads.
const LOADING_HINT_DELAY_MS = 2000;

// How often we re-probe the wiki when it is unreachable. The wiki
// container may still be starting on a fresh install, so we keep
// trying for a bit before giving up.
const PROBE_INTERVAL_MS = 2500;

type ProbeState = 'probing' | 'ready' | 'error';

/**
 * Hub Wiki page.
 *
 * Mounts a probe that fetches `WIKI_URL` in `no-cors` mode (the wiki
 * does not set Access-Control-Allow-Origin headers, so a regular fetch
 * would CORS-fail). A `no-cors` fetch returns an opaque response on
 * success and throws on network failure — that is exactly the
 * reachability signal we want.
 *
 * Once the probe succeeds we mount the iframe. If the probe fails we
 * show the error-state with a Retry button.
 */
export default function Wiki() {
  const [probeState, setProbeState] = useState<ProbeState>('probing');
  const [showLoadingHint, setShowLoadingHint] = useState(false);
  const [retryNonce, setRetryNonce] = useState(0);
  const mountedRef = useRef(true);

  // Track mounted state so async probes do not setState after unmount.
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  // Probe the wiki for reachability. We use `no-cors` mode because the
  // wiki does not advertise CORS headers and we do not need the body —
  // we just want to know the socket answers.
  const probe = useCallback(async () => {
    try {
      await fetch(WIKI_URL, { mode: 'no-cors', cache: 'no-store' });
      if (mountedRef.current) {
        setProbeState('ready');
      }
    } catch {
      if (mountedRef.current) {
        setProbeState('error');
      }
    }
  }, []);

  useEffect(() => {
    setProbeState('probing');
    setShowLoadingHint(false);

    const hintTimer = setTimeout(() => {
      if (mountedRef.current) {
        setShowLoadingHint(true);
      }
    }, LOADING_HINT_DELAY_MS);

    probe();

    // While probing, retry on an interval (the wiki container may still
    // be coming up). Stop retrying once we leave the probing state.
    const interval = setInterval(() => {
      if (mountedRef.current) {
        setProbeState((prev) => {
          if (prev === 'probing') {
            probe();
          }
          return prev;
        });
      }
    }, PROBE_INTERVAL_MS);

    return () => {
      clearTimeout(hintTimer);
      clearInterval(interval);
    };
  }, [probe, retryNonce]);

  const handleRetry = () => {
    setRetryNonce((n) => n + 1);
  };

  return (
    <div className="flex-1 h-full overflow-hidden flex flex-col">
      {probeState === 'ready' ? (
        <iframe
          src={WIKI_URL}
          title={t('wiki.iframe_title')}
          // The wiki is local and trusted. We allow scripts (MkDocs
          // Material needs them), same-origin (relative link navigation
          // inside the iframe), forms (search submits), and popups
          // (the wiki may open external research links — those route
          // back to the OS browser via Tauri's shell:allow-open).
          sandbox="allow-scripts allow-same-origin allow-forms allow-popups"
          className="flex-1 w-full border-0"
          style={{ background: 'var(--pc-bg-base)' }}
        />
      ) : probeState === 'probing' ? (
        <WikiLoadingState showHint={showLoadingHint} />
      ) : (
        <WikiErrorState onRetry={handleRetry} />
      )}
    </div>
  );
}

function WikiLoadingState({ showHint }: { showHint: boolean }) {
  return (
    <div className="flex-1 flex items-center justify-center" style={{ background: 'var(--pc-bg-base)' }}>
      <div className="flex flex-col items-center gap-4 text-center max-w-md px-6 animate-fade-in">
        <BookOpen className="h-8 w-8 opacity-40" style={{ color: 'var(--pc-text-muted)' }} />
        <div
          className="h-10 w-10 border-2 rounded-full animate-spin"
          style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }}
        />
        {showHint && (
          <>
            <p className="text-sm font-medium" style={{ color: 'var(--pc-text-primary)' }}>
              {t('wiki.loading_title')}
            </p>
            <p className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
              {t('wiki.loading_hint')}
            </p>
          </>
        )}
      </div>
    </div>
  );
}

function WikiErrorState({ onRetry }: { onRetry: () => void }) {
  return (
    <div className="flex-1 flex items-center justify-center" style={{ background: 'var(--pc-bg-base)' }}>
      <div className="flex flex-col items-center gap-4 text-center max-w-md px-6 animate-fade-in">
        <BookOpen className="h-8 w-8 opacity-40" style={{ color: 'var(--pc-text-muted)' }} />
        <p className="text-base font-semibold" style={{ color: 'var(--pc-text-primary)' }}>
          {t('wiki.error_title')}
        </p>
        <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
          {t('wiki.error_hint')}
        </p>
        <button
          onClick={onRetry}
          className="inline-flex items-center gap-2 px-4 py-2 rounded-lg text-sm font-medium transition-colors"
          style={{ background: 'var(--pc-accent)', color: '#fff' }}
        >
          <RefreshCw className="h-4 w-4" />
          {t('wiki.error_retry')}
        </button>
      </div>
    </div>
  );
}
