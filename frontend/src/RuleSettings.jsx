import React, { useEffect, useState } from 'react';
import { ArrowDown, ArrowUp, Copy, Eye, Loader2, Pencil, Plus, X } from 'lucide-react';
import { previewRules } from './api.js';
import { GiB } from './ui.jsx';

// Settings > Rules: hard constraints on what the plan may take (see
// crates/flinch-archive/src/rules.rs). The list holds the rules exactly as
// settings.json does; the form edits one at a time; the YAML view shows the
// whole list read-only; the preview plans the daemon's last inputs under the
// saved rules and under this draft, and the page saves rule changes only once
// that draft was previewed.

const EFFECTS = [
  ['keep', 'Keep'],
  ['keep_until', 'Keep until…'],
  ['keep_latest_seasons', 'Keep the newest seasons'],
  ['keep_first_season', 'Keep the first season'],
  ['prefer_evict', 'Prefer evict'],
  ['must_evict', 'Must evict'],
];

const EVENTS = [
  ['added', 'it was added'],
  ['last_played', 'it was last played'],
  ['requested', 'it was requested'],
];

/** Every list condition: `[key, label, placeholder, numeric]`. */
const LISTS = [
  ['root_folders', 'Root folders', '/data/media/kids'],
  ['disks', 'Disks', 'volume key'],
  ['tags', '*arr tags', 'kids'],
  ['plex_sections', 'Plex sections', '1, 4', true],
  ['requesters', 'Requested by', 'Seerr name'],
  ['themes', 'Themes', 'theme name'],
  ['genres', 'Genres', 'Animation'],
  ['qualities', 'Quality', '2160p'],
];

/** Every range condition: `[key, label, unit]`. */
const RANGES = [
  ['size_gib', 'Size', 'GiB'],
  ['age_days', 'On disk', 'days'],
  ['last_played_days', 'Last played', 'days ago'],
  ['p_watch', 'P(watch)', '0–1'],
];

const NEW_RULE = { name: '', enabled: true, scope: {}, effect: { type: 'keep' } };

const inputClass = 'input min-h-[40px] w-full sm:min-h-0';
const selectClass = 'min-h-[40px] rounded-md border border-line bg-ink-900 px-2 text-[13px] text-fg focus:border-fg-faint focus:outline-none sm:min-h-0';

/** A scope without its empty conditions, as the server writes it. */
function clean(scope) {
  const out = {};
  for (const [key, value] of Object.entries(scope || {})) {
    if (value == null || (Array.isArray(value) && !value.length)) continue;
    if (typeof value === 'object' && !Array.isArray(value)) {
      const range = Object.fromEntries(Object.entries(value).filter(([, bound]) => bound != null));
      if (Object.keys(range).length) out[key] = range;
    } else out[key] = value;
  }
  return out;
}

/** The rules as the server expects them: clean scopes, no blank names. */
export function rulesPayload(rules) {
  return rules.map((rule) => ({ ...rule, name: rule.name.trim(), scope: clean(rule.scope) }));
}

function effectLabel(effect) {
  if (effect.type === 'keep_until') {
    const event = EVENTS.find(([key]) => key === effect.after)?.[1] || effect.after;
    return `Keep ${effect.days} days after ${event}`;
  }
  if (effect.type === 'keep_latest_seasons') return `Keep the newest ${effect.seasons} season(s) of continuing shows`;
  return EFFECTS.find(([key]) => key === effect.type)?.[1] || effect.type;
}

/** A new effect of `type`, keeping what the old one had in common. */
function effectOf(type, old) {
  if (type === 'keep_until') return { type, days: old.days || 60, after: old.after || 'requested' };
  if (type === 'keep_latest_seasons') return { type, seasons: old.seasons || 2 };
  return { type };
}

function scopeLabel(scope) {
  const parts = [];
  const s = clean(scope);
  if (s.kind) parts.push(s.kind === 'movie' ? 'movies' : 'seasons');
  for (const [key, label] of LISTS) if (s[key]) parts.push(`${label.toLowerCase()} ${s[key].join(', ')}`);
  if (s.played != null) parts.push(s.played ? 'played' : 'never played');
  for (const [key, label, unit] of RANGES) {
    if (!s[key]) continue;
    const { min, max } = s[key];
    parts.push(`${label.toLowerCase()} ${min != null && max != null ? `${min}–${max}` : min != null ? `≥ ${min}` : `≤ ${max}`} ${unit}`);
  }
  return parts.length ? parts.join(' · ') : 'everything';
}

/** Plain YAML for the read-only view: JSON-quoted strings stay valid YAML. */
function yamlScalar(value) {
  if (typeof value !== 'string') return String(value);
  return /^[A-Za-z][\w .'/()-]*$/.test(value) && !/^(true|false|null|yes|no|on|off|~)$/i.test(value) && !/[ ]$/.test(value)
    ? value : JSON.stringify(value);
}

export function toYaml(value, indent = '') {
  if (Array.isArray(value)) {
    if (!value.length) return '[]';
    return value.map((item) => {
      const body = toYaml(item, `${indent}  `);
      return typeof item === 'object' && item !== null && !Array.isArray(item)
        ? `${indent}- ${body.trimStart()}` : `${indent}- ${body}`;
    }).join('\n');
  }
  if (value && typeof value === 'object') {
    return Object.entries(value).map(([key, item]) => {
      if (item && typeof item === 'object' && (!Array.isArray(item) || item.length)) {
        return `${indent}${key}:\n${toYaml(item, `${indent}  `)}`;
      }
      return `${indent}${key}: ${Array.isArray(item) ? '[]' : yamlScalar(item)}`;
    }).join('\n');
  }
  return yamlScalar(value);
}

/** Text the user types, committed parsed; a list keeps its trailing comma while typed. */
function ListInput({ id, value, numeric, placeholder, onChange }) {
  const shown = (value || []).join(', ');
  const [text, setText] = useState(shown);
  useEffect(() => { if (text.split(',').map((s) => s.trim()).filter(Boolean).join(', ') !== shown) setText(shown); }, [shown]); // eslint-disable-line react-hooks/exhaustive-deps
  const commit = (raw) => {
    setText(raw);
    const entries = raw.split(',').map((s) => s.trim()).filter(Boolean);
    onChange(numeric ? entries.map(Number).filter((n) => Number.isInteger(n) && n >= 0) : entries);
  };
  return <input id={id} className={`${inputClass} sm:w-72`} placeholder={placeholder} value={text} onChange={(e) => commit(e.target.value)} />;
}

const bound = (raw) => (raw.trim() === '' || !Number.isFinite(Number(raw)) ? undefined : Number(raw));

function RuleForm({ rule, onChange }) {
  const scope = rule.scope || {};
  const setScope = (patch) => onChange({ ...rule, scope: { ...scope, ...patch } });
  const setEffect = (type) => onChange({ ...rule, effect: effectOf(type, rule.effect) });
  return (
    <div className="space-y-3 rounded-md border border-line-soft p-3">
      <div className="flex flex-wrap items-center gap-2">
        <input aria-label="Rule name" className={`${inputClass} sm:w-56`} placeholder="Name" value={rule.name}
          onChange={(e) => onChange({ ...rule, name: e.target.value })} />
        <select aria-label="Effect" className={selectClass} value={rule.effect.type} onChange={(e) => setEffect(e.target.value)}>
          {EFFECTS.map(([key, label]) => <option key={key} value={key}>{label}</option>)}
        </select>
        {rule.effect.type === 'keep_until' && (
          <>
            <input aria-label="Days" type="number" min={1} max={3650} className="input w-20 text-right tabular-nums" value={rule.effect.days}
              onChange={(e) => onChange({ ...rule, effect: { ...rule.effect, days: Number(e.target.value) } })} />
            <span className="text-fg-muted">days after</span>
            <select aria-label="Event" className={selectClass} value={rule.effect.after}
              onChange={(e) => onChange({ ...rule, effect: { ...rule.effect, after: e.target.value } })}>
              {EVENTS.map(([key, label]) => <option key={key} value={key}>{label}</option>)}
            </select>
          </>
        )}
        {rule.effect.type === 'keep_latest_seasons' && (
          <>
            <input aria-label="Seasons" type="number" min={1} max={100} className="input w-20 text-right tabular-nums" value={rule.effect.seasons}
              onChange={(e) => onChange({ ...rule, effect: { ...rule.effect, seasons: Number(e.target.value) } })} />
            <span className="text-fg-muted">newest seasons on disk, while the show continues; older ones may go</span>
          </>
        )}
        {rule.effect.type === 'keep_first_season' && (
          <span className="text-fg-muted">the first regular season stays so the show can be started; the rest may go</span>
        )}
      </div>
      <div className="grid gap-2 sm:grid-cols-[120px_minmax(0,1fr)] sm:items-center">
        <label className="text-fg-muted">Kind</label>
        <select aria-label="Kind" className={`${selectClass} sm:w-40`} value={scope.kind || ''} onChange={(e) => setScope({ kind: e.target.value || undefined })}>
          <option value="">Movies and seasons</option>
          <option value="movie">Movies</option>
          <option value="season">Seasons</option>
        </select>
        {LISTS.map(([key, label, placeholder, numeric]) => (
          <React.Fragment key={key}>
            <label htmlFor={`rule-${key}`} className="text-fg-muted">{label}</label>
            <ListInput id={`rule-${key}`} value={scope[key]} numeric={numeric} placeholder={placeholder} onChange={(list) => setScope({ [key]: list })} />
          </React.Fragment>
        ))}
        <label className="text-fg-muted">Played</label>
        <select aria-label="Played" className={`${selectClass} sm:w-40`} value={scope.played == null ? '' : String(scope.played)}
          onChange={(e) => setScope({ played: e.target.value === '' ? undefined : e.target.value === 'true' })}>
          <option value="">Either</option>
          <option value="true">Played</option>
          <option value="false">Never played</option>
        </select>
        {RANGES.map(([key, label, unit]) => (
          <React.Fragment key={key}>
            <label className="text-fg-muted">{label}</label>
            <span className="inline-flex flex-wrap items-center gap-2">
              {['min', 'max'].map((end) => (
                <input key={end} aria-label={`${label} ${end}`} placeholder={end} className="input w-24 text-right tabular-nums"
                  defaultValue={scope[key]?.[end] ?? ''} onChange={(e) => setScope({ [key]: { ...scope[key], [end]: bound(e.target.value) } })} />
              ))}
              <span className="text-fg-faint">{unit}</span>
            </span>
          </React.Fragment>
        ))}
      </div>
      <p className="text-[12px] text-fg-faint">Every condition set must hold; a list holds when any entry matches. A fact FLINCH could not read keeps the item and never evicts it.</p>
    </div>
  );
}

function Changes({ title, changes, tone }) {
  if (!changes.length) return null;
  return (
    <div>
      <p className={`font-medium ${tone}`}>{title} ({changes.length})</p>
      <ul className="mt-1 space-y-0.5">
        {changes.slice(0, 50).map((change) => (
          <li key={change.id} className="flex flex-wrap gap-x-2">
            <span className="text-fg">{change.title}</span>
            <span className="tabular-nums text-fg-muted">{GiB(change.size_bytes)} GiB · regret {change.regret.toFixed(2)}</span>
            <span className="text-fg-faint">otherwise: {change.kept_because}</span>
          </li>
        ))}
        {changes.length > 50 && <li className="text-fg-faint">…and {changes.length - 50} more</li>}
      </ul>
    </div>
  );
}

function PreviewResult({ diff }) {
  const totals = (t) => `${t.items} items · ${GiB(t.bytes)} GiB · regret ${t.regret.toFixed(2)}${t.covered ? '' : ' · target not met'}`;
  return (
    <div className="space-y-2 rounded-md border border-line-soft p-3">
      <p className="text-fg-muted">Saved rules: <span className="text-fg">{totals(diff.saved)}</span></p>
      <p className="text-fg-muted">This draft: <span className="text-fg">{totals(diff.draft)}</span></p>
      {!diff.added.length && !diff.removed.length && <p className="text-fg-faint">The plan would not change.</p>}
      <Changes title="Would leave" changes={diff.added} tone="text-state-bad" />
      <Changes title="Would stay" changes={diff.removed} tone="text-state-ok" />
      {diff.rules.length > 0 && (
        <p className="text-fg-muted">{diff.rules.map((rule) => `${rule.name}: ${rule.items} items, ${GiB(rule.bytes)} GiB`).join(' · ')}</p>
      )}
      {diff.uncertain > 0 && <p className="text-state-warn">{diff.uncertain} item(s) kept only because a fact a rule asks about is missing.</p>}
      {diff.conflicts.length > 0 && (
        <div>
          <p className="font-medium text-state-warn">Conflicts ({diff.conflicts.length}): keep wins</p>
          <ul className="mt-1 space-y-0.5 text-fg-muted">
            {diff.conflicts.slice(0, 20).map((conflict) => (
              <li key={conflict.id}>{conflict.title}: “{conflict.keep}” over “{conflict.evict}”</li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}

/** What the saved rules did on the daemon's last run (`status.json` `rules`). */
function LastRun({ status }) {
  if (!status) return null;
  return (
    <div className="space-y-1 text-[12px] text-fg-muted">
      <p>
        Last run: {status.rules.map((rule) => `${rule.name} ${rule.items} items (${GiB(rule.bytes)} GiB)`).join(' · ') || 'no enabled rule'}
        {` · ${status.forced} forced · ${status.uncertain} kept for a missing fact`}
      </p>
      {status.conflicts_total > 0 && (
        <p className="text-state-warn">
          {status.conflicts_total} conflict(s), keep won: {status.conflicts.slice(0, 5).map((c) => `${c.title} (“${c.keep}” over “${c.evict}”)`).join(', ')}
          {status.conflicts_total > 5 ? ', …' : ''}
        </p>
      )}
    </div>
  );
}

/**
 * The rule list. `onPreviewed` gets the payload a preview ran on, so the page
 * can tell whether the rules about to be saved were previewed; `lastRun` is
 * the status block of the saved rules.
 */
export function RulesEditor({ rules, onChange, onPreviewed, lastRun }) {
  const [editing, setEditing] = useState(null);
  const [view, setView] = useState('form');
  const [preview, setPreview] = useState({ state: 'idle' });
  const set = (index, rule) => onChange(rules.map((r, i) => (i === index ? rule : r)));
  const move = (index, by) => {
    const next = [...rules];
    [next[index], next[index + by]] = [next[index + by], next[index]];
    onChange(next);
    setEditing(editing === index ? index + by : editing);
  };
  const run = async () => {
    const payload = rulesPayload(rules);
    setPreview({ state: 'running' });
    try {
      const diff = await previewRules(payload);
      setPreview({ state: 'done', diff });
      onPreviewed(JSON.stringify(payload));
    } catch (err) {
      setPreview({ state: 'error', error: String(err.message || err) });
    }
  };
  const yaml = rules.length ? toYaml(rulesPayload(rules)) : '[]';
  return (
    <div className="w-full space-y-3">
      <div className="flex flex-wrap gap-2">
        <button className={`btn ${view === 'form' ? 'btn-primary' : ''}`} onClick={() => setView('form')}>Rules</button>
        <button className={`btn ${view === 'yaml' ? 'btn-primary' : ''}`} onClick={() => setView('yaml')}>YAML</button>
      </div>
      {view === 'yaml' ? (
        <div className="relative">
          <pre className="max-h-96 overflow-auto rounded-md border border-line-soft bg-ink-950 p-3 font-mono text-[12px] text-fg">{yaml}</pre>
          <button className="btn absolute right-2 top-2 px-2" aria-label="Copy YAML" onClick={() => navigator.clipboard?.writeText(yaml)}><Copy size={13} /></button>
        </div>
      ) : (
        <>
          {rules.map((rule, i) => (
            <div key={i} className="space-y-2">
              <div className="flex flex-wrap items-center gap-2">
                <input type="checkbox" aria-label={`Enable ${rule.name || 'rule'}`} checked={rule.enabled !== false}
                  onChange={(e) => set(i, { ...rule, enabled: e.target.checked })} />
                <span className="font-medium text-fg">{rule.name || 'Unnamed rule'}</span>
                <span className="text-fg-muted">{effectLabel(rule.effect)} · {scopeLabel(rule.scope)}</span>
                <span className="ml-auto inline-flex gap-1">
                  <button className="btn px-2" aria-label="Move up" disabled={i === 0} onClick={() => move(i, -1)}><ArrowUp size={13} /></button>
                  <button className="btn px-2" aria-label="Move down" disabled={i === rules.length - 1} onClick={() => move(i, 1)}><ArrowDown size={13} /></button>
                  <button className="btn px-2" aria-label={`Edit ${rule.name || 'rule'}`} onClick={() => setEditing(editing === i ? null : i)}><Pencil size={13} /></button>
                  <button className="btn px-2" aria-label={`Remove ${rule.name || 'rule'}`}
                    onClick={() => { onChange(rules.filter((_, j) => j !== i)); setEditing(null); }}><X size={13} /></button>
                </span>
              </div>
              {editing === i && <RuleForm rule={rule} onChange={(next) => set(i, next)} />}
            </div>
          ))}
          <button className="btn" onClick={() => { onChange([...rules, { ...NEW_RULE, scope: {} }]); setEditing(rules.length); }}>
            <Plus size={13} /> Add rule
          </button>
        </>
      )}
      <div className="flex flex-wrap items-center gap-3">
        <button className="btn" onClick={run} disabled={preview.state === 'running'}>
          {preview.state === 'running' ? <Loader2 size={13} className="animate-spin" /> : <Eye size={13} />} Preview changes
        </button>
        {preview.state === 'error' && <span className="text-[12px] text-state-bad">{preview.error}</span>}
      </div>
      {preview.state === 'done' && <PreviewResult diff={preview.diff} />}
      <LastRun status={lastRun} />
    </div>
  );
}
