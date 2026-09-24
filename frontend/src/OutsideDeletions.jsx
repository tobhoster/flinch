import React from 'react';
import { SectionTitle } from './ui.jsx';

const LIMIT = 10;
const day = (unix) => new Date(unix * 1000).toLocaleDateString([], { month: 'short', day: 'numeric' });

/** Monitored with nothing on disk: the *arr will download it again. */
const returning = (d) => d.monitored === true && !d.on_disk;

/** How the item left, in the words of the app that saw it go. */
function how(d) {
  if (d.reason === 'manual') return d.kind === 'season' ? 'deleted through Sonarr' : 'deleted through Radarr';
  if (d.reason === 'missing_from_disk') return 'vanished from disk';
  return 'removed';
}

/** Whether it comes back: a file on disk, a monitored gap the *arr will fill, or neither. */
function Status({ d }) {
  if (d.on_disk) return <> · back on disk</>;
  if (returning(d)) return <> · <span className="text-state-warn">monitored: will download again</span></>;
  if (d.monitored === false) return <> · unmonitored</>;
  return null;
}

/**
 * Deletions FLINCH did not make, from `status.outside_deletions`: Radarr and
 * Sonarr history of the last 30 days. What will download again comes first,
 * each group newest first as the daemon sent it. An absent or empty list (an
 * older snapshot, or nothing deleted) renders nothing.
 */
export default function OutsideDeletions({ items }) {
  if (!items?.length) return null;
  const coming = items.filter(returning);
  const ordered = [...coming, ...items.filter((d) => !returning(d))];
  return (
    <section className="min-w-0">
      <SectionTitle hint="last 30 days" term="outside_deletions">Deleted outside FLINCH</SectionTitle>
      {coming.length > 0 && (
        <p className="mb-1 text-state-warn">
          <span className="num">{coming.length}</span> {coming.length === 1 ? 'is' : 'are'} still monitored with nothing on disk:
          {' '}Radarr or Sonarr will download {coming.length === 1 ? 'it' : 'them'} again.
        </p>
      )}
      <ul className="space-y-1 text-fg-muted">
        {ordered.slice(0, LIMIT).map((d) => (
          <li key={d.id} className="break-words">
            <span className="text-fg">{d.title || d.id}{d.season_label ? ` ${d.season_label}` : ''}</span>
            {' · '}<span className="num">{day(d.at_unix)}</span>
            {' · '}{how(d)}
            {d.kind === 'season' && <> · <span className="num">{d.files}</span> {d.files === 1 ? 'file' : 'files'}</>}
            <Status d={d} />
          </li>
        ))}
        {items.length > LIMIT && <li className="text-fg-faint">+{items.length - LIMIT} more</li>}
      </ul>
    </section>
  );
}
