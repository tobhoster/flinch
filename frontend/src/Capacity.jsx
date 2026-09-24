import React from 'react';
import { AlertTriangle } from 'lucide-react';
import { GiB, SectionTitle } from './ui.jsx';
import { Explain } from './Explain.jsx';

const pct = (p) => (p == null ? '—' : `${Math.round(p * 100)}%`);

/** Share of the bar, 0..100, for a fraction that may sit outside 0..1. */
const barPct = (f) => Math.max(0, Math.min(100, (f || 0) * 100));

const day = (unix) => new Date(unix * 1000).toLocaleDateString([], { month: 'short', day: 'numeric' });
const HELD_LIMIT = 5;

/**
 * Where a disk (or the aggregate) stands. A latched disk with nothing left to
 * free but evicted bytes still credited — in a recycle bin, or held by
 * something else — is waiting, not done.
 */
function phaseOf(v) {
  if (!v.latched) return 'idle';
  if (v.covered === false) return 'short';
  if (v.goal_bytes === 0 && (v.pending_bytes || 0) + (v.held_bytes || 0) > 0) return 'waiting';
  return 'freeing';
}
const PHASE_TONE = { idle: 'ok', freeing: 'warn', waiting: 'warn', short: 'bad' };

/**
 * Disk use against the watermarks. Crossing the ceiling latches eviction until
 * usage is back at the release mark; otherwise nothing is deleted. The daemon
 * only publishes `capacity` when it measured the *arr disks, so there is no
 * empty state.
 */
export default function Capacity({ c, eligible }) {
  const phase = phaseOf(c);
  const tone = PHASE_TONE[phase];
  const pending = c.pending_bytes || 0;
  const volumes = c.volumes || [];
  const unmatched = c.unmatched_roots || [];
  const untracked = c.untracked_bytes || 0;
  const held = volumes.flatMap((v) => v.held || []).sort((a, b) => b.bytes - a.bytes);
  const heldUntil = held.reduce((latest, h) => Math.max(latest, h.until || 0), 0);
  return (
    <section className="rounded-lg border border-line bg-ink-900 px-4 py-3">
      <SectionTitle hint={c.latched ? 'evicting' : 'idle'} term="watermarks">Storage</SectionTitle>
      <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1">
        <span>
          <span className={`num text-lg ${TEXT_TONES[tone]}`}>{((c.utilization || 0) * 100).toFixed(1)}%</span>
          <span className="text-fg-muted"> used{volumes.length > 1 ? ` across ${volumes.length} disks` : ''}</span>
        </span>
        <span className="num text-fg-muted">{GiB(c.used_bytes)} / {GiB(c.total_bytes)} GiB</span>
      </div>
      <Gauge className="mt-2 h-2" utilization={c.utilization} ceiling={c.ceiling} release={c.release} tone={tone} label="Storage used" />
      <div className="mt-2 flex flex-wrap justify-end gap-x-4 text-xs text-fg-faint">
        <span className="inline-flex items-center gap-1.5"><span className={`h-3 ${RELEASE_MARK}`} />release <span className="num">{pct(c.release)}</span></span>
        <span className="inline-flex items-center gap-1.5"><span className={`h-3 ${CEILING_MARK}`} />ceiling <span className="num">{pct(c.ceiling)}</span></span>
      </div>
      <p className="mt-1 text-fg-muted">
        {phase !== 'idle' && untracked > 0 && <>
          <span className="text-state-warn">
            Before evicting anything: <span className="num">{GiB(untracked)} GiB</span> here isn't library media.
          </span>
          {' '}<Explain term="untracked" />{' '}
        </>}
        {phase === 'idle' && <>
          Idle: nothing is deleted until usage reaches <span className="num">{pct(c.ceiling)}</span>.
          {' '}<span className="num text-fg">{GiB(eligible)} GiB</span> eligible when space is needed. <Explain term="eligible" />
        </>}
        {phase === 'freeing' && <>
          Freeing <span className="num text-state-warn">{GiB(c.goal_bytes)} GiB</span> to get back to <span className="num">{pct(c.release)}</span>,
          {' '}least regret per GiB first. <Explain term="eviction_order" />
          {' '}{c.goal_met
            ? 'All of it is handed to Maintainerr, which deletes it on its own schedule.'
            : <><span className="num text-fg">{GiB(c.handed_bytes)} GiB</span> handed to Maintainerr so far.</>}
        </>}
        {phase === 'short' && <>
          <span className="text-state-bad">
            Needs <span className="num">{GiB(c.goal_bytes)} GiB</span> but only <span className="num">{GiB(eligible)} GiB</span> is eligible.
          </span>
          {c.armed_never_played ? ' Review holds.' : ' Arm never-played in Settings or review holds.'} <Explain term="eligible" />
        </>}
      </p>
      {phase === 'idle' && untracked > 0 && (
        <p className="mt-1 text-fg-muted">
          <span className="num text-fg">{GiB(untracked)} GiB</span> of {volumes.length > 1 ? 'these disks' : 'this disk'} isn't library media:
          {' '}downloads, recycle bins and files no app tracks. <Explain term="untracked" />
        </p>
      )}
      {c.latched && pending > 0 && (
        <p className="mt-1 text-fg-muted">
          <span className="num text-state-warn">{GiB(pending)} GiB</span> evicted, waiting for the recycle bin to release it. <Explain term="pending" />
        </p>
      )}
      {volumes.length > 1 && (
        <ul className="mt-3 divide-y divide-line-soft border-t border-line-soft">
          {volumes.map((v) => <VolumeRow key={v.path} v={v} ceiling={c.ceiling} release={c.release} />)}
        </ul>
      )}
      {unmatched.length > 0 && (
        <p className="mt-3 flex gap-2 rounded-md bg-state-warn/10 px-3 py-2 text-state-warn">
          <AlertTriangle size={14} aria-hidden className="mt-0.5 shrink-0" />
          <span className="min-w-0 break-words">
            Not governed: <span className="font-mono text-fg">{unmatched.join(', ')}</span>.
            {' '}<span className="text-fg-muted">Items there are never evicted because no disk FLINCH can measure holds them.</span>
            {' '}<Explain term="not_governed" />
          </span>
        </p>
      )}
      {(c.held_bytes || 0) > 0 && (
        <div className="mt-3 flex gap-2 rounded-md bg-state-warn/10 px-3 py-2 text-state-warn">
          <AlertTriangle size={14} aria-hidden className="mt-0.5 shrink-0" />
          <div className="min-w-0 break-words">
            <p>
              <span className="num">{GiB(c.held_bytes)} GiB</span> handed over was never freed:
              {' '}<span className="text-fg-muted">
                the recycle bin's window passed and the disk did not drop. Something else still holds it, often a torrent seeding the same file.
                {' '}FLINCH counts it as on its way out and evicts nothing more for it{heldUntil > 0 ? <> until <span className="num text-fg">{day(heldUntil)}</span></> : ''}.
              </span>
              {' '}<Explain term="held" />
            </p>
            {held.length > 0 && (
              <ul className="mt-1 space-y-0.5 text-xs text-fg-muted">
                {held.slice(0, HELD_LIMIT).map((h) => (
                  <li key={h.id} className="truncate">
                    <span className="text-fg">{h.title || h.id}</span>
                    {' · '}<span className="num">{GiB(h.bytes)} GiB</span>
                    {h.held_since > 0 && <>{' · '}since {day(h.held_since)}</>}
                  </li>
                ))}
                {held.length > HELD_LIMIT && <li className="text-fg-faint">+{held.length - HELD_LIMIT} more</li>}
              </ul>
            )}
          </div>
        </div>
      )}
    </section>
  );
}

/** One disk: path, a mini gauge with both markers, and what it is doing. */
function VolumeRow({ v, ceiling, release }) {
  const phase = phaseOf(v);
  const tone = PHASE_TONE[phase];
  const pending = v.pending_bytes || 0;
  const state = {
    idle: 'idle',
    freeing: `freeing ${GiB(v.goal_bytes)} GiB, ${GiB(v.handed_bytes)} handed`,
    waiting: pending > 0 ? `waiting on recycle bin, ${GiB(pending)} GiB` : 'waiting on space not yet freed',
    short: `needs ${GiB(v.goal_bytes)} GiB, ${GiB(v.eligible_bytes)} GiB eligible`,
  }[phase];
  return (
    <li className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1 py-2 sm:grid-cols-[minmax(0,9rem)_minmax(0,1fr)_3rem_minmax(0,19rem)]">
      <span className="truncate font-mono text-fg" title={v.path}>{v.path}</span>
      <span className={`num text-right sm:order-3 ${TEXT_TONES[tone]}`}>{pct(v.utilization)}</span>
      <Gauge className="col-span-2 h-1.5 sm:order-2 sm:col-span-1" utilization={v.utilization} ceiling={ceiling} release={release} tone={tone} label={`${v.path} used`} />
      <span className="col-span-2 text-xs text-fg-muted sm:order-4 sm:col-span-1">
        <span className={phase === 'idle' ? '' : TEXT_TONES[tone]}>{state}</span>
        {phase !== 'waiting' && v.latched && pending > 0 && <>, {GiB(pending)} GiB in recycle bin</>}
        {(v.held_bytes || 0) > 0 && <span className="text-state-warn">, {GiB(v.held_bytes)} GiB held</span>}
        <span className="text-fg-faint"> · {GiB(v.used_bytes)} / {GiB(v.total_bytes)} GiB</span>
        {(v.untracked_bytes || 0) > 0 && <span className="text-fg-faint"> · {GiB(v.untracked_bytes)} GiB not library media</span>}
      </span>
    </li>
  );
}

/** A use bar with the release mark (dotted) and the ceiling (solid). */
function Gauge({ utilization, ceiling, release, tone, label, className }) {
  return (
    <div className={`relative rounded-sm bg-ink-700 ${className}`}
      role="meter" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(barPct(utilization))}>
      <div className={`h-full rounded-sm ${BAR_TONES[tone]}`} style={{ width: `${barPct(utilization)}%` }} />
      <div className={`absolute -bottom-1 -top-1 ${RELEASE_MARK}`} style={{ left: `${barPct(release)}%` }} title={`Release ${pct(release)}`} />
      <div className={`absolute -bottom-1 -top-1 ${CEILING_MARK}`} style={{ left: `${barPct(ceiling)}%` }} title={`Ceiling ${pct(ceiling)}`} />
    </div>
  );
}

const TEXT_TONES = { ok: 'text-state-ok', warn: 'text-state-warn', bad: 'text-state-bad' };
const BAR_TONES = { ok: 'bg-state-ok', warn: 'bg-state-warn', bad: 'bg-state-bad' };
/** The ceiling is a solid tick; the release mark a lighter dotted one. */
const CEILING_MARK = 'w-0.5 -translate-x-1/2 bg-fg';
const RELEASE_MARK = 'w-0 -translate-x-1/2 border-l-2 border-dotted border-fg-muted';
