import React from 'react';
import { JustWatch, SectionTitle } from './ui.jsx';

const LIMIT = 10;

const RULE = {
  cold_unstarted: 'never started',
  abandoned: 'abandoned',
  cold_request: 'cold request',
  streams: 'streams already',
};

/**
 * Incoming storage nobody is likely to watch, from `status.inflow`: advice
 * for the operator, acted on only for shows approved under Settings → Rules
 * (inflow actions). An absent or empty list (an older snapshot, or nothing
 * flagged) renders nothing.
 */
export default function Inflow({ items }) {
  if (!items?.length) return null;
  return (
    <section className="min-w-0">
      <SectionTitle hint="advice; approve in Settings" term="inflow">Coming in, likely unwatched</SectionTitle>
      <ul className="space-y-1 text-fg-muted">
        {items.slice(0, LIMIT).map((s) => (
          <li key={`${s.subject}-${s.rule}-${s.requester ?? ''}`} className="break-words">
            <span className="text-fg">{s.title || s.subject}</span>
            {' · '}{RULE[s.rule] ?? s.rule}
            {s.gib_per_season != null && <> · <span className="num">{s.gib_per_season.toFixed(1)}</span> GiB/season</>}
            <div className="text-fg-faint">{s.why}. Suggested: {s.action}.</div>
          </li>
        ))}
        {items.length > LIMIT && <li className="text-fg-faint">+{items.length - LIMIT} more</li>}
      </ul>
      {items.some((s) => s.rule === 'streams') && <p className="mt-1 text-xs"><JustWatch /></p>}
    </section>
  );
}
