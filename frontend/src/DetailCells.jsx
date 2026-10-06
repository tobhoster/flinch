import React from 'react';
import { Explain } from './Explain.jsx';

/**
 * Detail-sheet cells for the per-item fields the daemon reports: the governed
 * disk, the Plex ids Maintainerr acts on, reacquisition and the advice. A
 * missing field predates the daemon reporting it and renders a dash; `null`
 * for disk or Plex is a real "not found" and is flagged.
 */

/** The governed disk the item's files live on; `null` means it is never evicted. */
export function Disk({ volume }) {
  if (volume === undefined) return <span className="text-fg-faint">—</span>;
  if (volume === null) {
    return <span className="inline-flex items-center gap-1.5 text-state-warn">not governed <Explain term="not_governed" /></span>;
  }
  return <span className="break-all font-mono text-fg-muted">{volume}</span>;
}

/** The Plex ids; `null` means the catalogue-id join found nothing. */
export function PlexIds({ plex }) {
  if (plex === undefined) return <span className="text-fg-faint">—</span>;
  if (plex === null) {
    return <span className="text-state-warn">not matched <Explain term="unresolved" /></span>;
  }
  return (
    <span className="num break-all text-fg-muted">
      rk {plex.rating_key}{plex.season_rating_key ? ` · season ${plex.season_rating_key}` : ''}
    </span>
  );
}

/** Reacquisition friction as a word; 1.0 is an ordinary re-download. */
export function Reacquisition({ friction }) {
  if (friction == null) return <span className="text-fg-faint">—</span>;
  const level = friction <= 1.5 ? 'Low' : friction <= 3 ? 'Medium' : 'High';
  return <span className={friction > 3 ? 'text-state-warn' : 'text-fg-muted'} title={`friction ${friction.toFixed(1)}`}>{level}</span>;
}

/** Advice action labels, shared by the table column and the detail sheet. */
export const ADVICE = { keep_original: 'Keep', downgrade_quality: 'Downgrade', eligible_for_eviction: 'Evictable' };

/** The quality advice; nothing in Radarr or Sonarr is changed. */
export function Advice({ advice, full = false }) {
  if (!advice?.action) return <span className="text-fg-faint">—</span>;
  const label = ADVICE[advice.action.type] ?? advice.action.type;
  if (!full) return <span className="text-fg-muted" title={advice.explanation}>{label}</span>;
  return (
    <span className="text-fg-muted">
      <span className="text-fg">{label}</span>{advice.explanation ? ` — ${advice.explanation}` : ''}
    </span>
  );
}
