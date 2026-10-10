import React from 'react';
import { GiB, SectionTitle, day } from './ui.jsx';

const LIMIT = 8;

const HOLD = {
  profile_unknown: 'profile unknown',
  no_compact_profile: 'no compact profile',
  already_compact: 'already compact',
  no_smaller_release: 'no release 30% smaller',
  other_season_keeps: 'another season keeps its original',
  daily_cap: 'daily cap',
};

const OUTCOME = {
  pending: 'waiting for a smaller file',
  landed: 'smaller file landed',
  nothing_smaller: 'nothing smaller in 14 days',
  gone: 'left the library',
  failed: 'refused',
};

const GUARD = { flag: 'flagged', unmonitor: 'unmonitored', upgrades_off: 'upgrades off on its profile' };

const SEARCH_HOLD = {
  protected: 'pinned or partway',
  hard_to_replace: 'hard to get back',
  in_plan: 'in the plan',
  churning: 'grabbed again and again',
  size_unknown: 'no release size yet',
  no_headroom: 'disk lacks headroom',
  daily_cap: 'daily cap',
};

const SEARCH_OUTCOME = {
  pending: 'searching',
  upgraded: 'reached its cutoff',
  nothing_found: 'nothing better in 14 days',
  gone: 'left the library',
  failed: 'refused',
};

/** How many of each hold reason, most common first. */
function holdCounts(held) {
  const counts = {};
  for (const h of held) counts[h.hold] = (counts[h.hold] ?? 0) + 1;
  return Object.entries(counts).sort((a, b) => b[1] - a[1]);
}

/** Upgrade searches for cutoff-unmet items, likeliest watched first (`status.upgrade_search`). */
function UpgradeSearches({ search }) {
  const picks = search.picks ?? [];
  const recent = search.recent ?? [];
  return (
    <div>
      <SectionTitle hint={search.dry_run ? 'dry run: printed, not sent' : undefined} term="upgrade_search">Upgrade searches</SectionTitle>
      <p className="text-fg-muted">
        <span className="num text-fg">{search.searched_today ?? 0}</span> of <span className="num">{search.max_per_day}</span> today
        {' · '}<span className="num text-fg">{search.unmet ?? 0}</span> below cutoff
        {' · '}<span className="num text-fg">{search.upgraded ?? 0}</span> upgraded
      </p>
      {picks.length > 0 && (
        <ul className="mt-1 space-y-1 text-fg-muted">
          {picks.map((p) => (
            <li key={p.card_id} className="break-words">
              <span className="text-fg">{p.title}</span>
              {' · '}P(watch) <span className="num">{Math.round(p.p_watch * 100)}%</span>
              {' · '}<span className="num">{GiB(p.bytes)}</span> → up to <span className="num">{GiB(p.expected_bytes)}</span> GiB
              {search.dry_run ? ' · would search' : ' · searched'}
            </li>
          ))}
        </ul>
      )}
      {search.held?.length > 0 && (
        <p className="mt-1 text-fg-faint">
          Waiting: {holdCounts(search.held).map(([hold, n]) => `${n} ${SEARCH_HOLD[hold] ?? hold}`).join(' · ')}
        </p>
      )}
      {recent.length > 0 && (
        <ul className="mt-2 space-y-1 text-fg-muted">
          {recent.slice(0, LIMIT).map((s) => (
            <li key={`${s.card_id}-${s.at}`} className="break-words">
              <span className="text-fg">{s.title}</span> · {day(s.at)} · {SEARCH_OUTCOME[s.outcome?.state] ?? s.outcome?.state}
              {s.outcome?.state === 'failed' && <span className="text-fg-faint"> ({s.outcome.reason})</span>}
            </li>
          ))}
        </ul>
      )}
      {search.problems?.map((p) => <p key={p} className="mt-1 text-state-bad">{p}</p>)}
    </div>
  );
}

/**
 * Moves to the compact profile (`status.quality_actions`), upgrade searches
 * (`status.upgrade_search`) and items grabbed again and again
 * (`status.upgrade_churn`). Absent blocks (off, or an older snapshot) render
 * nothing.
 */
export default function QualityActions({ actions, churn, search }) {
  const moves = actions?.moves ?? [];
  const recent = actions?.recent ?? [];
  const flagged = churn?.items ?? [];
  const showActions = actions && (actions.enabled || recent.length > 0);
  const showSearch = search && (search.enabled || search.recent?.length > 0);
  if (!showActions && !showSearch && !flagged.length) return null;
  return (
    <section className="min-w-0 space-y-4">
      {showActions && (
        <div>
          <SectionTitle hint={actions.dry_run ? 'dry run: printed, not sent' : undefined} term="quality_actions">Downgrades</SectionTitle>
          <p className="text-fg-muted">
            <span className="num text-fg">{actions.acted_today ?? 0}</span> of <span className="num">{actions.max_per_day}</span> today
            {' · '}<span className="num text-fg">{GiB(actions.reclaimed_bytes)}</span> GiB given back
          </p>
          {moves.length > 0 && (
            <ul className="mt-1 space-y-1 text-fg-muted">
              {moves.flatMap((m) => m.cards).map((c) => (
                <li key={c.card_id} className="break-words">
                  <span className="text-fg">{c.title}</span>
                  {' · '}<span className="num">{GiB(c.bytes)}</span> GiB
                  {c.release_bytes != null && <> → a <span className="num">{GiB(c.release_bytes)}</span> GiB release</>}
                  {actions.dry_run ? ' · would move' : ' · moved'}
                </li>
              ))}
            </ul>
          )}
          {actions.held?.length > 0 && (
            <p className="mt-1 text-fg-faint">
              Waiting: {holdCounts(actions.held).map(([hold, n]) => `${n} ${HOLD[hold] ?? hold}`).join(' · ')}
            </p>
          )}
          {recent.length > 0 && (
            <ul className="mt-2 space-y-1 text-fg-muted">
              {recent.slice(0, LIMIT).map((a) => (
                <li key={`${a.card_id}-${a.at}`} className="break-words">
                  <span className="text-fg">{a.title}</span> · {day(a.at)} · {OUTCOME[a.outcome?.state] ?? a.outcome?.state}
                  {a.outcome?.state === 'landed' && <> (<span className="num">{GiB(a.from_bytes)}</span> → <span className="num">{GiB(a.outcome.bytes)}</span> GiB)</>}
                  {a.outcome?.state === 'failed' && <span className="text-fg-faint"> ({a.outcome.reason})</span>}
                </li>
              ))}
            </ul>
          )}
          {actions.problems?.map((p) => <p key={p} className="mt-1 text-state-bad">{p}</p>)}
        </div>
      )}
      {showSearch && <UpgradeSearches search={search} />}
      {churn && (flagged.length > 0 || churn.problems?.length > 0) && (
        <div>
          <SectionTitle hint={`over ${churn.limit} grabs in 30 days`} term="upgrade_churn">Grabbed again and again</SectionTitle>
          <ul className="space-y-1 text-fg-muted">
            {flagged.slice(0, LIMIT).map((c) => (
              <li key={c.card_id} className="break-words">
                <span className="text-fg">{c.title}</span>
                {' · '}<span className="num">{c.grabs}</span> grabs, <span className="num">{c.imports}</span> imports
                {' · '}{c.applied ? GUARD[c.applied.action] ?? c.applied.action : 'flagged'}
                {c.note && <span className="text-fg-faint"> ({c.note})</span>}
              </li>
            ))}
            {flagged.length > LIMIT && <li className="text-fg-faint">+{flagged.length - LIMIT} more</li>}
          </ul>
          {churn.problems?.map((p) => <p key={p} className="mt-1 text-state-bad">{p}</p>)}
        </div>
      )}
    </section>
  );
}
