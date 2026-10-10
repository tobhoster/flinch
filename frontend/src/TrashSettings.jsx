import React from 'react';
import { RotateCcw } from 'lucide-react';
import { INSTANCE_NAME } from './arrRef.js';

// Settings → Quality profiles (TRaSH): each app's sync config as JSON, in
// Recyclarr's field names, so a Recyclarr config reads across. "FLINCH
// preset" drops the app's config from the saved file, which the daemon then
// fills with the built-in preset. Extra instances (Settings → Instances) sync
// only with an entry under `instances.extra`, keyed `radarr@4k`.

const APPS = [['radarr', 'Radarr'], ['sonarr', 'Sonarr']];

/** The form's copy of `settings.trash`: the instances as editable text. */
export function trashForm(trash = {}) {
  const text = (app) => JSON.stringify(trash.instances?.[app] ?? {}, null, 2);
  return {
    ...trash,
    radarr: text('radarr'),
    sonarr: text('sonarr'),
    extra: JSON.stringify(trash.instances?.extra ?? {}, null, 2),
    preset: { radarr: false, sonarr: false },
  };
}

const isObject = (value) => !!value && typeof value === 'object' && !Array.isArray(value);
const extraKey = (key) => {
  const [app, name, ...more] = key.split('@');
  return !more.length && (app === 'radarr' || app === 'sonarr') && INSTANCE_NAME.test(name || '');
};

/** `{ trash, invalid }`: the settings to save, and the labels at fault. */
export function trashPayload(form) {
  const invalid = [];
  const { radarr, sonarr, extra, preset, instances: _saved, ...rest } = form;
  const hours = Number(form.schedule_hours);
  if (!Number.isInteger(hours) || hours < 1 || hours > 720) invalid.push('TRaSH schedule (1–720 hours)');
  const commit = String(form.guide_commit || '').trim();
  if (!/^[0-9a-f]{40}$/.test(commit)) invalid.push('TRaSH-Guides commit (40 hex digits)');
  const pcd = Object.fromEntries(Object.entries(form.pcd || {}).map(([key, value]) => [key, String(value || '').trim()]));
  if (![pcd.commit, pcd.schema_commit].every((c) => /^[0-9a-f]{40}$/.test(c || ''))) invalid.push('Profilarr database commits (40 hex digits)');
  if (![pcd.repository, pcd.schema_repository].every((r) => /^[\w.-]+\/[\w.-]+$/.test(r || ''))) invalid.push('Profilarr database repositories (owner/name)');
  const instances = {};
  for (const [app, label] of APPS) {
    if (preset[app]) continue;
    try {
      const parsed = JSON.parse({ radarr, sonarr }[app]);
      if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) instances[app] = parsed;
      else invalid.push(`${label} quality config (a JSON object)`);
    } catch {
      invalid.push(`${label} quality config (JSON)`);
    }
  }
  try {
    const parsed = JSON.parse(extra || '{}');
    if (!isObject(parsed) || !Object.entries(parsed).every(([key, config]) => extraKey(key) && isObject(config))) {
      invalid.push('Extra instances quality config (an object keyed radarr@name or sonarr@name)');
    } else if (Object.keys(parsed).length) {
      instances.extra = parsed;
    }
  } catch {
    invalid.push('Extra instances quality config (JSON)');
  }
  return { trash: { ...rest, schedule_hours: hours, guide_commit: commit, pcd, instances }, invalid };
}

/** One textarea per app, with a reset to the built-in preset. */
export function TrashInstances({ form, onChange }) {
  return (
    <div className="w-full space-y-3">
      {APPS.map(([app, label]) => (
        <div key={app} className="space-y-1.5">
          <div className="flex items-center gap-2">
            <label htmlFor={`trash-${app}`} className="text-fg-muted">{label}</label>
            <button className="btn ml-auto px-2 text-xs" onClick={() => onChange({ ...form, preset: { ...form.preset, [app]: true } })}
              disabled={form.preset[app]}>
              <RotateCcw size={12} /> FLINCH preset
            </button>
          </div>
          {form.preset[app]
            ? <p className="text-[12px] text-fg-faint">The built-in preset replaces this app’s config when you save.</p>
            : (
              <textarea id={`trash-${app}`} spellCheck={false} rows={10}
                className="input w-full font-mono text-[12px] leading-snug"
                value={form[app]} onChange={(e) => onChange({ ...form, [app]: e.target.value })} />
            )}
        </div>
      ))}
      <div className="space-y-1.5">
        <label htmlFor="trash-extra" className="text-fg-muted">Extra instances</label>
        <p className="text-[12px] text-fg-faint">
          One config per extra instance, keyed <code>radarr@4k</code> or <code>sonarr@anime</code>, in the same fields. An extra instance without an entry is not synced: a 4K or anime library rarely wants the HD preset.
        </p>
        <textarea id="trash-extra" spellCheck={false} rows={6}
          className="input w-full font-mono text-[12px] leading-snug"
          value={form.extra} onChange={(e) => onChange({ ...form, extra: e.target.value })} />
      </div>
    </div>
  );
}
