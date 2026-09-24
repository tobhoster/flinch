import React, { useMemo } from 'react';
import { AlertTriangle } from 'lucide-react';
import RecentRuns from './Progress.jsx';
import Capacity from './Capacity.jsx';
import MaintainerrSync from './Maintainerr.jsx';
import ModelCard from './ModelCard.jsx';
import { GiB, SectionTitle, ago } from './ui.jsx';
import { Explain, GlossaryCard } from './Explain.jsx';
import { GLOSSARY } from './glossary.js';

const pct = (p) => (p == null ? '—' : `${Math.round(p * 100)}%`);
const CANDIDATE_LIMIT = 25;

// Watch sources grouped the way the Library labels them. `tautulli_no_stream`
// is Tautulli's absence record, so it counts under Tautulli.
const SOURCES = [
  ['Plex', ['plex']],
  ['Show-level', ['plex_show']],
  ['Tautulli', ['tautulli', 'tautulli_no_stream']],
  ['History', ['plex_history']],
  ['Export', ['export']],
];
const KNOWN_SOURCES = new Set(SOURCES.flatMap(([, keys]) => keys));

/** Held-reason buckets, as glossary keys: the label and note live there. */
const HELD = {
  reserve: 'held_reserve',
  notGoverned: 'not_governed',
  unresolved: 'unresolved',
  noEvidence: 'held_no_evidence',
  evidenceHeld: 'held_evidence',
  newest: 'held_newest',
  yours: 'held_yours',
  noDate: 'held_no_date',
  floor: 'held_floor',
  empty: 'held_empty',
  excluded: 'held_excluded',
};

/**
 * The daemon's reason strings for items it is not evicting. "Eligible —" is the
 * reserve (waiting for a disk to need space); the ungoverned and unmatched forms
 * can never be evicted; the evidence forms wait on watch evidence. Anything
 * else falls through to the policy and floor buckets.
 */
const NOT_GOVERNED_REASON = 'Eligible, but no governed disk';
const UNRESOLVED_REASON = 'Eligible, but not matched in Plex';
const NO_EVIDENCE_REASON = 'No watch evidence';
const EVIDENCE_HELD_REASON = 'until the watch evidence is complete';
const RESERVE_REASON = /^Eligible\s+—/;

/** Hard guards the operator set: the keep tag, a Plex label or collection, their own exclusion. */
const OPERATOR_GUARDS = new Set(['favorite', 'keep-collection']);

export default function Overview({ status, items, history, loading }) {
  const s = status || {};

  const totals = useMemo(() => {
    let bytes = 0, onDisk = 0, candidates = 0, held = 0;
    for (const i of items) {
      const size = i.size_bytes || 0;
      bytes += size;
      if (size > 0) onDisk += 1;
      if (i.decision === 'delete') candidates += 1;
      else if (size > 0) held += 1;
    }
    return { bytes, onDisk, candidates, held };
  }, [items]);

  const evidence = useMemo(() => {
    const counts = SOURCES.map(([label, keys]) => ({ label, n: items.filter((i) => keys.includes(i.watch_source)).length }));
    const other = items.filter((i) => i.watch_source && !KNOWN_SOURCES.has(i.watch_source)).length;
    if (other > 0) counts.push({ label: 'Other', n: other });
    counts.push({ label: 'No data', n: items.filter((i) => !i.watch_source).length });
    return counts;
  }, [items]);

  const candidates = useMemo(() => items
    .filter((i) => i.decision === 'delete')
    .sort((a, b) => (b.size_bytes || 0) - (a.size_bytes || 0)), [items]);

  // Why everything else is held. Without this an all-held library looks broken.
  const held = useMemo(() => {
    const buckets = new Map();
    const add = (key, item) => {
      const bucket = buckets.get(key) || { key, label: GLOSSARY[key].term, count: 0, bytes: 0 };
      bucket.count += 1;
      bucket.bytes += item.size_bytes || 0;
      buckets.set(key, bucket);
    };
    for (const item of items) {
      if ((item.size_bytes || 0) === 0) { add(HELD.empty, item); continue; }
      if (item.decision === 'delete') continue;
      const reason = item.reason || '';
      const watchedNoDate = reason.includes('no date')
        || ((item.watched_fraction ?? 0) >= 1 && item.last_watched_days == null);
      if (item.hard_guard === 'newest-season') add(HELD.newest, item);
      else if (OPERATOR_GUARDS.has(item.hard_guard)) add(HELD.yours, item);
      else if (reason.startsWith(NOT_GOVERNED_REASON)) add(HELD.notGoverned, item);
      else if (reason.startsWith(UNRESOLVED_REASON)) add(HELD.unresolved, item);
      else if (reason.startsWith(NO_EVIDENCE_REASON)) add(HELD.noEvidence, item);
      else if (reason.includes(EVIDENCE_HELD_REASON)) add(HELD.evidenceHeld, item);
      else if (RESERVE_REASON.test(reason)) add(HELD.reserve, item);
      else if (watchedNoDate) add(HELD.noDate, item);
      else if (item.protected) add(HELD.excluded, item);
      else add(HELD.floor, item);
    }
    return [...buckets.values()].sort((a, b) => b.bytes - a.bytes || b.count - a.count);
  }, [items]);

  const closest = useMemo(() => items
    .filter((i) => (i.size_bytes || 0) > 0 && i.decision !== 'delete')
    .sort((a, b) => (b.p_safe ?? -1) - (a.p_safe ?? -1))
    .slice(0, 3), [items]);

  if (loading) return <p className="mt-6 text-[13px] text-fg-muted">Loading…</p>;

  const hasData = s.scanned !== undefined && s.scanned !== null;
  if (!hasData && items.length === 0) {
    return (
      <div className="mt-6 space-y-6 text-[13px]">
        {s.last_error && <RunFailed status={s} snapshot={false} />}
        <p className="text-fg-muted">No snapshot yet. Trigger a run, or wait for the next scheduled one.</p>
      </div>
    );
  }

  const candidateCount = s.delete_candidates ?? totals.candidates;

  return (
    <div className="mt-6 space-y-6 text-[13px]">
      {s.last_error && <RunFailed status={s} snapshot />}

      <dl className="grid grid-cols-2 gap-px overflow-hidden rounded-lg border border-line bg-line md:grid-cols-4">
        <Metric label="On disk" value={`${GiB(totals.bytes)} GiB`} note={`${totals.onDisk} of ${items.length} items`} />
        <Metric label="Reclaimable" value={`${GiB(s.reclaimed_bytes)} GiB`} />
        <Metric label="Candidates" value={candidateCount} warn={candidateCount > 0} />
        <Metric label="Held" value={totals.held} />
      </dl>

      {s.capacity && <Capacity c={s.capacity} eligible={s.eligible_bytes} />}

      <section>
        <SectionTitle hint={`${items.length} items`} term="evidence">Watch evidence</SectionTitle>
        <div className="flex flex-wrap gap-x-5 gap-y-1">
          {evidence.map((e) => (
            <span key={e.label} className="whitespace-nowrap">
              <span className="text-fg-muted">{e.label}</span> <span className="num">{e.n}</span>
            </span>
          ))}
        </div>
        {(s.evidence_problems || []).length > 0 && (
          <p className="mt-2 flex items-start gap-2 text-state-warn">
            <AlertTriangle size={14} aria-hidden className="mt-0.5 shrink-0" />
            <span className="min-w-0 break-words">
              Never-played reclaim is held off: {s.evidence_problems.join(', ')}. <Explain term="evidence_complete" />
            </span>
          </p>
        )}
      </section>

      <ModelCard fit={s.fit} benchmark={s.benchmark} />

      <div className="grid grid-cols-1 gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        {candidates.length > 0
          ? <CandidateList items={candidates} />
          : <NothingToReclaim held={held} closest={closest} status={s} />}
        <div className="min-w-0 space-y-6">
          <RecentRuns history={history} />
          <MaintainerrSync sync={s.sync} />
          {(s.inflow?.compact > 0 || s.inflow?.premium > 0) && (
            <p className="text-fg-muted">
              Quality tiers (advice): <span className="num text-fg">{s.inflow.compact}</span> compact
              {' · '}<span className="num text-fg">{s.inflow.premium}</span> premium <Explain term="quality_tier" />
            </p>
          )}
        </div>
      </div>

      <GlossaryCard />
    </div>
  );
}

/** The newest cycle failed; what is on screen is the last good snapshot. */
function RunFailed({ status, snapshot }) {
  const secs = status.last_error_at ? Math.max(0, Math.floor(Date.now() / 1000) - status.last_error_at) : null;
  return (
    <div role="alert" className="flex gap-2.5 rounded-lg border border-state-bad/40 bg-state-bad/10 px-4 py-3 text-state-bad">
      <AlertTriangle size={15} aria-hidden className="mt-0.5 shrink-0" />
      <p className="min-w-0 break-words">
        <span className="font-medium">Last run failed{secs === null ? '' : ` ${ago(secs)}`}:</span>
        {' '}<span className="text-fg">{status.last_error}</span>
        {snapshot && <span className="text-fg-muted"> Showing the last good snapshot.</span>}
      </p>
    </div>
  );
}

function Metric({ label, value, note, warn }) {
  return (
    <div className="min-w-0 bg-ink-900 px-4 py-3">
      <dt className="text-xs text-fg-muted">{label}</dt>
      <dd className={`num mt-0.5 text-lg ${warn ? 'text-state-warn' : 'text-fg'}`}>{value}</dd>
      {note && <dd className="text-xs text-fg-faint">{note}</dd>}
    </div>
  );
}

const ROW = 'md:grid md:grid-cols-[minmax(0,2fr)_80px_56px_minmax(0,3fr)] md:gap-3';

function CandidateList({ items }) {
  const shown = items.slice(0, CANDIDATE_LIMIT);
  return (
    <section className="min-w-0">
      <SectionTitle hint="largest first">Candidates</SectionTitle>
      <div className={`hidden border-b border-line pb-1.5 text-xs text-fg-muted ${ROW}`}>
        <span>Title</span>
        <span className="text-right">Size</span>
        <span className="inline-flex items-center justify-end gap-1">P(safe) <Explain term="p_safe" /></span>
        <span>Reason</span>
      </div>
      {shown.map((i) => (
        <div key={i.id} className={`border-b border-line-soft py-1.5 last:border-0 ${ROW}`}>
          <div className="flex min-w-0 items-baseline gap-3 md:contents">
            <span className="min-w-0 flex-1 truncate" title={i.title}>{i.title}</span>
            <span className="num shrink-0 text-right">{GiB(i.size_bytes)} GiB</span>
            <span className="num w-10 shrink-0 text-right text-fg-muted md:w-auto">{pct(i.p_safe)}</span>
          </div>
          <span className="block truncate text-xs text-fg-muted md:text-[13px]" title={i.reason}>{i.reason}</span>
        </div>
      ))}
      {items.length > shown.length && (
        <p className="pt-2 text-xs text-fg-muted">{items.length - shown.length} more in Series and Movies.</p>
      )}
    </section>
  );
}

function NothingToReclaim({ held, closest, status }) {
  const eligibleHeld = held.some((row) => [HELD.reserve, HELD.notGoverned, HELD.unresolved].includes(row.key));
  const why = status.capacity && !status.capacity.latched
    ? 'Storage is under the ceiling, so nothing is scheduled.'
    : eligibleHeld ? 'Nothing is scheduled this run.' : 'Nothing passes the policy and the P(safe) floor.';
  return (
    <section className="min-w-0 space-y-5">
      <div>
        <SectionTitle>No candidates</SectionTitle>
        <p className="text-fg-muted">{why} Held items by reason:</p>
        <table className="mt-2 w-full">
          <thead>
            <tr className="border-b border-line text-left text-xs text-fg-muted">
              <th className="py-1.5 pr-3 font-normal">Reason</th>
              <th className="py-1.5 pr-3 text-right font-normal">Items</th>
              <th className="py-1.5 text-right font-normal">Size</th>
            </tr>
          </thead>
          <tbody>
            {held.map((row) => (
              <tr key={row.key} className="border-b border-line-soft last:border-0">
                <td className="py-1.5 pr-3">
                  <span className="inline-flex items-center gap-1.5">{row.label} <Explain term={row.key} /></span>
                </td>
                <td className="num py-1.5 pr-3 text-right">{row.count}</td>
                <td className="num whitespace-nowrap py-1.5 text-right">{GiB(row.bytes)} GiB</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {closest.length > 0 && (
        <div>
          <h3 className="mb-1.5 flex items-center gap-1.5 text-xs text-fg-muted">Closest by P(safe) <Explain term="p_safe" /></h3>
          {closest.map((i) => (
            <div key={i.id} className="border-b border-line-soft py-1.5 last:border-0">
              <div className="flex items-baseline gap-3">
                <span className="num w-10 shrink-0 text-right">{pct(i.p_safe)}</span>
                <span className="min-w-0 flex-1 truncate" title={i.title}>{i.title}</span>
                <span className="num shrink-0 text-fg-muted">{GiB(i.size_bytes)} GiB</span>
              </div>
              <div className="truncate pl-[3.25rem] text-xs text-fg-muted" title={i.reason}>
                {i.hard_guard ? `Guard: ${i.hard_guard}. ` : ''}{i.reason}
              </div>
            </div>
          ))}
        </div>
      )}

      {status.shadow_items > 0 && (
        <p className="text-fg-muted">
          Never-played reclaim would add <span className="num text-fg">{status.shadow_items}</span> items
          (<span className="num text-fg">{(status.shadow_gib || 0).toFixed(1)} GiB</span>). Enable it in Settings.
        </p>
      )}
    </section>
  );
}
