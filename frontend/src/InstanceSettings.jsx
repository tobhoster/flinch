import React from 'react';
import { Plus, X } from 'lucide-react';
import { INSTANCE_NAME, instanceLabel } from './arrRef.js';

// Settings > Instances: Radarr and Sonarr instances beyond the default pair
// (a 4K Radarr, an anime Sonarr). An API key never enters settings.json, only
// the name of the flinch-arrd environment variable holding it. The name is
// part of every card id of the instance (`radarr@4k-7`).

const APPS = [['radarr', 'Radarr'], ['sonarr', 'Sonarr']];
const MAX_PER_APP = 8;
const KEY_ENV = /^[A-Z0-9_]{1,64}$/;
const HTTP = /^https?:\/\//;

const NEW_INSTANCE = { app: 'radarr', name: '', url: '', key_env: '', archive_root: '', compact_profile: '', public_url: '' };

const inputClass = 'input min-h-[40px] w-full sm:min-h-0';
const selectClass = 'min-h-[40px] rounded-md border border-line bg-ink-900 px-2 text-[13px] text-fg focus:border-fg-faint focus:outline-none sm:min-h-0';

/** The form's copy of `settings.instances`: every field a string. */
export function instancesForm(instances) {
  return (instances || []).map((instance) => ({ ...NEW_INSTANCE, ...instance }));
}

/** `{ instances, invalid }`: trimmed rows for the PUT body, and the labels at fault. */
export function instancesPayload(rows) {
  const instances = rows.map((row) => ({
    app: row.app,
    name: row.name.trim(),
    url: row.url.trim(),
    key_env: row.key_env.trim(),
    archive_root: row.archive_root.trim(),
    compact_profile: row.compact_profile.trim(),
    public_url: row.public_url.trim(),
  }));
  const invalid = [];
  for (const [app, label] of APPS) {
    if (instances.filter((i) => i.app === app).length > MAX_PER_APP) invalid.push(`${label} instances (at most ${MAX_PER_APP})`);
  }
  instances.forEach((instance, index) => {
    const label = instanceLabel(instance.app, instance.name || `#${index + 1}`);
    if (!INSTANCE_NAME.test(instance.name)) invalid.push(`${label} name (1–24 of a-z, 0-9, _)`);
    else if (instances.slice(0, index).some((other) => other.app === instance.app && other.name === instance.name)) invalid.push(`${label} name (used twice)`);
    if (!HTTP.test(instance.url)) invalid.push(`${label} URL (http:// or https://)`);
    if (instance.public_url && !HTTP.test(instance.public_url)) invalid.push(`${label} browser URL (http:// or https://)`);
    if (!KEY_ENV.test(instance.key_env)) invalid.push(`${label} API key variable (a variable name: A-Z, 0-9, _)`);
  });
  return { instances, invalid };
}

/** The editable list of extra instances; `onChange` gets the whole new list. */
export function InstanceList({ instances, onChange }) {
  const set = (index, patch) => onChange(instances.map((instance, i) => (i === index ? { ...instance, ...patch } : instance)));
  const full = APPS.every(([app]) => instances.filter((i) => i.app === app).length >= MAX_PER_APP);
  return (
    <div className="w-full space-y-3">
      {instances.map((instance, i) => {
        const keyBad = instance.key_env.trim() !== '' && !KEY_ENV.test(instance.key_env.trim());
        const nameBad = instance.name.trim() !== '' && !INSTANCE_NAME.test(instance.name.trim());
        return (
          <div key={i} className="space-y-2 rounded-md border border-line-soft p-3">
            <div className="flex flex-wrap items-center gap-2">
              <select id={`instances-app-${i}`} aria-label="App" className={selectClass} value={instance.app}
                onChange={(e) => set(i, { app: e.target.value })}>
                {APPS.map(([key, label]) => <option key={key} value={key}>{label}</option>)}
              </select>
              <input aria-label="Instance name" className={`${inputClass} sm:w-40`} placeholder="Name, e.g. 4k" autoComplete="off" spellCheck={false}
                value={instance.name} onChange={(e) => set(i, { name: e.target.value.toLowerCase() })} />
              <span className="text-[12px] text-fg-faint">{instance.name.trim() ? `ids ${instance.app}@${instance.name.trim()}-…` : ''}</span>
              <button className="btn ml-auto px-2" aria-label="Remove instance" onClick={() => onChange(instances.filter((_, j) => j !== i))}>
                <X size={13} />
              </button>
            </div>
            {nameBad && <p className="text-[12px] text-state-bad">A name is 1–24 of a-z, 0-9 and _ (no dash).</p>}
            <input aria-label="URL" className={inputClass} placeholder={instance.app === 'radarr' ? 'http://radarr-4k:7878' : 'http://sonarr-anime:8989'}
              autoComplete="off" spellCheck={false} value={instance.url} onChange={(e) => set(i, { url: e.target.value })} />
            <input aria-label="API key variable" className={inputClass}
              placeholder={`API key variable name, e.g. ${instance.app.toUpperCase()}_${(instance.name.trim() || 'NAME').toUpperCase()}_API_KEY`}
              autoComplete="off" spellCheck={false} value={instance.key_env} onChange={(e) => set(i, { key_env: e.target.value.toUpperCase() })} />
            {keyBad && <p className="text-[12px] text-state-bad">The name of an environment variable (A-Z, 0-9, _), never the key itself.</p>}
            <div className="flex flex-wrap gap-2">
              <input aria-label="Archive root" className={`${inputClass} sm:w-56`} placeholder="Archive root (blank: never)" autoComplete="off" spellCheck={false}
                value={instance.archive_root} onChange={(e) => set(i, { archive_root: e.target.value })} />
              <input aria-label="Compact profile" className={`${inputClass} sm:w-56`} placeholder="Compact profile (blank: none)" autoComplete="off" spellCheck={false}
                value={instance.compact_profile} onChange={(e) => set(i, { compact_profile: e.target.value })} />
            </div>
            <input aria-label="Browser URL" className={inputClass}
              placeholder={`Browser URL (blank: the sibling host ${instance.app}-${instance.name.trim() || 'name'}.<your domain>)`}
              autoComplete="off" spellCheck={false} value={instance.public_url} onChange={(e) => set(i, { public_url: e.target.value })} />
          </div>
        );
      })}
      <button className="btn px-2.5 text-xs" disabled={full} onClick={() => onChange([...instances, { ...NEW_INSTANCE }])}>
        <Plus size={13} /> Add instance
      </button>
    </div>
  );
}

/** The instances the daemon used last run, from status.json `arr_instances`. */
export function InUse({ status }) {
  const instances = status?.arr_instances || [];
  if (!instances.length) return <span className="text-fg-faint">Not reported yet: the next run lists them.</span>;
  return (
    <ul className="space-y-0.5">
      {instances.map((instance) => (
        <li key={`${instance.app}@${instance.name}`} className="text-fg-muted">
          <span className="text-fg">{instanceLabel(instance.app, instance.name)}</span>
          {!instance.name && <span className="text-fg-faint"> (default)</span>}
          <span className="break-all text-fg-faint"> · {instance.url}{instance.public_url ? ` · opens ${instance.public_url}` : ''}</span>
        </li>
      ))}
    </ul>
  );
}
