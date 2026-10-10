import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Check, ChevronDown, ChevronRight, Loader2, SlidersHorizontal, Upload } from 'lucide-react';
import { applyTrash, loadTrashDiff } from './api.js';
import { Card, EmptyState, SectionTitle, ago } from './ui.jsx';
import { instanceLabel } from './arrRef.js';

// The Quality profiles tab: the daemon's preview of the TRaSH-Guides sync,
// one checkbox per change. Nothing is written until the operator applies a
// selection; the daemon then applies it at its next run (a dry run prints it).

const KIND = { custom_format: 'Custom formats', quality_profile: 'Quality profiles', quality_definition: 'Quality sizes' };
const ACTION_TONE = { create: 'text-state-ok', update: 'text-state-info', delete: 'text-state-bad' };
const POLL_MS = 5000;
// The daemon waits at least a minute between runs, and a run can take a few.
const GIVE_UP_MS = 15 * 60 * 1000;

const now = () => Math.floor(Date.now() / 1000);

/** Selecting a change selects what it needs (a profile needs its formats). */
function withRequirements(ids, changes) {
  const byId = new Map(changes.map((change) => [change.id, change]));
  const out = new Set(ids);
  const stack = [...ids];
  while (stack.length) {
    for (const need of byId.get(stack.pop())?.requires || []) {
      if (!out.has(need) && byId.has(need)) { out.add(need); stack.push(need); }
    }
  }
  return out;
}

function FieldDiff({ fields }) {
  return (
    <table className="mt-2 w-full table-fixed text-[12px]">
      <tbody className="divide-y divide-line-soft">
        {fields.map((field, i) => (
          <tr key={i} className="align-top">
            <td className="w-1/4 py-1 pr-3 text-fg-muted">{field.field}</td>
            <td className="w-[37%] break-words py-1 pr-3 font-mono text-state-bad/80">{field.from ?? '—'}</td>
            <td className="w-[38%] break-words py-1 font-mono text-state-ok/90">{field.to ?? '—'}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

const GIB = 1024 ** 3;
const pct = (ratio) => `${(ratio * 100).toFixed(1)}%`;

/** The GiB estimate beside a profile or size change: items × cached releases. */
function ImpactLine({ impact }) {
  if (!impact) return null;
  const gib = impact.delta_bytes / GIB;
  const tone = gib > 0.05 ? 'text-state-warn' : gib < -0.05 ? 'text-state-ok' : 'text-fg-faint';
  const disk = impact.utilization_before != null && impact.utilization_after != null
    ? ` · disk forecast ${pct(impact.utilization_before)} → ${pct(impact.utilization_after)}` : '';
  return (
    <div className="ml-6 mt-1 text-[11px]">
      <p className={tone}>
        {gib >= 0 ? '+' : ''}{gib.toFixed(1)} GiB once upgrades settle · {impact.items} item{impact.items === 1 ? '' : 's'} ({impact.sampled} with a cached release search){disk}
      </p>
      {impact.warnings?.map((warning, i) => <p key={i} className="text-state-warn">{warning}</p>)}
    </div>
  );
}

function ChangeRow({ change, impact, checked, forced, onToggle }) {
  const [open, setOpen] = useState(false);
  return (
    <li className="py-2">
      <div className="flex items-center gap-2">
        <input type="checkbox" aria-label={`Select ${change.name}`} checked={checked} onChange={() => onToggle(change.id)} />
        <button className="inline-flex min-w-0 items-center gap-1 text-left" onClick={() => setOpen(!open)} aria-expanded={open}>
          {open ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
          <span className={`w-14 shrink-0 text-[11px] uppercase ${ACTION_TONE[change.action] || ''}`}>{change.action}</span>
          <span className="truncate text-fg">{change.name}</span>
        </button>
        <span className="ml-auto shrink-0 text-[11px] text-fg-faint">
          {forced ? 'needed by a selected profile' : `${change.fields?.length || 0} field${change.fields?.length === 1 ? '' : 's'}`}
        </span>
      </div>
      <ImpactLine impact={impact} />
      {open && change.fields?.length > 0 && <FieldDiff fields={change.fields} />}
    </li>
  );
}

function AppPreview({ state, selected, forced, onToggle }) {
  const groups = Object.entries(KIND).map(([kind, label]) => [label, state.changes.filter((change) => change.kind === kind)]).filter(([, rows]) => rows.length);
  const source = state.source === 'pcd' ? 'Profilarr database' : 'TRaSH-Guides';
  return (
    <Card className="p-4 sm:p-5">
      <SectionTitle hint={state.error ? undefined : `${source} · ${state.changes.length} pending · ${state.in_sync} in sync${state.compact_profile_id ? ` · compact profile #${state.compact_profile_id}` : ''}`}>
        {instanceLabel(state.app, state.instance)}
      </SectionTitle>
      {state.error && <p className="text-[12px] text-state-bad">{state.error}</p>}
      {state.problems?.map((problem, i) => <p key={i} className="text-[12px] text-state-warn">{problem}</p>)}
      {!state.error && !state.changes.length && <p className="text-[12px] text-fg-muted">Everything FLINCH manages here matches the guide and your overrides.</p>}
      {groups.map(([label, rows]) => (
        <div key={label} className="mt-3">
          <h3 className="text-[12px] font-medium text-fg-muted">{label}</h3>
          <ul className="divide-y divide-line-soft">
            {rows.map((change) => (
              <ChangeRow key={change.id} change={change} impact={state.impacts?.[change.id]} checked={selected.has(change.id) || forced.has(change.id)}
                forced={forced.has(change.id) && !selected.has(change.id)} onToggle={onToggle} />
            ))}
          </ul>
        </div>
      ))}
    </Card>
  );
}

function LastApply({ record }) {
  if (!record) return null;
  const sum = (key) => record.apps.reduce((n, app) => n + (app[key]?.length || 0), 0);
  const printed = record.apps.flatMap((app) => (app.printed || []).map((line) => `${app.instance ? `${app.app}@${app.instance}` : app.app}: ${line}`));
  const failed = record.apps.flatMap((app) => app.failed || []);
  return (
    <Card className="p-4 text-[12px] sm:p-5">
      <SectionTitle hint={`${ago(Math.max(0, now() - record.at_unix))}${record.automatic ? ' · automatic' : ''}`}>Last apply</SectionTitle>
      {record.dry_run
        ? <p className="text-fg-muted">Dry run: {printed.length} request(s) printed, nothing sent. Turn the planner’s dry run off to write.</p>
        : <p className="text-fg-muted">{sum('applied')} applied and read back · {sum('unverified')} not confirmed · {failed.length} failed</p>}
      {failed.map((f) => <p key={f.id} className="text-state-bad">{f.id}: {f.error}</p>)}
      {record.apps.flatMap((app) => app.unverified || []).map((id) => <p key={id} className="text-state-warn">{id}: accepted, but still pending when read back</p>)}
      {printed.length > 0 && <pre className="mt-2 max-h-48 overflow-auto whitespace-pre-wrap font-mono text-fg-faint">{printed.join('\n')}</pre>}
    </Card>
  );
}

export default function QualityProfiles() {
  const [preview, setPreview] = useState(undefined);
  const [selected, setSelected] = useState(new Set());
  const [state, setState] = useState('idle'); // idle | sending | queued | error
  const [message, setMessage] = useState('');
  const timer = useRef(null);
  // The last apply before ours, and when ours was queued: ours has landed
  // once a newer apply is published.
  const queued = useRef({ before: 0, at: 0 });

  const refresh = useCallback(() => loadTrashDiff().then(setPreview).catch(() => setPreview(null)), []);
  useEffect(() => { refresh(); return () => clearTimeout(timer.current); }, [refresh]);
  // While an apply waits for the daemon, follow it until it lands.
  useEffect(() => {
    clearTimeout(timer.current);
    if (state === 'queued' && preview && (preview.last_apply?.at_unix || 0) > queued.current.before) {
      setState('idle');
      setSelected(new Set());
      return;
    }
    if (state === 'queued' && Date.now() - queued.current.at > GIVE_UP_MS) {
      setMessage('The daemon has not applied it yet. Is flinch-arrd running? The request stays queued.');
      setState('error');
      return;
    }
    if (preview?.apply_pending || state === 'queued') timer.current = setTimeout(refresh, POLL_MS);
  }, [preview, state, refresh]);

  if (preview === undefined) return <Card className="mt-6 p-5 text-[13px] text-fg-muted">Loading the preview…</Card>;
  if (!preview || !preview.enabled) {
    return (
      <div className="mt-6">
        <EmptyState icon={SlidersHorizontal} title={preview ? 'The quality sync is off' : 'No preview yet'}
          body="Switch it on in Settings → Quality profiles (TRaSH); the daemon previews at its next run. FLINCH then lists every change to Radarr’s and Sonarr’s custom formats, profiles and sizes here, and writes nothing until you apply." />
      </div>
    );
  }

  const changes = preview.apps.flatMap((app) => app.changes);
  const forced = withRequirements(selected, changes);
  const toggle = (id) => {
    const next = new Set(selected);
    if (next.has(id)) next.delete(id); else next.add(id);
    setSelected(next);
  };
  const selectAll = () => setSelected(new Set(changes.filter((change) => change.action !== 'delete').map((change) => change.id)));
  const apply = async () => {
    setState('sending');
    try {
      queued.current = { before: preview.last_apply?.at_unix || 0, at: Date.now() };
      await applyTrash([...forced]);
      setMessage(preview.dry_run ? 'Queued. Dry run: the daemon prints the requests at its next run and sends none.' : 'Queued. The daemon applies it at its next run, then reads it back.');
      setState('queued');
      refresh();
    } catch (err) {
      setMessage(String(err.message || err));
      setState('error');
    }
  };
  const sources = [
    preview.guide_commit && `TRaSH-Guides ${preview.guide_commit.slice(0, 8)}`,
    preview.pcd_commit && `Profilarr database ${preview.pcd_commit.slice(0, 8)}${preview.pcd_license ? ` (${preview.pcd_license})` : ''}`,
  ].filter(Boolean).join(' · ') || '—';

  return (
    <div className="mt-6 max-w-4xl space-y-4 text-[13px]">
      <Card className="p-4 sm:p-5">
        <SectionTitle term="trash_sync" hint={`${sources} · previewed ${ago(Math.max(0, now() - preview.refreshed_at_unix))}${preview.apply_automatically ? ' · applies automatically' : ''}`}>
          Quality profiles
        </SectionTitle>
        {preview.error && <p className="text-[12px] text-state-bad">{preview.error}</p>}
        {preview.dry_run && <p className="text-[12px] text-state-warn">Dry run: applying prints every request in the daemon log and sends none.</p>}
        <p className="text-[12px] text-fg-faint">GiB estimates are where upgrades settle: items on the profile × their cached Prowlarr release sizes, not what is on disk today. A profile is deleted only when you select it here.</p>
        <div className="mt-3 flex flex-wrap items-center gap-2">
          <button className="btn btn-primary" disabled={!forced.size || state === 'sending' || !!preview.apply_pending} onClick={apply}>
            {state === 'sending' ? <Loader2 size={14} className="animate-spin" /> : <Upload size={14} />}
            Apply {forced.size || ''} selected
          </button>
          <button className="btn" onClick={selectAll} disabled={!changes.length}><Check size={13} /> Select all but deletions</button>
          <button className="btn" onClick={() => setSelected(new Set())} disabled={!selected.size}>Clear</button>
          {preview.apply_pending && <span className="text-[12px] text-fg-muted">An apply of {preview.apply_pending.changes?.length} change(s) is waiting for the daemon…</span>}
          {state === 'error' && <span className="text-[12px] text-state-bad">{message}</span>}
          {state === 'queued' && !preview.apply_pending && <span className="text-[12px] text-fg-muted">{message}</span>}
        </div>
      </Card>
      {preview.apps.map((app) => <AppPreview key={`${app.app}@${app.instance || ''}`} state={app} selected={selected} forced={forced} onToggle={toggle} />)}
      <LastApply record={preview.last_apply} />
    </div>
  );
}
