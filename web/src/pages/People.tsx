/**
 * People — V1.1 §3.3 person-row + §5.1 nav item #4.
 *
 * Renders a list of people from the PWG. Avatar uses the 5-slot colour
 * rotation (V1.1 §3.3); slot is computed deterministically from the
 * person's identifier so the same person always gets the same slot.
 *
 * Wires to the PWG Gateway `/api/people?sort=recency` when available; falls
 * back to a tasteful empty-state otherwise.
 */

import { useEffect, useState } from 'react';
import { Users } from 'lucide-react';
import { apiFetch } from '@/lib/api';

interface Person {
  id: string;
  name: string;
  role?: string;
  channels?: string;   // "iMessage · WhatsApp · Email"
  recency?: string;    // "2H AGO" — Plex Mono uppercase
  status?: { label: string; variant: 'strong' | 'active' | 'dormant' | 'lost' | 'new' };
}

interface PeopleResponse {
  people: Person[];
}

/**
 * CRC32-style hash mapped to 5 slots. We need a deterministic per-person
 * colour slot so the wiki, Hub, and iOS all paint Alison's avatar in the
 * same colour. Full CRC32 is overkill — a small Fowler-Noll-Vo hash is
 * stable enough and avoids pulling in a crypto dependency.
 */
function slotForId(id: string): 0 | 1 | 2 | 3 | 4 {
  let h = 2166136261 >>> 0;
  for (let i = 0; i < id.length; i++) {
    h ^= id.charCodeAt(i);
    h = Math.imul(h, 16777619) >>> 0;
  }
  return (h % 5) as 0 | 1 | 2 | 3 | 4;
}

function initialFor(name: string): string {
  const trimmed = name.trim();
  if (!trimmed) return '?';
  return trimmed.slice(0, 1).toUpperCase();
}

const SLOT_BACKGROUNDS: Record<number, string> = {
  0: 'var(--chassis-deep)',
  1: 'var(--accent-soft)',
  2: 'var(--accent-2-soft)',
  3: 'var(--accent-3-soft)',
  4: 'var(--accent-warm-soft)',
};

export default function People() {
  const [people, setPeople] = useState<Person[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const res = await apiFetch<PeopleResponse>('/api/people?sort=recency');
        if (!cancelled) {
          setPeople(res.people ?? []);
          setError(null);
        }
      } catch (e) {
        if (!cancelled) {
          setPeople([]);
          setError(e instanceof Error ? e.message : 'Unable to load people');
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
          <div className="strap">YOUR NETWORK</div>
          <h1 className="title">People</h1>
        </div>
        <div className="meta">
          {people.length > 0 ? `${people.length} PEOPLE` : ''}
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
          <span style={{ fontFamily: 'var(--f-body)', fontSize: '13px', color: 'var(--ink-muted)' }}>
            Loading
          </span>
        </div>
      )}

      {!loading && people.length === 0 && (
        <div className="empty-state" style={{ padding: 'var(--space-5)' }}>
          <Users
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
            No people yet
          </div>
          <div
            style={{
              fontFamily: 'var(--f-body)',
              fontSize: '13px',
              color: 'var(--ink-subdued)',
            }}
          >
            Contacts will appear here once your channels (iMessage, WhatsApp,
            email) finish syncing.
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

      {!loading && people.length > 0 && (
        <div>
          {people.map((p) => (
            <PersonRow key={p.id} person={p} />
          ))}
        </div>
      )}
    </div>
  );
}

function PersonRow({ person }: { person: Person }) {
  const slot = slotForId(person.id);
  return (
    <div
      style={{
        display: 'grid',
        gridTemplateColumns: '36px 1fr 220px 120px 88px',
        gap: 'var(--space-3)',
        alignItems: 'center',
        padding: 'var(--space-3) 0',
        borderBottom: '1px solid var(--hairline)',
      }}
    >
      <div
        aria-hidden
        style={{
          width: '36px',
          height: '36px',
          borderRadius: '9999px',
          background: SLOT_BACKGROUNDS[slot],
          color: 'var(--ink)',
          fontFamily: 'var(--f-display)',
          fontWeight: 600,
          fontSize: '13px',
          lineHeight: '16px',
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'center',
          flexShrink: 0,
        }}
      >
        {initialFor(person.name)}
      </div>
      <div>
        <div
          style={{
            fontFamily: 'var(--f-body)',
            fontWeight: 600,
            fontSize: '14px',
            lineHeight: '20px',
            color: 'var(--ink)',
          }}
        >
          {person.name}
        </div>
        {person.role && (
          <div
            style={{
              fontFamily: 'var(--f-body)',
              fontSize: '13px',
              lineHeight: '20px',
              color: 'var(--ink-muted)',
            }}
          >
            {person.role}
          </div>
        )}
      </div>
      <div
        style={{
          fontFamily: 'var(--f-mono)',
          fontSize: '10px',
          lineHeight: '14px',
          letterSpacing: '0.08em',
          color: 'var(--ink-muted)',
        }}
      >
        {person.channels}
      </div>
      <div
        style={{
          fontFamily: 'var(--f-mono)',
          fontSize: '10px',
          lineHeight: '14px',
          textTransform: 'uppercase',
          letterSpacing: '0.08em',
          color: 'var(--ink-muted)',
        }}
      >
        {person.recency}
      </div>
      <div>
        {person.status && <span className={`pill pill--${person.status.variant}`}>{person.status.label}</span>}
      </div>
    </div>
  );
}
