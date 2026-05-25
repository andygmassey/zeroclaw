/**
 * CitationStrip — V1.1 §3.9.
 *
 * Sentence case prefix in Plex Sans (no longer lowercase mono — V1.1
 * retired the "single lowercase mono exception" rule) + UPPERCASE Plex
 * Mono hyperlinked sources with secondary-link treatment (dotted underline,
 * muted oxblood; on hover, dotted → solid, muted → full oxblood).
 *
 * Each source links to the underlying record. The Hub routes those links
 * to the per-record viewer (calendar entry, iMessage thread, WhatsApp
 * conversation, etc.); the consumer of this component is responsible for
 * computing the `href` for each source.
 */

interface CitationSource {
  /** UPPERCASE label shown in Plex Mono (e.g. "CALENDAR 12 FEB"). */
  label: string;
  /**
   * Where this source points. Hub-internal routes look like
   * `/wiki/conversations/<id>` or `/timeline?from=<iso>`. External hrefs
   * (mailto:, etc.) are also fine.
   */
  href: string;
}

interface CitationStripProps {
  sources: CitationSource[];
  /** Optional prefix override. Defaults to "Cited from:". */
  prefix?: string;
}

export default function CitationStrip({
  sources,
  prefix = 'Cited from:',
}: CitationStripProps) {
  if (sources.length === 0) return null;

  return (
    <div className="citation-strip">
      <span className="label">{prefix} </span>
      <span className="sources">
        {sources.map((source, idx) => (
          <span key={`${source.label}-${idx}`}>
            {idx > 0 && <span className="sep">·</span>}
            <a className="source" href={source.href}>
              {source.label.toUpperCase()}
            </a>
          </span>
        ))}
      </span>
    </div>
  );
}
