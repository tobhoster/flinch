import React from 'react';
import { Explain } from './Explain.jsx';

/**
 * Detail-sheet cells for the per-item fields the daemon reports: the governed
 * disk, the Plex ids Maintainerr acts on, and the quality-tier advice. A
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
    return <span className="text-state-warn">not matched by id (never protected or scheduled) <Explain term="unresolved" /></span>;
  }
  return (
    <span className="num break-all text-fg-muted">
      rk {plex.rating_key}{plex.season_rating_key ? ` · season ${plex.season_rating_key}` : ''}
    </span>
  );
}

const TIERS = { premium: 'Premium', compact: 'Compact' };

/** Recyclarr profile advice; nothing in Radarr or Sonarr is changed. */
export function QualityTier({ inflow }) {
  if (!inflow) return <span className="text-fg-faint">—</span>;
  return (
    <span className="text-fg-muted">
      <span className="text-fg">{TIERS[inflow.tier] ?? inflow.tier}</span>{inflow.reason ? ` — ${inflow.reason}` : ''}
    </span>
  );
}
