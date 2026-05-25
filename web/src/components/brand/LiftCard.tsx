/**
 * LiftCard — V1.1 §3.18 "white tile on cream" pattern.
 *
 * Fixup 4 (load-bearing): the `.lift-card` IS the white panel + shadow.
 * NOT a nested chassis-tone1 + panel stack. The optional `surround` prop
 * draws the outer chassis-tone1 backdrop only when the card needs the
 * "tile sitting on a cream tray" framing (e.g. Settings → Connected
 * accounts on a chassis page).
 *
 * Canonical instances (V1.1 §3.18):
 *   1. Person-page hero infobox (Wiki) — avatar, name, signals.
 *   2. OstlerInstaller "Save your emergency key" page (RecoveryKeyView).
 *   3. Wiki "Recommended for you" card on Home.
 *   4. Hub Settings "Connected accounts" card.
 *
 * Discipline: no more than 2 lift-cards visible on the same screen.
 */

import type { ReactNode } from 'react';

interface LiftCardProps {
  children: ReactNode;
  /**
   * When true, wraps the white tile in a `chassis-tone1` surround. Use this
   * on pages where the white tile sits directly on `--chassis` (the page
   * background) — the surround creates the "tray" framing. Default false:
   * just the white tile + shadow, no surround.
   */
  surround?: boolean;
  /** Optional className passed through to the white tile element. */
  className?: string;
}

export default function LiftCard({
  children,
  surround = false,
  className = '',
}: LiftCardProps) {
  const tile = (
    <div className={`lift-card ${className}`.trim()}>
      <div className="inner">{children}</div>
    </div>
  );

  if (!surround) return tile;

  return <div className="lift-card-surround">{tile}</div>;
}
