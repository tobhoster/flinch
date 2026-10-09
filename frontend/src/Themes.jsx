import React from 'react';
import { GiB, SectionTitle, pct } from './ui.jsx';

const LIMIT = 12;

/**
 * Storage by theme, from `status.themes`: each theme's bytes on disk, titles,
 * share played in the last year and bytes the plan evicts, largest first. A
 * cold theme (seldom played) is where large items get downgrade advice. An
 * absent block (no vectors yet, or an older snapshot) renders nothing.
 */
export default function Themes({ themes }) {
  const list = themes?.themes;
  if (!list?.length) return null;
  return (
    <section className="min-w-0">
      <SectionTitle hint={`${list.length} themes`} term="themes">Storage by theme</SectionTitle>
      <ul className="space-y-1 text-fg-muted">
        {list.slice(0, LIMIT).map((t) => (
          <li key={t.name} className="flex flex-wrap items-baseline gap-x-2 break-words">
            <span className={t.cold ? 'text-state-warn' : 'text-fg'}>{t.name}</span>
            <span><span className="num">{GiB(t.bytes)}</span> GiB</span>
            <span>· <span className="num">{t.titles}</span> titles</span>
            <span>· <span className="num">{pct(t.played_share)}</span> played in a year</span>
            {t.planned_bytes > 0 && <span>· <span className="num">{GiB(t.planned_bytes)}</span> GiB planned</span>}
            {t.cold && <span className="text-state-warn">· seldom played</span>}
          </li>
        ))}
        {list.length > LIMIT && <li className="text-fg-faint">+{list.length - LIMIT} more</li>}
        {themes.unthemed_titles > 0 && (
          <li className="text-fg-faint">
            No theme yet: <span className="num">{themes.unthemed_titles}</span> titles, <span className="num">{GiB(themes.unthemed_bytes)}</span> GiB
          </li>
        )}
      </ul>
    </section>
  );
}
