/**
 * Timeline — V1.1 §3.5 timeline-row + §5.1 nav item #3.
 *
 * Wires to the PWG Gateway `/api/timeline` endpoint when available; falls
 * back to a tasteful empty-state when the Hub hasn't been paired or the
 * gateway hasn't returned any events yet.
 *
 * Visual spec (Fixup 6):
 *   - Vertical rail (timeline spine) is 1.5px @ var(--ink-faint),
 *     NOT 1px @ var(--hairline). Bumped per Andy's R5 feedback —
 *     hairline read too faint.
 */

import { useEffect, useState } from 'react';
import { Clock } from 'lucide-react';
import { apiFetch } from '@/lib/api';

interface TimelineEvent {
  id: string;
  time: string;       // ISO 8601 or "14:30" — render as-is
  action: string;
  detail?: string;
  pill?: { label: string; variant?: 'strong' | 'active' | 'dormant' | 'lost' | 'new' };
  is_now?: boolean;
  is_future?: boolean;
}

interface TimelineResponse {
  events: TimelineEvent[];
}

export default function Timeline() {
  const [events, setEvents] = useState<TimelineEvent[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const res = await apiFetch<TimelineResponse>('/api/timeline');
        if (!cancelled) {
          setEvents(res.events ?? []);
          setError(null);
        }
      } catch (e) {
        if (!cancelled) {
          // Empty-state is fine — `apiFetch` returning an error simply means
          // the endpoint isn't wired yet, not that the page is broken.
          setEvents([]);
          setError(e instanceof Error ? e.message : 'Unable to load timeline');
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <div style={{ padding: 'var(--space-5)' }}>
      <div className="section-header">
        <div>
          <div className="strap">PERSONAL</div>
          <h1 className="title">Timeline</h1>
        </div>
        <div className="meta">
          {events.length > 0 ? `${events.length} EVENTS` : ''}
        </div>
      </div>

      {loading && (
        <div className="loading-state" style={{ padding: 'var(--space-4) 0' }}>
          <span
            className="rounded-full animate-spin"
            style={{
              display: 'inline-block',
              width: '14px',
              height: '14px',
              border: '1.5px solid var(--accent-soft-2)',
              borderTopColor: 'var(--accent)',
              marginRight: 'var(--space-2)',
            }}
          />
          <span
            style={{
              fontFamily: 'var(--f-body)',
              fontSize: '13px',
              color: 'var(--ink-muted)',
            }}
          >
            Loading
          </span>
        </div>
      )}

      {!loading && events.length === 0 && (
        <div className="empty-state" style={{ padding: 'var(--space-5)' }}>
          <Clock
            style={{
              width: '40px',
              height: '40px',
              margin: '0 auto var(--space-3)',
              color: 'var(--ink-faint)',
              strokeWidth: 1.5,
              display: 'block',
            }}
          />
          <div
            style={{
              fontFamily: 'var(--f-body)',
              fontSize: '14px',
              color: 'var(--ink-muted)',
              marginBottom: '4px',
            }}
          >
            Your timeline is empty
          </div>
          <div
            style={{
              fontFamily: 'var(--f-body)',
              fontSize: '13px',
              color: 'var(--ink-subdued)',
            }}
          >
            Events from your calendar, messages and notes will appear here as
            they're captured.
          </div>
          {error && (
            <div
              style={{
                marginTop: 'var(--space-3)',
                fontFamily: 'var(--f-mono)',
                fontSize: '10px',
                textTransform: 'uppercase',
                letterSpacing: '0.08em',
                color: 'var(--ink-subdued)',
              }}
            >
              {error}
            </div>
          )}
        </div>
      )}

      {!loading && events.length > 0 && (
        <div className="timeline">
          {events.map((ev, idx) => (
            <TimelineRow key={ev.id} event={ev} isLast={idx === events.length - 1} />
          ))}
        </div>
      )}
    </div>
  );
}

function TimelineRow({ event, isLast }: { event: TimelineEvent; isLast: boolean }) {
  return (
    <div
      style={{
        display: 'grid',
        gridTemplateColumns: '60px 20px 1fr 100px',
        gap: 'var(--space-3)',
        alignItems: 'flex-start',
        padding: 'var(--space-3) 0',
      }}
    >
      <div
        style={{
          fontFamily: 'var(--f-mono)',
          fontSize: '10px',
          lineHeight: '14px',
          textTransform: 'uppercase',
          letterSpacing: '0.08em',
          color: 'var(--ink-muted)',
          textAlign: 'right',
          paddingTop: '4px',
        }}
      >
        {event.time}
      </div>
      <div style={{ position: 'relative', height: '100%', minHeight: '24px' }}>
        {/* Spine — 1.5px ink-faint per Fixup 6 */}
        {!isLast && (
          <div
            style={{
              position: 'absolute',
              left: '50%',
              top: 0,
              bottom: '-16px',
              width: '1.5px',
              background: 'var(--ink-faint)',
              transform: 'translateX(-0.75px)',
            }}
          />
        )}
        <div
          style={{
            position: 'absolute',
            left: '50%',
            top: '6px',
            width: '10px',
            height: '10px',
            borderRadius: '9999px',
            background: event.is_now ? 'var(--ink)' : 'transparent',
            border: '1px solid',
            borderColor: event.is_now ? 'var(--ink)' : 'var(--ink-faint)',
            transform: 'translateX(-50%)',
          }}
        />
      </div>
      <div
        style={{
          fontFamily: 'var(--f-body)',
          fontSize: '14px',
          lineHeight: '22px',
          color: event.is_future ? 'var(--ink-muted)' : 'var(--ink)',
        }}
      >
        <span style={{ fontWeight: 500 }}>{event.action}</span>
        {event.detail && (
          <span style={{ fontWeight: 400, color: 'var(--ink-muted)' }}>
            {' '}
            {event.detail}
          </span>
        )}
      </div>
      <div style={{ textAlign: 'right' }}>
        {event.pill && (
          <span className={`pill pill--${event.pill.variant ?? 'dormant'}`}>{event.pill.label}</span>
        )}
      </div>
    </div>
  );
}
