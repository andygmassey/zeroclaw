import { Routes, Route, Navigate } from 'react-router-dom';
import { useState, useEffect, createContext, useContext, Component, type ReactNode, type ErrorInfo } from 'react';
import { ThemeProvider } from './contexts/ThemeContext';
import Layout from './components/layout/Layout';
import Dashboard from './pages/Dashboard';
import AgentChat from './pages/AgentChat';
import Timeline from './pages/Timeline';
import People from './pages/People';
import Tools from './pages/Tools';
import Cron from './pages/Cron';
import Integrations from './pages/Integrations';
import Memory from './pages/Memory';
import Config from './pages/Config';
import Cost from './pages/Cost';
import Logs from './pages/Logs';
import Doctor from './pages/Doctor';
import Pairing from './pages/Pairing';
import Canvas from './pages/Canvas';
import Wiki from './pages/Wiki';
import LetterAvatar from './components/brand/LetterAvatar';
import { AuthProvider, useAuth } from './hooks/useAuth';
import { DraftContext, useDraftStore } from './hooks/useDraft';
import { setLocale, t, type Locale } from './lib/i18n';
import { loadLocale, saveLocale } from './contexts/ThemeContext';
import { getAdminPairCode, getPublicHealth } from './lib/api';

// Locale context
interface LocaleContextType {
  locale: string;
  setAppLocale: (locale: string) => void;
}

export const LocaleContext = createContext<LocaleContextType>({
  locale: 'en',
  setAppLocale: () => {},
});

export const useLocaleContext = () => useContext(LocaleContext);

// ---------------------------------------------------------------------------
// Error boundary: catches render crashes and shows a recoverable message
// instead of a black screen
// ---------------------------------------------------------------------------

interface ErrorBoundaryState {
  error: Error | null;
}

export class ErrorBoundary extends Component<
  { children: ReactNode },
  ErrorBoundaryState
> {
  constructor(props: { children: ReactNode }) {
    super(props);
    this.state = { error: null };
  }

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error('[Ostler] Render error:', error, info.componentStack);
  }

  render() {
    if (this.state.error) {
      // V1.1 §3.16 error-state — accent-soft background + accent-soft-2 border,
      // ghost "Try again" button.
      return (
        <div style={{ padding: 'var(--space-4)' }}>
          <div
            className="w-full max-w-lg"
            style={{
              background: 'var(--accent-soft)',
              border: '1px solid var(--accent-soft-2)',
              borderRadius: 'var(--radius-card)',
              padding: 'var(--space-4)',
            }}
          >
            <h2
              style={{
                margin: 0,
                marginBottom: 'var(--space-2)',
                fontFamily: 'var(--f-display)',
                fontWeight: 600,
                fontSize: '18px',
                lineHeight: '24px',
                color: 'var(--accent)',
              }}
            >
              Something went wrong
            </h2>
            <p
              style={{
                margin: 0,
                marginBottom: 'var(--space-3)',
                fontFamily: 'var(--f-body)',
                fontSize: '13px',
                lineHeight: '20px',
                color: 'var(--ink-muted)',
              }}
            >
              A render error occurred. Check the browser console for details.
            </p>
            <pre
              style={{
                fontFamily: 'var(--f-mono)',
                fontSize: '11px',
                background: 'var(--panel)',
                color: 'var(--accent)',
                border: '1px solid var(--hairline)',
                borderRadius: 'var(--radius-chip)',
                padding: 'var(--space-3)',
                overflowX: 'auto',
                whiteSpace: 'pre-wrap',
                wordBreak: 'break-all',
              }}
            >
              {this.state.error.message}
            </pre>
            <button
              onClick={() => this.setState({ error: null })}
              className="btn-ghost"
              style={{ marginTop: 'var(--space-3)' }}
            >
              Try again
            </button>
          </div>
        </div>
      );
    }
    return this.props.children;
  }
}

// Pairing dialog component
function PairingDialog({ onPair }: { onPair: (code: string) => Promise<void> }) {
  const [code, setCode] = useState('');
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);
  const [displayCode, setDisplayCode] = useState<string | null>(null);
  const [codeLoading, setCodeLoading] = useState(true);

  // Fetch the current pairing code (public endpoint works in Docker too)
  useEffect(() => {
    let cancelled = false;
    getAdminPairCode()
      .then((data) => {
        if (!cancelled && data.pairing_code) {
          setDisplayCode(data.pairing_code);
          setCode(data.pairing_code); // auto-fill so user just clicks "Pair"
        }
      })
      .catch(() => {
        // Endpoint not reachable; user must check terminal / docker logs.
      })
      .finally(() => {
        if (!cancelled) setCodeLoading(false);
      });
    return () => { cancelled = true; };
  }, []);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError('');
    try {
      await onPair(code);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Pairing failed');
    } finally {
      setLoading(false);
    }
  };

  return (
    <div
      className="min-h-screen flex items-center justify-center"
      style={{ background: 'var(--chassis)', padding: 'var(--space-5)' }}
    >
      {/* Lift-card pattern — white tile on cream chassis, single shadow. */}
      <div
        className="lift-card w-full max-w-md animate-fade-in-scale"
        style={{ padding: 'var(--space-5)', marginBottom: 0 }}
      >
        <div className="text-center" style={{ marginBottom: 'var(--space-4)' }}>
          {/* Circular O marque — V1.1 §3.6 Instance A "product mark" */}
          <div style={{ display: 'flex', justifyContent: 'center', marginBottom: 'var(--space-3)' }}>
            <LetterAvatar instance="product" letter="O" size={64} ariaLabel="Ostler" />
          </div>
          <h1
            style={{
              margin: 0,
              marginBottom: 'var(--space-2)',
              fontFamily: 'var(--f-display)',
              fontWeight: 600,
              fontSize: '24px',
              lineHeight: '30px',
              letterSpacing: '-0.3px',
              color: 'var(--ink)',
            }}
          >
            Ostler
          </h1>
          <p
            style={{
              margin: 0,
              fontFamily: 'var(--f-body)',
              fontSize: '13px',
              lineHeight: '20px',
              color: 'var(--ink-muted)',
            }}
          >
            {displayCode ? t('app.pairing_subtitle_code_ready') : t('app.pairing_subtitle_enter_code')}
          </p>
        </div>

        {/* Show the pairing code if available (localhost) */}
        {!codeLoading && displayCode && (
          <div
            style={{
              marginBottom: 'var(--space-4)',
              padding: 'var(--space-3)',
              borderRadius: 'var(--radius-card)',
              background: 'var(--accent-soft)',
              border: '1px solid var(--accent-soft-2)',
              textAlign: 'center',
            }}
          >
            <div
              style={{
                fontFamily: 'var(--f-mono)',
                fontWeight: 500,
                fontSize: '36px',
                lineHeight: '44px',
                letterSpacing: '0.4em',
                color: 'var(--ink)',
                padding: '4px 0',
              }}
            >
              {displayCode}
            </div>
            <p
              style={{
                margin: 0,
                marginTop: '4px',
                fontFamily: 'var(--f-body)',
                fontSize: '12px',
                lineHeight: '16px',
                color: 'var(--ink-muted)',
              }}
            >
              Enter this code below or on another device
            </p>
          </div>
        )}

        <form onSubmit={handleSubmit}>
          <input
            type="text"
            value={code}
            onChange={(e) => setCode(e.target.value)}
            placeholder="6-digit code"
            className="w-full"
            style={{
              padding: '12px 16px',
              textAlign: 'center',
              fontFamily: 'var(--f-mono)',
              fontSize: '20px',
              letterSpacing: '0.3em',
              fontWeight: 500,
              marginBottom: 'var(--space-3)',
              background: 'var(--panel)',
              border: '1px solid var(--hairline-soft)',
              borderRadius: 'var(--radius-control)',
              color: 'var(--ink)',
            }}
            maxLength={6}
            autoFocus
          />
          {error && (
            <p
              aria-live="polite"
              className="animate-fade-in"
              style={{
                fontFamily: 'var(--f-body)',
                fontSize: '13px',
                lineHeight: '20px',
                color: 'var(--accent)',
                textAlign: 'center',
                marginBottom: 'var(--space-3)',
              }}
            >
              {error}
            </p>
          )}
          <button
            type="submit"
            disabled={loading || code.length < 6}
            className="btn-primary w-full"
          >
            {loading ? (
              <span className="flex items-center justify-center gap-2">
                <span
                  className="border-2 rounded-full animate-spin"
                  style={{
                    width: '14px',
                    height: '14px',
                    borderColor: 'rgba(255,255,255,0.35)',
                    borderTopColor: '#fff',
                  }}
                />
                Pairing...
              </span>
            ) : (
              'Pair'
            )}
          </button>
        </form>
      </div>
    </div>
  );
}

// Hook: poll the gateway /health endpoint until it responds, so the bundled
// Tauri shell can wait for the launchd-managed Hub service to come online
// after the user opens the .app fresh.
function useGatewayReady(): { ready: boolean; attempts: number } {
  const [ready, setReady] = useState(false);
  const [attempts, setAttempts] = useState(0);

  useEffect(() => {
    let cancelled = false;

    const probe = async () => {
      try {
        await getPublicHealth();
        if (!cancelled) setReady(true);
      } catch {
        if (!cancelled) {
          setAttempts((n) => n + 1);
        }
      }
    };

    probe();
    const interval = setInterval(() => {
      if (!cancelled && !ready) probe();
    }, 2500);

    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [ready]);

  return { ready, attempts };
}

function HubStartingScreen({ attempts }: { attempts: number }) {
  return (
    <div
      className="min-h-screen flex items-center justify-center"
      style={{ background: 'var(--chassis)' }}
    >
      <div className="flex flex-col items-center gap-3 animate-fade-in text-center">
        <LetterAvatar instance="product" letter="O" size={56} ariaLabel="Ostler" />
        <div
          className="rounded-full animate-spin"
          style={{
            width: '32px',
            height: '32px',
            border: '1.5px solid var(--accent-soft-2)',
            borderTopColor: 'var(--accent)',
            marginTop: '4px',
          }}
        />
        <p
          style={{
            margin: 0,
            fontFamily: 'var(--f-body)',
            fontWeight: 500,
            fontSize: '14px',
            lineHeight: '22px',
            color: 'var(--ink)',
          }}
        >
          {t('app.hub_starting')}
        </p>
        <p
          style={{
            margin: 0,
            maxWidth: '320px',
            fontFamily: 'var(--f-body)',
            fontSize: '13px',
            lineHeight: '20px',
            color: 'var(--ink-muted)',
          }}
        >
          {t('app.hub_starting_hint')}
        </p>
        {attempts > 4 && (
          <p
            style={{
              margin: 0,
              fontFamily: 'var(--f-mono)',
              fontSize: '10px',
              lineHeight: '14px',
              textTransform: 'uppercase',
              letterSpacing: '0.08em',
              color: 'var(--ink-subdued)',
            }}
          >
            Attempt {attempts}
          </p>
        )}
      </div>
    </div>
  );
}

function AppContent() {
  const { isAuthenticated, requiresPairing, loading, pair, logout } = useAuth();
  const [locale, setLocaleState] = useState(loadLocale());
  const draftStore = useDraftStore();
  const { ready: gatewayReady, attempts: gatewayAttempts } = useGatewayReady();
  setLocale(locale as Locale);

  const setAppLocale = (newLocale: string) => {
    setLocaleState(newLocale);
    setLocale(newLocale as Locale);
    saveLocale(newLocale);
  };

  // Listen for 401 events to force logout
  useEffect(() => {
    const handler = () => {
      logout();
    };
    window.addEventListener('ostler-unauthorized', handler);
    return () => window.removeEventListener('ostler-unauthorized', handler);
  }, [logout]);

  if (!gatewayReady) {
    return <HubStartingScreen attempts={gatewayAttempts} />;
  }

  if (loading) {
    return (
      <div
        className="min-h-screen flex items-center justify-center"
        style={{ background: 'var(--chassis)' }}
      >
        <div className="flex flex-col items-center gap-3 animate-fade-in">
          <div
            className="rounded-full animate-spin"
            style={{
              width: '28px',
              height: '28px',
              border: '1.5px solid var(--accent-soft-2)',
              borderTopColor: 'var(--accent)',
            }}
          />
          <p
            style={{
              margin: 0,
              fontFamily: 'var(--f-body)',
              fontSize: '13px',
              lineHeight: '20px',
              color: 'var(--ink-muted)',
            }}
          >
            Connecting...
          </p>
        </div>
      </div>
    );
  }

  if (!isAuthenticated && requiresPairing) {
    return <PairingDialog onPair={pair} />;
  }

  return (
    <DraftContext.Provider value={draftStore}>
      <LocaleContext.Provider value={{ locale, setAppLocale }}>
        <Routes>
          <Route element={<Layout />}>
            {/* V1.1 §5.1 nav surfaces (9 items) */}
            <Route path="/" element={<Dashboard />} />
            <Route path="/chat" element={<AgentChat />} />
            <Route path="/timeline" element={<Timeline />} />
            <Route path="/people" element={<People />} />
            <Route path="/wiki" element={<Wiki />} />
            <Route path="/memory" element={<Memory />} />
            <Route path="/integrations" element={<Integrations />} />
            <Route path="/health" element={<Doctor />} />
            <Route path="/settings" element={<Config />} />

            {/* Legacy aliases (URL-direct only — hidden from nav per V1.1 §5.1) */}
            <Route path="/agent" element={<AgentChat />} />
            <Route path="/doctor" element={<Doctor />} />
            <Route path="/config" element={<Config />} />
            <Route path="/tools" element={<Tools />} />
            <Route path="/cron" element={<Cron />} />
            <Route path="/cost" element={<Cost />} />
            <Route path="/logs" element={<Logs />} />
            <Route path="/canvas" element={<Canvas />} />
            <Route path="/pairing" element={<Pairing />} />

            <Route path="*" element={<Navigate to="/" replace />} />
          </Route>
        </Routes>
      </LocaleContext.Provider>
    </DraftContext.Provider>
  );
}

export default function App() {
  return (
    <AuthProvider>
      <ThemeProvider>
        <AppContent />
      </ThemeProvider>
    </AuthProvider>
  );
}
