import React from 'react';
import { AlertTriangle } from 'lucide-react';
import { GiB, SectionTitle, day, pct } from './ui.jsx';
import { Explain } from './Explain.jsx';

const HELD_LIMIT = 5;
const METHOD = { milp: 'MILP', emergency: 'emergency greedy', solver_fallback: 'greedy fallback' };

/** Share of the bar, 0..100, for a fraction that may sit outside 0..1. */
const barPct = (f) => Math.max(0, Math.min(100, (f || 0) * 100));

/** Tone of a disk (or the aggregate): emergency, short of its target, needing space, or fine. */
const toneOf = (v) => (v.emergency || v.covered === false ? 'bad' : v.target_reclaim_bytes > 0 ? 'warn' : 'ok');

/**
 * Disk use against the projection. Each disk projects its use `window_days`
 * ahead; past the target the plan must free `target_reclaim_bytes`. The
 * daemon only publishes `capacity` when it measured the *arr disks.
 */
export default function Capacity({ c }) {
  const tone = toneOf(c);
  const volumes = c.volumes || [];
  const unmatched = c.unmatched_roots || [];
  const held = volumes.flatMap((v) => v.held || []).sort((a, b) => b.bytes - a.bytes);
  const heldUntil = held.reduce((latest, h) => Math.max(latest, h.until || 0), 0);
  const marks = { target: c.target_utilization, emergency: c.emergency_utilization };
  return (
    <section className="rounded-lg border border-line bg-ink-900 px-4 py-3">
      <SectionTitle hint={c.emergency ? 'emergency' : c.healthy ? 'healthy' : 'freeing space'} term="projection">Storage</SectionTitle>
      <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1">
        <span>
          <span className={`num text-lg ${TEXT_TONES[tone]}`}>{((c.utilization || 0) * 100).toFixed(1)}%</span>
          <span className="text-fg-muted"> used</span>
        </span>
        <span className="num text-fg-muted">{GiB(c.used_bytes)} / {GiB(c.total_bytes)} GiB</span>
      </div>
      <div className="mt-1 flex flex-wrap justify-end gap-x-4 text-xs text-fg-faint">
        <span className="inline-flex items-center gap-1.5"><span className={`h-3 ${TARGET_MARK}`} />target <span className="num">{pct(c.target_utilization)}</span></span>
        <span className="inline-flex items-center gap-1.5"><span className={`h-3 ${EMERGENCY_MARK}`} />emergency <span className="num">{pct(c.emergency_utilization)}</span></span>
        <span className="inline-flex items-center gap-1.5"><span className={`h-2 w-3 ${PROJECTED_BAR}`} />in <span className="num">{c.window_days}</span> d</span>
      </div>
      <ul className="mt-1 divide-y divide-line-soft">
        {volumes.map((v) => <VolumeRow key={v.path} v={v} marks={marks} window={c.window_days} />)}
      </ul>
      <p className="mt-2 text-fg-muted">
        {c.healthy ? <>
          Healthy: every disk stays under its target. Nothing is planned.
          {' '}<span className="num text-fg">{GiB(c.eligible_bytes)} GiB</span> eligible. <Explain term="eligible" />
        </> : <>
          Need <span className="num text-state-warn">{GiB(c.target_reclaim_bytes)} GiB</span>,
          {' '}plan takes <span className="num text-fg">{GiB(c.planned_bytes)} GiB</span>
          {c.method && <> ({METHOD[c.method] ?? c.method})</>}, total regret <span className="num text-fg">{(c.total_regret || 0).toFixed(2)}</span>. <Explain term="plan" />
          {c.covered === false && <span className="text-state-bad"> Only <span className="num">{GiB(c.eligible_bytes)} GiB</span> is eligible.</span>}
          {c.goal_met && ' All of it is handed to Maintainerr.'}
          {c.goal_met === false && (c.handed_bytes || 0) > 0 && <> <span className="num text-fg">{GiB(c.handed_bytes)} GiB</span> handed so far.</>}
        </>}
      </p>
      {c.method === 'solver_fallback' && (
        <Warning>Solver failed{c.solver_error ? <>: <span className="text-fg">{c.solver_error}</span></> : ''}. Used the greedy fallback.</Warning>
      )}
      {(c.untracked_bytes || 0) > 0 && (
        <p className="mt-1 text-fg-muted">
          <span className="num text-fg">{GiB(c.untracked_bytes)} GiB</span> is not library media. <Explain term="untracked" />
        </p>
      )}
      {(c.pending_bytes || 0) > 0 && (
        <p className="mt-1 text-fg-muted">
          <span className="num text-fg">{GiB(c.pending_bytes)} GiB</span> is in recycle bins. <Explain term="pending" />
        </p>
      )}
      {unmatched.length > 0 && (
        <Warning>
          Not governed: <span className="font-mono text-fg">{unmatched.join(', ')}</span>. Nothing there is evicted. <Explain term="not_governed" />
        </Warning>
      )}
      {(c.held_bytes || 0) > 0 && (
        <Warning>
          <span className="num">{GiB(c.held_bytes)} GiB</span> handed over was never freed
          {heldUntil > 0 && <>; counted as freed until <span className="num text-fg">{day(heldUntil)}</span></>}. <Explain term="held" />
          <ul className="mt-1 space-y-0.5 text-xs text-fg-muted">
            {held.slice(0, HELD_LIMIT).map((h) => (
              <li key={h.id} className="truncate">
                <span className="text-fg">{h.title || h.id}</span> · <span className="num">{GiB(h.bytes)} GiB</span>
                {h.held_since > 0 && <> · since {day(h.held_since)}</>}
              </li>
            ))}
            {held.length > HELD_LIMIT && <li className="text-fg-faint">+{held.length - HELD_LIMIT} more</li>}
          </ul>
        </Warning>
      )}
    </section>
  );
}

function Warning({ children }) {
  return (
    <div className="mt-3 flex gap-2 rounded-md bg-state-warn/10 px-3 py-2 text-state-warn">
      <AlertTriangle size={14} aria-hidden className="mt-0.5 shrink-0" />
      <div className="min-w-0 break-words">{children}</div>
    </div>
  );
}

/** One disk: gauge (used, projected, marks), then ingest, queue and what it needs. */
function VolumeRow({ v, marks, window }) {
  const tone = toneOf(v);
  const cap = v.capacity_bytes || v.total_bytes;
  const projected = cap ? v.projected_used_bytes / cap : 0;
  const need = v.target_reclaim_bytes > 0;
  return (
    <li className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1 py-2 sm:grid-cols-[minmax(0,9rem)_minmax(0,1fr)_7rem]">
      <span className="truncate font-mono text-fg" title={v.path}>{v.path}</span>
      <span className={`num text-right sm:order-3 ${TEXT_TONES[tone]}`}>{pct(v.utilization)} → {pct(projected)}</span>
      <Gauge className="col-span-2 h-1.5 sm:order-2 sm:col-span-1" used={v.utilization} projected={projected} marks={marks} tone={tone} label={`${v.path} used`} />
      <span className="col-span-2 text-xs text-fg-muted sm:order-4 sm:col-span-3">
        {v.emergency
          ? <span className="text-state-bad">emergency, </span>
          : null}
        {need
          ? <span className={TEXT_TONES[tone]}>needs {GiB(v.target_reclaim_bytes)} GiB, plan {GiB(v.planned_bytes)} GiB{v.covered === false ? `, ${GiB(v.eligible_bytes)} GiB eligible` : ''}</span>
          : 'under target'}
        <span className="text-fg-faint">
          {' · '}{GiB(v.used_bytes)} / {GiB(cap)} GiB · +{GiB(v.daily_ingest_bytes)} GiB/day · queue {GiB(v.queue_bytes)} GiB · {GiB(v.projected_used_bytes)} GiB in {window} d
          {(v.pending_bytes || 0) > 0 && <> · {GiB(v.pending_bytes)} GiB in recycle bin</>}
        </span>
        {(v.held_bytes || 0) > 0 && <span className="text-state-warn"> · {GiB(v.held_bytes)} GiB held</span>}
      </span>
    </li>
  );
}

/** Use bar, a lighter extension to the projected use, and the target (solid) and emergency (red) marks. */
function Gauge({ used, projected, marks, tone, label, className }) {
  return (
    <div className={`relative rounded-sm bg-ink-700 ${className}`}
      role="meter" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(barPct(used))}>
      <div className={`absolute inset-y-0 left-0 rounded-sm ${PROJECTED_BAR}`} style={{ width: `${barPct(projected)}%` }} />
      <div className={`absolute inset-y-0 left-0 rounded-sm ${BAR_TONES[tone]}`} style={{ width: `${barPct(used)}%` }} />
      <div className={`absolute -bottom-1 -top-1 ${TARGET_MARK}`} style={{ left: `${barPct(marks.target)}%` }} title={`Target ${pct(marks.target)}`} />
      <div className={`absolute -bottom-1 -top-1 ${EMERGENCY_MARK}`} style={{ left: `${barPct(marks.emergency)}%` }} title={`Emergency ${pct(marks.emergency)}`} />
    </div>
  );
}

const TEXT_TONES = { ok: 'text-state-ok', warn: 'text-state-warn', bad: 'text-state-bad' };
const BAR_TONES = { ok: 'bg-state-ok', warn: 'bg-state-warn', bad: 'bg-state-bad' };
const PROJECTED_BAR = 'bg-fg-faint/40';
const TARGET_MARK = 'w-0.5 -translate-x-1/2 bg-fg';
const EMERGENCY_MARK = 'w-0.5 -translate-x-1/2 bg-state-bad';
