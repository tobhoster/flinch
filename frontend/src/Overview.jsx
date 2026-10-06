import React, { useMemo } from 'react';
import { AlertTriangle } from 'lucide-react';
import RecentRuns from './Progress.jsx';
import Capacity from './Capacity.jsx';
import MaintainerrSync from './Maintainerr.jsx';
import OutsideDeletions from './OutsideDeletions.jsx';
import ModelCard from './ModelCard.jsx';
import { GiB, SectionTitle, ago } from './ui.jsx';
import { Explain, GlossaryCard } from './Explain.jsx';

const CANDIDATE_LIMIT = 25;
const regretText = (r) => (r == null ? '—' : r.toFixed(2));

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

/**
 * Groups for the daemon's kept-item `reason` sentences: `[pattern, label,
 * glossary key]`. A reason no pattern matches is its own group.
 */
const KEPT_GROUPS = [
  [/^Pinned/, 'Pinned', 'pinned'],
  [/grace period/, 'In grace period', 'grace_period'],
  [/^Not needed/, 'Eligible, not needed', 'eligible'],
  [/^Never played/, 'Never played', 'never_played'],
  [/^Not matched in Plex/, 'Not matched in Plex', 'unresolved'],
  [/^On no governed disk/, 'Not governed', 'not_governed'],
  [/must go first/, 'Earlier season first', 'plan'],
];

/** What lifts the never-played hold (`never_played_hold`), by whether the settings ask for it. */
const SHADOW_HINT = {
  leaving_soon_untitled: { held: 'Held until a Leaving Soon collection is named.', off: 'Name a Leaving Soon collection, then enable it in Settings.' },
  incomplete_evidence: { held: 'Held until the watch evidence is complete.', off: 'Enable it in Settings once the watch evidence is complete.' },
};

export default function Overview({ status, items, history, loading }) {
  const s = status || {};

  const totals = useMemo(() => {
    let bytes = 0, onDisk = 0, planned = 0, kept = 0;
    for (const i of items) {
      const size = i.size_bytes || 0;
      bytes += size;
      if (size > 0) onDisk += 1;
      if (i.decision === 'delete') planned += 1;
      else if (size > 0) kept += 1;
    }
    return { bytes, onDisk, planned, kept };
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

  // Why everything else is kept. Without this an all-kept library looks broken.
  const kept = useMemo(() => {
    const groups = new Map();
    for (const item of items) {
      if (item.decision === 'delete') continue;
      const reason = (item.size_bytes || 0) === 0 ? 'Nothing on disk' : item.reason || 'No reason recorded';
      const [, label, term] = KEPT_GROUPS.find(([re]) => re.test(reason)) ?? [null, reason, null];
      const group = groups.get(label) || { label, term, count: 0, bytes: 0 };
      group.count += 1;
      group.bytes += item.size_bytes || 0;
      groups.set(label, group);
    }
    return [...groups.values()].sort((a, b) => b.bytes - a.bytes || b.count - a.count);
  }, [items]);

  const cheapest = useMemo(() => items
    .filter((i) => i.decision !== 'delete' && i.regret != null)
    .sort((a, b) => a.regret - b.regret)
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

  const planned = s.delete_candidates ?? totals.planned;

  return (
    <div className="mt-6 space-y-6 text-[13px]">
      {s.last_error && <RunFailed status={s} snapshot />}

      <dl className="grid grid-cols-2 gap-px overflow-hidden rounded-lg border border-line bg-line md:grid-cols-4">
        <Metric label="On disk" value={`${GiB(totals.bytes)} GiB`} note={`${totals.onDisk} of ${items.length} items`} />
        <Metric label="Planned" value={planned} note={`${GiB(s.reclaimed_bytes)} GiB`} warn={planned > 0} />
        <Metric label="Eligible" value={`${GiB(s.eligible_bytes)} GiB`} />
        <Metric label="Kept" value={totals.kept} />
      </dl>

      {s.capacity && <Capacity c={s.capacity} />}

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
              Never-played reclaim is off: {s.evidence_problems.join(', ')}. <Explain term="evidence_complete" />
            </span>
          </p>
        )}
      </section>

      <ModelCard fit={s.fit} />

      <div className="grid grid-cols-1 gap-6 lg:grid-cols-[minmax(0,3fr)_minmax(0,2fr)]">
        {candidates.length > 0
          ? <CandidateList items={candidates} />
          : <NothingPlanned kept={kept} cheapest={cheapest} status={s} />}
        <div className="min-w-0 space-y-6">
          <RecentRuns history={history} target={s.capacity?.target_utilization} />
          <MaintainerrSync sync={s.sync} dryRun={s.dry_run} />
          <OutsideDeletions items={s.outside_deletions} />
          {s.quality && (
            <p className="text-fg-muted">
              Quality advice: <span className="num text-fg">{s.quality.keep ?? 0}</span> keep
              {' · '}<span className="num text-fg">{s.quality.downgrade ?? 0}</span> downgrade
              {' · '}<span className="num text-fg">{s.quality.evict ?? 0}</span> evictable <Explain term="advice" />
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
      <SectionTitle hint="largest first" term="plan">Planned</SectionTitle>
      <div className={`hidden border-b border-line pb-1.5 text-xs text-fg-muted ${ROW}`}>
        <span>Title</span>
        <span className="text-right">Size</span>
        <span className="inline-flex items-center justify-end gap-1">Regret <Explain term="regret" /></span>
        <span>Reason</span>
      </div>
      {shown.map((i) => (
        <div key={i.id} className={`border-b border-line-soft py-1.5 last:border-0 ${ROW}`}>
          <div className="flex min-w-0 items-baseline gap-3 md:contents">
            <span className="min-w-0 flex-1 truncate" title={i.title}>{i.title}</span>
            <span className="num shrink-0 text-right">{GiB(i.size_bytes)} GiB</span>
            <span className="num w-10 shrink-0 text-right text-fg-muted md:w-auto">{regretText(i.regret)}</span>
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

function NothingPlanned({ kept, cheapest, status }) {
  const why = status.capacity?.healthy ? 'Storage is healthy.' : 'Nothing eligible.';
  const shadowHint = SHADOW_HINT[status.never_played_hold]?.[status.never_played_requested ? 'held' : 'off'] || 'Enable it in Settings.';
  return (
    <section className="min-w-0 space-y-5">
      <div>
        <SectionTitle>Nothing planned</SectionTitle>
        <p className="text-fg-muted">{why} Kept items by reason:</p>
        <table className="mt-2 w-full">
          <thead>
            <tr className="border-b border-line text-left text-xs text-fg-muted">
              <th className="py-1.5 pr-3 font-normal">Reason</th>
              <th className="py-1.5 pr-3 text-right font-normal">Items</th>
              <th className="py-1.5 text-right font-normal">Size</th>
            </tr>
          </thead>
          <tbody>
            {kept.map((row) => (
              <tr key={row.label} className="border-b border-line-soft last:border-0">
                <td className="py-1.5 pr-3">
                  <span className="inline-flex items-center gap-1.5">{row.label} {row.term && <Explain term={row.term} />}</span>
                </td>
                <td className="num py-1.5 pr-3 text-right">{row.count}</td>
                <td className="num whitespace-nowrap py-1.5 text-right">{GiB(row.bytes)} GiB</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {cheapest.length > 0 && (
        <div>
          <h3 className="mb-1.5 flex items-center gap-1.5 text-xs text-fg-muted">Lowest regret <Explain term="regret" /></h3>
          {cheapest.map((i) => (
            <div key={i.id} className="flex items-baseline gap-3 border-b border-line-soft py-1.5 last:border-0">
              <span className="num w-10 shrink-0 text-right">{regretText(i.regret)}</span>
              <span className="min-w-0 flex-1 truncate" title={i.title}>{i.title}</span>
              <span className="num shrink-0 text-fg-muted">{GiB(i.size_bytes)} GiB</span>
            </div>
          ))}
        </div>
      )}

      {status.shadow_items > 0 && (
        <p className="text-fg-muted">
          Never-played reclaim would add <span className="num text-fg">{status.shadow_items}</span> items
          (<span className="num text-fg">{(status.shadow_gib || 0).toFixed(1)} GiB</span>). {shadowHint}
        </p>
      )}
    </section>
  );
}
