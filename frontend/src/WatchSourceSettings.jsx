import React from 'react';
import { Plus, X } from 'lucide-react';

// The Tracearr and Trakt sources of Settings > Watch sources. FLINCH reads
// their play logs as watch evidence; a token never enters settings.json, only
// the name of the flinch-arrd environment variable holding it.

const KINDS = [
  ['tracearr', 'Tracearr'],
  ['trakt', 'Trakt'],
];

const NEW_SOURCE = { kind: 'tracearr', name: '', url: '', token_env: '', client_id_env: '', retention_days: 0 };

const inputClass = 'input min-h-[40px] w-full sm:min-h-0';
const selectClass = 'min-h-[40px] rounded-md border border-line bg-ink-900 px-2 text-[13px] text-fg focus:border-fg-faint focus:outline-none sm:min-h-0';

/** The list as the form edits it: every field present. */
export function watchSourcesForm(sources) {
  return (sources || []).map((source) => ({ ...NEW_SOURCE, ...source }));
}

/** The list as the PUT sends it: trimmed names, a whole number of days. */
export function watchSourcesPayload(sources) {
  return sources.map((source) => ({
    ...source,
    name: source.name.trim(),
    url: source.url.trim(),
    token_env: source.token_env.trim(),
    client_id_env: source.client_id_env.trim(),
    retention_days: Math.max(0, Math.round(Number(source.retention_days) || 0)),
  }));
}

/** The editable list of sources; `onChange` gets the whole new list. */
export function WatchSources({ sources, onChange }) {
  const set = (index, patch) => onChange(sources.map((source, i) => (i === index ? { ...source, ...patch } : source)));
  const envName = (value) => value.toUpperCase();
  return (
    <div className="w-full space-y-3">
      {sources.map((source, i) => (
        <div key={i} className="space-y-2 rounded-md border border-line-soft p-3">
          <div className="flex flex-wrap items-center gap-2">
            <select id={`watch-sources-kind-${i}`} aria-label="Source kind" className={selectClass} value={source.kind}
              onChange={(e) => set(i, { kind: e.target.value })}>
              {KINDS.map(([key, label]) => <option key={key} value={key}>{label}</option>)}
            </select>
            <input aria-label="Name" className={`${inputClass} sm:w-48`} placeholder={source.kind === 'trakt' ? 'Whose account, e.g. ann' : 'Name (optional)'}
              autoComplete="off" spellCheck={false} value={source.name} onChange={(e) => set(i, { name: e.target.value })} />
            <button className="btn ml-auto px-2" aria-label="Remove source" onClick={() => onChange(sources.filter((_, j) => j !== i))}>
              <X size={13} />
            </button>
          </div>
          <input aria-label="URL" className={inputClass} autoComplete="off" spellCheck={false}
            placeholder={source.kind === 'trakt' ? 'https://api.trakt.tv (blank)' : 'http://tracearr:3000'}
            value={source.url} onChange={(e) => set(i, { url: e.target.value })} />
          <div className="flex flex-wrap gap-2">
            <input aria-label="Token variable" className={`${inputClass} sm:w-72`} autoComplete="off" spellCheck={false}
              placeholder={source.kind === 'trakt' ? 'Access token variable, e.g. FLINCH_TRAKT_ANN' : 'API key variable, e.g. FLINCH_TRACEARR_KEY'}
              value={source.token_env} onChange={(e) => set(i, { token_env: envName(e.target.value) })} />
            {source.kind === 'trakt' ? (
              <input aria-label="Client id variable" className={`${inputClass} sm:w-72`} autoComplete="off" spellCheck={false}
                placeholder="Client id variable, e.g. FLINCH_TRAKT_CLIENT_ID"
                value={source.client_id_env} onChange={(e) => set(i, { client_id_env: envName(e.target.value) })} />
            ) : (
              <label className="flex items-center gap-2 text-[13px] text-fg-muted">
                History kept
                <input aria-label="Retention days" type="number" min={0} max={36500} step={1} className={`${inputClass} sm:w-24`}
                  value={source.retention_days} onChange={(e) => set(i, { retention_days: e.target.value })} />
                days (0 = all)
              </label>
            )}
          </div>
        </div>
      ))}
      <button className="btn px-2.5 text-xs" disabled={sources.length >= 16} onClick={() => onChange([...sources, { ...NEW_SOURCE }])}>
        <Plus size={13} /> Add source
      </button>
    </div>
  );
}

/** One line for the status cell: per source, complete or not and what joined. */
export function watchSourcesSummary(status) {
  const sources = status?.watch_sources?.sources;
  if (!sources?.length) return '—';
  return sources
    .map((s) => `${s.source}: ${s.complete ? `${s.plays} plays, ${s.joined} items` : 'incomplete'}${s.never_played ? `, ${s.never_played} never played` : ''}`)
    .join(' · ');
}
