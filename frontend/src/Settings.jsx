import React, { useEffect, useState } from 'react';
import { Check, Loader2, Plus, Save, X } from 'lucide-react';
import { Card, JustWatch, SectionTitle, every } from './ui.jsx';
import { loadSettings, saveSettings } from './api.js';
import { Explain } from './Explain.jsx';
import { NotifyChannels, NotifyTest } from './NotifySettings.jsx';
import { PathMap, TorrentClients, torrentSummary, torrentsPayload } from './TorrentSettings.jsx';
import { RulesEditor, rulesPayload } from './RuleSettings.jsx';
import { TrashInstances, trashForm, trashPayload } from './TrashSettings.jsx';
import { WatchSources, watchSourcesForm, watchSourcesPayload, watchSourcesSummary } from './WatchSourceSettings.jsx';
import { HouseholdRecipients, WEEKDAYS, householdForm, householdPayload } from './HouseholdSettings.jsx';
import { InflowApprovals, inflowActionsForm, inflowActionsSummary } from './InflowActions.jsx';
import { InUse, InstanceList, instancesForm, instancesPayload } from './InstanceSettings.jsx';

// Settings are written to `state/settings.json` on the shared volume and
// re-read by the daemon each cycle, so changes apply on the next run.

const GIB = 2 ** 30;
const PERCENT = { scale: 100, ok: (v) => v > 0 && v <= 100 };
const BYTES = { scale: 1 / GIB, digits: 2 };
const inRange = (lo, hi) => ({ ok: (v) => v >= lo && v <= hi });

/** The Matryoshka sizes EmbeddingGemma 2 keeps meaningful. */
const DIMENSIONS = [128, 256, 512, 768];
const GEMMA = 'embeddinggemma-2';
/** The encoders the daemon can run, with what each costs. */
const EMBEDDING_MODELS = [
  [GEMMA, 'EmbeddingGemma 2 — best quality, ~580 MB weights, ~650 MB peak'],
  ['bge-small-en-v1.5', 'bge-small — ~130 MB'],
  ['all-minilm-l6-v2', 'MiniLM — ~90 MB'],
];
const halfLifeOk = (v) => v === 0 || (v >= 7 && v <= 3650);

/**
 * Every numeric field as `[group, key, label, spec]`; `group` null is top
 * level. The form shows `daemon value × scale`; `ok` checks the form value;
 * `optional` fields may be blank (sent as null).
 */
const NUMERIC = [
  [null, 'interval_s', 'Scan interval', {}],
  [null, 'grace_runs', 'Grace runs', {}],
  [null, 'max_items', 'Items per run', {}],
  [null, 'max_gib', 'Size per run', {}],
  ['capacity', 'target_utilization', 'Target', PERCENT],
  ['capacity', 'emergency_utilization', 'Emergency', PERCENT],
  ['capacity', 'sliding_window_days', 'Window (1–365 days)', inRange(1, 365)],
  ['capacity', 'ewma_alpha', 'EWMA alpha (above 0, at most 1)', { ok: (v) => v > 0 && v <= 1 }],
  ['capacity', 'headroom_buffer_bytes', 'Headroom', { ...BYTES, ok: (v) => v >= 0 }],
  ['capacity', 'max_capacity_bytes', 'Max capacity', { ...BYTES, ok: (v) => v > 0, optional: true }],
  ['planner', 'quantum_mb', 'Quantum (1–10240 MiB)', inRange(1, 10240)],
  ['planner', 'grace_period_days', 'Grace period (0–3650 days)', inRange(0, 3650)],
  ['embedding', 'dimensions', 'Embedding dimensions', { ok: (v) => DIMENSIONS.includes(v) }],
  ['embedding', 'daily_budget', 'Embedding budget (1–20000 titles)', inRange(1, 20000)],
  ['taste', 'half_life_days', 'Taste half-life (0, or 7–3650 days)', { ok: halfLifeOk }],
  ['notify', 'digest_hour_utc', 'Digest hour (0–23 UTC)', inRange(0, 23)],
  ['notify', 'max_per_hour', 'Notifications per hour (1–120)', inRange(1, 120)],
  ['quality_actions', 'max_per_day', 'Quality moves per day (1–50)', inRange(1, 50)],
  ['upgrade_search', 'max_per_day', 'Upgrade searches per day (1–50)', inRange(1, 50)],
  ['upgrade_guard', 'max_grabs_per_item_30d', 'Grabs per item (1–100)', inRange(1, 100)],
  ['torrents', 'min_ratio', 'Minimum seed ratio (0–100)', inRange(0, 100)],
  ['torrents', 'min_seed_days', 'Minimum seed days (0–3650)', inRange(0, 3650)],
  ['torrents', 'prefer_after_ratio', 'Desired seed ratio (0–100)', inRange(0, 100)],
  ['native', 'leaving_soon_days', 'Leaving Soon window (1–90 days)', inRange(1, 90)],
  ['native', 'max_deletes_per_run', 'Deletes per run (1–500)', inRange(1, 500)],
  ['dupes', 'max_per_run', 'Duplicate removals per run (1–50)', inRange(1, 50)],
  ['dupes', 'unowned_min_gib', 'Unowned folder size (1–10000 GiB)', inRange(1, 10000)],
  ['dupes', 'unowned_max_folders', 'Unowned folders measured (0–200)', inRange(0, 200)],
  ['household', 'keep_days', 'Requested keep (1–365 days)', inRange(1, 365)],
  ['household', 'link_days', 'Link lifetime (1–90 days)', inRange(1, 90)],
  ['archive', 'max_moves_per_run', 'Archive moves per run (1–50)', inRange(1, 50)],
];

const groupOf = (obj, group) => (group ? obj[group] : obj);

/** The archive tier's last run, in one line. */
function archiveSummary(archive) {
  if (!archive) return '—';
  const gib = (bytes) => (bytes / GIB).toFixed(1);
  const rooms = archive.destinations.map((d) => `${d.root}: ${gib(d.headroom_bytes)} GiB free`).join(', ');
  const moved = archive.moved.length ? `, ${archive.moved.length} ${archive.dry_run ? 'would move' : 'moved'}` : '';
  const failed = archive.failed.length ? `, ${archive.failed.length} failed` : '';
  const unresolved = archive.unresolved.length ? ` · ${archive.unresolved.join('; ')}` : '';
  return `${archive.planned} planned (${gib(archive.planned_bytes)} GiB)${moved}${failed}${rooms ? ` · ${rooms}` : ''}${unresolved}`;
}

/** Settings as the form shows them: scaled numbers, user weights as editable rows. */
function toForm(settings) {
  const form = { ...settings, capacity: { ...settings.capacity }, planner: { ...settings.planner }, embedding: { ...settings.embedding }, notify: { ...settings.notify } };
  form.taste = { half_life_days: 0, ...settings.taste };
  form.dupes = { ...settings.dupes };
  form.archive = { ...settings.archive };
  form.household = { ...settings.household };
  form.notify.household = householdForm(settings.notify?.household);
  Object.assign(form, { quality_actions: { ...settings.quality_actions }, upgrade_search: { ...settings.upgrade_search }, upgrade_guard: { ...settings.upgrade_guard }, native: { ...settings.native } });
  const torrents = settings.torrents || {};
  form.torrents = {
    ...torrents,
    clients: (torrents.clients || []).map((client) => ({ ...client, username: client.username || '', password_env: client.password_env || '' })),
    path_map: torrents.path_map || [],
  };
  form.trash = trashForm(settings.trash);
  form.watch_sources = watchSourcesForm(settings.watch_sources);
  form.ignore_viewers = (settings.ignore_viewers || []).join(', ');
  form.inflow_actions = inflowActionsForm(settings.inflow_actions);
  form.instances = instancesForm(settings.instances);
  const streaming = settings.streaming || {};
  form.streaming = { ...streaming, region: streaming.region || '', tmdb_key_env: streaming.tmdb_key_env ?? 'TMDB_API_KEY', provider_ids: (streaming.provider_ids || []).join(', ') };
  for (const [group, key, , { scale = 1, digits = 2 }] of NUMERIC) {
    const target = groupOf(form, group);
    const value = target[key];
    target[key] = value == null ? '' : Math.round(value * scale * 10 ** digits) / 10 ** digits;
  }
  form.planner.user_weights = Object.entries(settings.planner?.user_weights || {}).map(([name, weight]) => ({ name, weight }));
  return form;
}

/** A typed number, or NaN for blank or junk. */
const parse = (raw) => (typeof raw === 'string' && raw.trim() === '' ? NaN : Number(raw));

/**
 * Inputs hold whatever the user typed; numbers are coerced once, here, so the
 * PUT body never carries a string for a numeric field. The target must sit
 * below the emergency mark. User weight names are unique (case-insensitive)
 * and weights at least 0. The token field starts blank (the server never sends
 * it back); blank keeps the saved token while the URL is unchanged.
 */
function toPayload(form) {
  const payload = { ...form, capacity: { ...form.capacity }, planner: { ...form.planner }, embedding: { ...form.embedding }, notify: { ...form.notify } };
  payload.taste = { ...form.taste };
  payload.dupes = { ...form.dupes };
  payload.household = { ...form.household };
  Object.assign(payload, { quality_actions: { ...form.quality_actions }, upgrade_search: { ...form.upgrade_search }, upgrade_guard: { ...form.upgrade_guard }, native: { ...form.native } });
  payload.torrents = torrentsPayload(form.torrents);
  payload.watch_sources = watchSourcesPayload(form.watch_sources);
  delete payload.plex_token_set;
  delete payload.jellyfin_token_set;
  const invalid = [];
  for (const [group, key, label, { scale = 1, ok = (v) => v >= 0, optional }] of NUMERIC) {
    const raw = groupOf(form, group)[key];
    const target = groupOf(payload, group);
    if (optional && String(raw).trim() === '') { target[key] = null; continue; }
    const value = parse(raw);
    if (!Number.isFinite(value) || !ok(value)) invalid.push(label);
    else target[key] = scale === 1 ? value : scale < 1 ? Math.round(value / scale) : value / scale;
  }
  if (!invalid.length && payload.capacity.target_utilization >= payload.capacity.emergency_utilization) {
    invalid.push('Target (below Emergency)');
  }
  const weights = {};
  for (const { name, weight } of form.planner.user_weights) {
    const key = name.trim();
    const value = parse(weight);
    const dup = Object.keys(weights).some((k) => k.toLowerCase() === key.toLowerCase());
    if (!key || dup || !Number.isFinite(value) || value < 0) invalid.push(`Weight for “${key || 'blank name'}”`);
    else weights[key] = value;
  }
  payload.planner.user_weights = weights;
  payload.notify.ui_url = String(form.notify.ui_url || '').trim();
  payload.notify.channels = form.notify.channels.map((channel) => ({
    ...channel, name: channel.name.trim(), url: channel.url.trim(), url_env: channel.url_env.trim(), token_env: channel.token_env.trim(),
  }));
  const household = householdPayload(form.notify.household);
  payload.notify.household = household.household;
  invalid.push(...household.invalid);
  payload.rules = rulesPayload(form.rules || []);
  if (payload.rules.some((rule) => !rule.name)) invalid.push('Rule names');
  payload.ignore_viewers = String(form.ignore_viewers || '').split(',').map((name) => name.trim()).filter(Boolean);
  if (payload.ignore_viewers.length > 50 || payload.ignore_viewers.some((name) => name.length > 100)) invalid.push('Ignored viewers (up to 50 names)');
  const trash = trashPayload(form.trash);
  payload.trash = trash.trash;
  invalid.push(...trash.invalid);
  const instances = instancesPayload(form.instances);
  payload.instances = instances.instances;
  invalid.push(...instances.invalid);
  const ids = String(form.streaming.provider_ids || '').split(',').map((id) => id.trim()).filter(Boolean).map(Number);
  if (ids.some((id) => !Number.isInteger(id) || id <= 0) || ids.length > 64) invalid.push('Streaming providers (TMDB ids)');
  payload.streaming = { ...form.streaming, region: String(form.streaming.region || '').trim().toUpperCase(), tmdb_key_env: String(form.streaming.tmdb_key_env || '').trim(), provider_ids: ids };
  if (payload.streaming.enabled && (!/^[A-Z]{2}$/.test(payload.streaming.region) || !ids.length || !payload.streaming.tmdb_key_env)) {
    invalid.push('Streaming (region, providers and key variable)');
  }
  return { payload, invalid };
}

export default function Settings({ status }) {
  const [form, setForm] = useState(null);
  const [loadError, setLoadError] = useState('');
  const [state, setState] = useState('idle'); // idle | dirty | saving | saved | error
  const [error, setError] = useState('');
  // The rules as saved, and as last previewed: a rule change saves only once
  // the very list being saved was previewed.
  const [savedRules, setSavedRules] = useState('[]');
  const [previewedRules, setPreviewedRules] = useState(null);

  const adopt = (settings) => {
    setForm(toForm(settings));
    setSavedRules(JSON.stringify(rulesPayload(settings.rules || [])));
  };

  useEffect(() => {
    loadSettings().then(adopt).catch((err) => setLoadError(String(err.message || err)));
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  if (loadError) {
    return (
      <Card className="mt-6 p-5 text-[13px]">
        <p className="text-state-bad">settings.json could not be read: {loadError}</p>
        <p className="mt-1 text-fg-muted">The daemon keeps running on its last good settings. Fix or delete the file on the state volume, then reload.</p>
      </Card>
    );
  }
  if (!form) return <Card className="mt-6 p-5 text-[13px] text-fg-muted">Loading settings…</Card>;

  const update = (next) => { setForm(next); setState('dirty'); };
  const valueOf = (event) => (event.target.type === 'checkbox' ? event.target.checked : event.target.value);
  const set = (key) => (event) => update({ ...form, [key]: valueOf(event) });
  const setIn = (group, key) => (event) => update({ ...form, [group]: { ...form[group], [key]: valueOf(event) } });
  const gemma = (form.embedding.model || GEMMA) === GEMMA;
  // Posters are EmbeddingGemma 2's alone: another model switches them off.
  const setModel = (event) => {
    const model = event.target.value;
    update({ ...form, embedding: { ...form.embedding, model, posters: model === GEMMA && !!form.embedding.posters } });
  };
  const setHousehold = (key) => (event) => update({ ...form, notify: { ...form.notify, household: { ...form.notify.household, [key]: valueOf(event) } } });
  const setPcd = (key) => (event) => update({ ...form, trash: { ...form.trash, pcd: { ...form.trash.pcd, [key]: valueOf(event) } } });
  const weights = form.planner.user_weights;
  const setWeights = (rows) => update({ ...form, planner: { ...form.planner, user_weights: rows } });
  const setWeight = (index, field) => (event) => setWeights(weights.map((row, i) => (i === index ? { ...row, [field]: event.target.value } : row)));

  const save = async () => {
    const { payload, invalid } = toPayload(form);
    if (invalid.length) {
      setError(`Check: ${invalid.join(', ')}.`);
      setState('error');
      return;
    }
    const rules = JSON.stringify(payload.rules);
    if (rules !== savedRules && rules !== previewedRules) {
      setError('Preview the rule changes (Rules → Preview changes) before saving.');
      setState('error');
      return;
    }
    setState('saving');
    try {
      await saveSettings(payload);
      adopt(await loadSettings());
      setState('saved');
    } catch (err) {
      setError(String(err.message || err));
      setState('error');
    }
  };

  const num = (key, props) => ({ id: key, value: form[key], onChange: set(key), ...props });
  const numIn = (group, key, props) => ({ id: `${group}.${key}`, value: form[group][key], onChange: setIn(group, key), ...props });
  // What holds never-played reclaim whatever its switch says: incomplete watch
  // evidence the daemon reported, or a Leaving Soon title this form leaves blank.
  const neverPlayedHeld = status?.never_played_hold === 'incomplete_evidence' ? 'the watch evidence is complete'
    : !String(form.collection_leaving || '').trim() ? 'a Leaving Soon collection is named' : null;

  return (
    <div className="mt-6 max-w-3xl text-[13px]">
      <Card className="px-4 pt-4 sm:px-5 sm:pt-5">
        <Section title="Storage" term="projection">
          <Row label="Thresholds" htmlFor="capacity.target_utilization"
            help="Plans keep each disk under the target. At the emergency mark the plan is greedy. Target must be below emergency.">
            <NumberField {...numIn('capacity', 'target_utilization', { min: 1, max: 99, step: 1 })} unit="% target" />
            <NumberField {...numIn('capacity', 'emergency_utilization', { min: 1, max: 100, step: 1 })} unit="% emergency" />
          </Row>
          <Row label="Projection" htmlFor="capacity.sliding_window_days" help="How far ahead to project, and how fast the daily download average reacts (higher alpha = faster).">
            <NumberField {...numIn('capacity', 'sliding_window_days', { min: 1, max: 365, step: 1 })} unit="days" />
            <NumberField {...numIn('capacity', 'ewma_alpha', { min: 0.01, max: 1, step: 0.05 })} unit="alpha" />
          </Row>
          <Row label="Headroom" htmlFor="capacity.headroom_buffer_bytes" help="Extra space freed on top of what the projection needs.">
            <NumberField {...numIn('capacity', 'headroom_buffer_bytes', { min: 0, step: 1 })} unit="GiB" />
          </Row>
          <Row label="Max capacity" htmlFor="capacity.max_capacity_bytes" help="Treat each disk as at most this big. Blank uses the measured size.">
            <NumberField {...numIn('capacity', 'max_capacity_bytes', { min: 1, step: 1 })} unit="GiB" />
          </Row>
        </Section>

        <Section title="Planner" term="plan">
          <Row label="Dry run" htmlFor="planner.dry_run" term="dry_run" help="Items are handed over (to Maintainerr, or deleted by the native executor) only when this is off.">
            <Toggle id="planner.dry_run" checked={!!form.planner.dry_run} onChange={setIn('planner', 'dry_run')}>
              Plan only
            </Toggle>
            {!form.planner.dry_run && <span className="text-state-bad">Live: deletions are scheduled</span>}
          </Row>
          <Row label="Grace period" htmlFor="planner.grace_period_days" term="grace_period" help="Newer items are never picked.">
            <NumberField {...numIn('planner', 'grace_period_days', { min: 0, max: 3650, step: 1 })} unit="days" />
          </Row>
          <Row label="Quantum" htmlFor="planner.quantum_mb" help="Size step the solver rounds to. Larger is faster, coarser.">
            <NumberField {...numIn('planner', 'quantum_mb', { min: 1, max: 10240, step: 1 })} unit="MiB" />
          </Row>
          <Row label="Never played" htmlFor="unwatched_reclaim_enabled" term="never_played"
            help={neverPlayedHeld && form.unwatched_reclaim_enabled ? `Held until ${neverPlayedHeld}.` : 'Off keeps items nobody finished.'}>
            <Toggle id="unwatched_reclaim_enabled" checked={!!form.unwatched_reclaim_enabled} onChange={set('unwatched_reclaim_enabled')}>
              Allow picking items nobody finished
            </Toggle>
          </Row>
          <Row label="User weights" htmlFor="weight-name-0" term="user_weights" help="Seerr display name. Unlisted users weigh 1.">
            <div className="w-full space-y-2">
              {weights.map((row, i) => (
                <div key={i} className="flex items-center gap-2">
                  <input id={`weight-name-${i}`} aria-label="Seerr user" className="input min-h-[40px] w-full sm:min-h-0 sm:w-48"
                    value={row.name} onChange={setWeight(i, 'name')} placeholder="Display name" />
                  <NumberField aria-label={`Weight for ${row.name}`} value={row.weight} onChange={setWeight(i, 'weight')} min={0} step={0.1} />
                  <button className="btn px-2" aria-label={`Remove ${row.name || 'row'}`} onClick={() => setWeights(weights.filter((_, j) => j !== i))}>
                    <X size={13} />
                  </button>
                </div>
              ))}
              <button className="btn px-2.5 text-xs" onClick={() => setWeights([...weights, { name: '', weight: 1 }])}>
                <Plus size={13} /> Add user
              </button>
            </div>
          </Row>
        </Section>

        <Section title="Schedule">
          <Row label="Scan interval" htmlFor="interval_s" help="Time between runs.">
            <NumberField {...num('interval_s', { min: 300, step: 300 })} unit="seconds" />
            {every(form.interval_s) && <span className="text-fg-faint">{every(form.interval_s)}</span>}
          </Row>
          <Row label="Grace runs" htmlFor="grace_runs" term="grace_runs" help="Runs in a row an item must stay picked before hand-off.">
            <NumberField {...num('grace_runs', { min: 1, max: 20 })} unit="runs" />
          </Row>
          <Row label="Per-run cap" htmlFor="max_items" help="Most one run may hand off; the first limit hit applies.">
            <NumberField {...num('max_items', { min: 0 })} unit="items" />
            <NumberField {...num('max_gib', { min: 0 })} unit="GiB" />
          </Row>
        </Section>

        <Section title="Rules">
          <Row label="Keep tag" htmlFor="keep_tag" term="pinned" help="*arr tag, Plex label or collection that pins an item. Blank disables it.">
            <TextField id="keep_tag" placeholder="flinch-keep" autoComplete="off" spellCheck={false} value={form.keep_tag} onChange={set('keep_tag')} />
          </Row>
          <Row label="Rules" term="rules" help="Hard keeps and forced evictions, never a change to regret; keep beats evict. Preview a change before saving it.">
            <RulesEditor rules={form.rules || []} onChange={(rules) => update({ ...form, rules })} onPreviewed={setPreviewedRules} lastRun={status?.rules} />
          </Row>
          <Row label="Ignored viewers" htmlFor="ignore_viewers" term="ignore_viewers"
            help="Names whose plays count as no play: a guest, a kid's profile, your own test plays. Matched per source (Plex account, Tautulli user, Jellyfin/Emby user, Tracearr username, Trakt source name). Health stays as read, so an item only they played reads as never played only where the record is complete. Plex's own item state of the token's account cannot be split and still counts.">
            <TextField id="ignore_viewers" placeholder="guest, kids" autoComplete="off" spellCheck={false} value={form.ignore_viewers} onChange={set('ignore_viewers')} />
          </Row>
          <Row label="Inflow actions" htmlFor="inflow_actions.enabled" term="inflow_actions" help={inflowActionsSummary(status)}>
            <Toggle id="inflow_actions.enabled" checked={!!form.inflow_actions.enabled}
              onChange={(e) => update({ ...form, inflow_actions: { ...form.inflow_actions, enabled: e.target.checked } })}>
              Act on the approved advice below while a disk is over its target
            </Toggle>
            <InflowApprovals config={form.inflow_actions} onChange={(inflow_actions) => update({ ...form, inflow_actions })}
              suggestions={status?.inflow} status={status?.inflow_actions} />
          </Row>
        </Section>

        <Section title="Executor" term="executor">
          <Row label="Deletes" htmlFor="executor"
            help="Maintainerr deletes what FLINCH hands to its collections. Native: FLINCH announces in Plex and deletes through Radarr and Sonarr itself, and needs no Maintainerr.">
            <select id="executor" value={form.executor || 'maintainerr'} onChange={set('executor')}
              className="min-h-[40px] rounded-md border border-line bg-ink-900 px-2 text-[13px] text-fg focus:border-fg-faint focus:outline-none sm:min-h-0">
              <option value="maintainerr">Maintainerr</option>
              <option value="native">FLINCH (native)</option>
            </select>
          </Row>
          {form.executor === 'native' && (
            <>
              <Row label="Leaving Soon" htmlFor="native.leaving_soon_days" term="leaving_soon"
                help="Items nobody finished wait this long on the Leaving Soon shelf (the collection named under Leaving Soon below) before they are deleted. Without the shelf's server they are held.">
                <NumberField {...numIn('native', 'leaving_soon_days', { min: 1, max: 90, step: 1 })} unit="days" />
              </Row>
              <Row label="Shelf on" htmlFor="native.leaving_soon_server" help="Plex keeps one collection per library, promoted to home. Jellyfin/Emby (the server under Jellyfin) keeps one collection for the whole server and has no home promotion: name it to sort first. Switching moves every item back off the shelf; its window restarts.">
                <select id="native.leaving_soon_server" value={form.native.leaving_soon_server || 'plex'} onChange={setIn('native', 'leaving_soon_server')}
                  className="min-h-[40px] rounded-md border border-line bg-ink-900 px-2 text-[13px] text-fg focus:border-fg-faint focus:outline-none sm:min-h-0">
                  <option value="plex">Plex</option>
                  <option value="jellyfin">Jellyfin / Emby</option>
                </select>
              </Row>
              <Row label="Movies" htmlFor="native.delete_mode" help="Seasons always keep the show: their episode files go and the season is unmonitored.">
                <select id="native.delete_mode" value={form.native.delete_mode || 'file_and_unmonitor'} onChange={setIn('native', 'delete_mode')}
                  className="min-h-[40px] rounded-md border border-line bg-ink-900 px-2 text-[13px] text-fg focus:border-fg-faint focus:outline-none sm:min-h-0">
                  <option value="file_and_unmonitor">Delete the file and unmonitor</option>
                  <option value="remove_entry">Remove the movie from Radarr</option>
                </select>
              </Row>
              {form.native.delete_mode === 'remove_entry' && (
                <Row label="Import exclusion" htmlFor="native.add_import_exclusion" help="So an import list does not add a removed movie straight back.">
                  <Toggle id="native.add_import_exclusion" checked={!!form.native.add_import_exclusion} onChange={setIn('native', 'add_import_exclusion')}>
                    Add Radarr's import exclusion
                  </Toggle>
                </Row>
              )}
              <Row label="Seerr" htmlFor="native.seerr_cleanup" help="After a delete, clear the title's Seerr request so it can be requested again. Needs SEERR_URL and SEERR_API_KEY.">
                <Toggle id="native.seerr_cleanup" checked={!!form.native.seerr_cleanup} onChange={setIn('native', 'seerr_cleanup')}>
                  Clear Seerr requests of deleted items
                </Toggle>
              </Row>
              <Row label="Deletes per run" htmlFor="native.max_deletes_per_run" help="Every delete in one run counts, expired Leaving Soon windows included. The per-run cap above bounds new deletes and announcements too.">
                <NumberField {...numIn('native', 'max_deletes_per_run', { min: 1, max: 500, step: 1 })} unit="deletes" />
              </Row>
              <Row label="Poster badges" htmlFor="native.poster_overlays" help="Draw “Leaves Oct 23” on the Plex poster of each Leaving Soon item; the original poster is put back before it leaves the shelf. Leave off if Kometa manages your overlays: both rewrite the same poster.">
                <Toggle id="native.poster_overlays" checked={!!form.native.poster_overlays} onChange={setIn('native', 'poster_overlays')}>
                  Badge Leaving Soon posters with their date
                </Toggle>
              </Row>
            </>
          )}
        </Section>

        <Section title="Collections">
          <Row label="Movies" htmlFor="collection_movie" help="Maintainerr's delete collection for movies.">
            <TextField id="collection_movie" value={form.collection_movie} onChange={set('collection_movie')} />
          </Row>
          <Row label="Seasons" htmlFor="collection_season" help="Maintainerr's delete collection for seasons.">
            <TextField id="collection_season" value={form.collection_season} onChange={set('collection_season')} />
          </Row>
          <Row label="Leaving Soon" htmlFor="collection_leaving" term="leaving_soon"
            help="Announces items nobody finished before deletion: a Maintainerr collection, or with the native executor a Plex collection FLINCH keeps in each library. Blank keeps never-played reclaim off.">
            <TextField id="collection_leaving" value={form.collection_leaving} onChange={set('collection_leaving')} />
          </Row>
        </Section>

        <Section title="Instances">
          <Row label="Extra instances" htmlFor="instances-app-0"
            help="Radarr and Sonarr beyond the default pair, such as a 4K Radarr or an anime Sonarr; up to 8 per app. The name (a-z, 0-9, _) is part of each item's id: renaming one makes its items new to FLINCH. Name the flinch-arrd environment variable holding the API key; the key never enters settings.json. Numbered variables add instances too: RADARR_<N>_URL and RADARR_<N>_API_KEY, optional RADARR_<N>_NAME, _ARCHIVE_ROOT, _COMPACT_PROFILE, _PUBLIC_URL, and the same for SONARR_<N>_. Every instance must be readable or the run stops; a TRaSH sync needs its own entry under Quality profiles; import-list toggles cover the default pair only.">
            <InstanceList instances={form.instances} onChange={(instances) => update({ ...form, instances })} />
          </Row>
          <Row label="In use" help="What the daemon talked to on its last run, defaults first. An instance that could not be set up is left out and named in the daemon log.">
            <InUse status={status} />
          </Row>
        </Section>

        <Section title="Plex">
          <Row label="URL" htmlFor="plex_url" help="Watch state source. Without URL and token every item is kept.">
            <TextField id="plex_url" placeholder="https://plex:32400" value={form.plex_url} onChange={set('plex_url')} />
          </Row>
          <Row label="Token" htmlFor="plex_token" help="Never sent back to the browser. Blank keeps the saved one; a new URL needs it again.">
            <TextField id="plex_token" type="password" autoComplete="off" placeholder={form.plex_token_set ? 'Saved' : ''}
              value={form.plex_token} onChange={set('plex_token')} />
          </Row>
        </Section>

        <Section title="Jellyfin / Emby">
          <Row label="Server" htmlFor="jellyfin.kind" help="Read for every user's played state. Leaving Soon and collections stay in Plex.">
            <select id="jellyfin.kind" value={form.jellyfin?.kind || 'jellyfin'} onChange={setIn('jellyfin', 'kind')}
              className="min-h-[40px] rounded-md border border-line bg-ink-900 px-2 text-[13px] text-fg focus:border-fg-faint focus:outline-none sm:min-h-0">
              <option value="jellyfin">Jellyfin</option>
              <option value="emby">Emby</option>
            </select>
          </Row>
          <Row label="URL" htmlFor="jellyfin.url" help="Blank turns it off. Once set, it must be read in full every cycle or never-played reclaim holds.">
            <TextField id="jellyfin.url" placeholder="http://jellyfin:8096" value={form.jellyfin?.url || ''} onChange={setIn('jellyfin', 'url')} />
          </Row>
          <Row label="API key" htmlFor="jellyfin.token" help="An admin API key. Never sent back to the browser. Blank keeps the saved one; a new URL needs it again.">
            <TextField id="jellyfin.token" type="password" autoComplete="off" placeholder={form.jellyfin_token_set ? 'Saved' : ''}
              value={form.jellyfin?.token || ''} onChange={setIn('jellyfin', 'token')} />
          </Row>
          <Row label="Key variable" htmlFor="jellyfin.api_key_env" help="Or the name of an environment variable holding the key, e.g. FLINCH_JELLYFIN_KEY.">
            <TextField id="jellyfin.api_key_env" value={form.jellyfin?.api_key_env || ''} onChange={setIn('jellyfin', 'api_key_env')} />
          </Row>
        </Section>

        <Section title="Watch sources" term="watch_sources">
          <Row label="Tracearr / Trakt" htmlFor="watch-sources-kind-0"
            help="Play logs read as watch evidence, joined by TMDB/TVDB/IMDb id. Tracearr is read for every user; Trakt once per household member's account. Once set, each must be read in full every cycle or never-played reclaim holds.">
            <WatchSources sources={form.watch_sources} onChange={(watch_sources) => update({ ...form, watch_sources })} />
          </Row>
          <Row label="Status" help="Only a complete Tracearr read can say an item was never played, and only for items that arrived after its record began.">
            <span className="text-fg-muted">{watchSourcesSummary(status)}</span>
          </Row>
        </Section>

        <Section title="Taste embeddings">
          <Row label="Embed titles" htmlFor="embedding.enabled"
            help="The chosen model runs inside the daemon on its CPU. The first run downloads its weights to the state volume. Off keeps the vectors already made.">
            <Toggle id="embedding.enabled" checked={!!form.embedding.enabled} onChange={setIn('embedding', 'enabled')}>
              Embed new and changed titles
            </Toggle>
          </Row>
          <Row label="Model" htmlFor="embedding.model"
            help="EmbeddingGemma 2 describes titles best. bge-small (~200 MB resident) and MiniLM (~110 MB) suit a small host, at some cost in nuance. Switching embeds every title again.">
            <select id="embedding.model" value={form.embedding.model || GEMMA} onChange={setModel}
              className="min-h-[40px] rounded-md border border-line bg-ink-900 px-2 text-[13px] text-fg focus:border-fg-faint focus:outline-none sm:min-h-0">
              {EMBEDDING_MODELS.map(([id, label]) => <option key={id} value={id}>{label}</option>)}
            </select>
          </Row>
          {gemma && (
            <Row label="Dimensions" htmlFor="embedding.dimensions" help="Vector length kept from the model's 768. Smaller is lighter, coarser; changing it embeds every title again.">
              <select id="embedding.dimensions" value={form.embedding.dimensions} onChange={setIn('embedding', 'dimensions')}
                className="min-h-[40px] rounded-md border border-line bg-ink-900 px-2 text-[13px] text-fg focus:border-fg-faint focus:outline-none sm:min-h-0">
                {DIMENSIONS.map((d) => <option key={d} value={d}>{d}</option>)}
              </select>
            </Row>
          )}
          <Row label="Daily budget" htmlFor="embedding.daily_budget" help="Most titles embedded per day (UTC), at most two minutes of CPU per run; a large library fills in over several days.">
            <NumberField {...numIn('embedding', 'daily_budget', { min: 1, max: 20000, step: 50 })} unit="titles" />
          </Row>
          <Row label="Posters" htmlFor="embedding.posters"
            help="EmbeddingGemma 2 only. Describe each title by its poster too, through the model's vision tower: about 335 MB more weights, and several seconds of CPU per title. Posters come from the TMDB/TheTVDB address Radarr and Sonarr list, fetched without any credential. Switching it on or off embeds every title again.">
            <Toggle id="embedding.posters" checked={!!form.embedding.posters} onChange={setIn('embedding', 'posters')} disabled={!gemma}>
              Include the poster in each title's vector
            </Toggle>
          </Row>
          <Row label="Taste half-life" htmlFor="taste.half_life_days"
            help="Weigh what the household played recently above what it played long ago: an outcome counts half after this many days. 0 counts every outcome alike. The next daily fit uses it.">
            <NumberField {...numIn('taste', 'half_life_days', { min: 0, max: 3650, step: 30 })} unit="days" />
          </Row>
        </Section>

        <Section title="Torrents" term="seeding">
          <Row label="Clients" htmlFor="torrents-kind-0"
            help="qBittorrent or Transmission, read every run and never written to except to remove a torrent after the native executor deleted its item. Name the flinch-arrd environment variable holding the password; the password never enters settings.json.">
            <TorrentClients clients={form.torrents.clients} onChange={(clients) => update({ ...form, torrents: { ...form.torrents, clients } })} />
          </Row>
          <Row label="Seed goals" htmlFor="torrents.respect_seed_goals"
            help="Keep an item while one of its torrents has neither reached its client's share limit nor FLINCH's minimum below.">
            <Toggle id="torrents.respect_seed_goals" checked={!!form.torrents.respect_seed_goals} onChange={setIn('torrents', 'respect_seed_goals')}>
              Keep items still seeding toward their goal
            </Toggle>
          </Row>
          <Row label="Minimum" htmlFor="torrents.min_ratio" help="A torrent at this ratio, or seeded this many days, has met FLINCH's goal. 0 and 0: only the client's own limits count.">
            <NumberField {...numIn('torrents', 'min_ratio', { min: 0, max: 100, step: 0.1 })} unit="ratio" />
            <NumberField {...numIn('torrents', 'min_seed_days', { min: 0, max: 3650, step: 1 })} unit="days" />
          </Row>
          <Row label="Desired ratio" htmlFor="torrents.prefer_after_ratio" term="desired_ratio"
            help="Softer than the goal: an item whose torrent is below this ratio is not kept, only drawn on last, once nothing else on its disk fills the target. A rule that keeps or forces it still decides. 0 is off.">
            <NumberField {...numIn('torrents', 'prefer_after_ratio', { min: 0, max: 100, step: 0.1 })} unit="ratio" />
          </Row>
          <Row label="Remove after delete" htmlFor="torrents.remove_after_delete"
            help="With the native executor: after an item is deleted, remove its torrents and their data once they met their goal, so hardlinked bytes are freed. A torrent shared with an item that stays is kept.">
            <Toggle id="torrents.remove_after_delete" checked={!!form.torrents.remove_after_delete} onChange={setIn('torrents', 'remove_after_delete')}>
              Remove the torrents of deleted items
            </Toggle>
          </Row>
          <Row label="Path map" htmlFor="torrents-from-0"
            help="Where the clients' and the *arrs' paths are mounted in flinch-arrd, for the hardlink check. Without it, links that cannot be read keep the item.">
            <PathMap rows={form.torrents.path_map} onChange={(path_map) => update({ ...form, torrents: { ...form.torrents, path_map } })} />
          </Row>
          <Row label="Status" help={torrentSummary(status)}>
            <span className="text-fg-muted">{status?.torrents ? `${status.torrents.by_history + status.torrents.by_path} items held by torrents` : '—'}</span>
          </Row>
        </Section>

        <Section title="Streaming" term="streaming">
          <Row label="Discount" htmlFor="streaming.enabled"
            help="A title that streams on a service you subscribe to, in your region, is cheaper to lose: its re-download cost drops toward the floor, never below. Unknown availability changes nothing.">
            <Toggle id="streaming.enabled" checked={!!form.streaming.enabled} onChange={setIn('streaming', 'enabled')}>
              Use streaming availability
            </Toggle>
          </Row>
          <Row label="Region" htmlFor="streaming.region" help="Two-letter country code, e.g. DE or US.">
            <TextField id="streaming.region" spellCheck={false} autoComplete="off" maxLength={2} value={form.streaming.region} onChange={setIn('streaming', 'region')} />
          </Row>
          <Row label="Your services" htmlFor="streaming.provider_ids" help="TMDB provider ids, comma-separated (8 = Netflix, 337 = Disney Plus, 9 = Prime Video).">
            <TextField id="streaming.provider_ids" spellCheck={false} autoComplete="off" placeholder="8, 337" value={form.streaming.provider_ids} onChange={setIn('streaming', 'provider_ids')} />
          </Row>
          <Row label="TMDB key" htmlFor="streaming.tmdb_key_env" help="Environment variable of flinch-arrd holding a TMDB v3 API key or v4 read token.">
            <TextField id="streaming.tmdb_key_env" spellCheck={false} autoComplete="off" value={form.streaming.tmdb_key_env} onChange={setIn('streaming', 'tmdb_key_env')} />
          </Row>
          <Row label="Status" help="Lookups trickle in, 40 a run, each kept a week.">
            <span className="text-fg-muted">
              {status?.streaming ? `${status.streaming.streaming} of ${status.streaming.known} titles stream on your services (${status.streaming.region})` : '—'}
              {' · '}<JustWatch />
            </span>
          </Row>
        </Section>

        <Section title="Quality" term="quality_actions">
          <Row label="Downgrades" htmlFor="quality_actions.enabled"
            help="Moves an item advised a downgrade to the compact profile and asks Radarr or Sonarr to search. Never a pinned item, one someone is partway through, or one being evicted; a show only when every season on disk is advised. Dry run prints the moves.">
            <Toggle id="quality_actions.enabled" checked={!!form.quality_actions.enabled} onChange={setIn('quality_actions', 'enabled')}>
              Act on downgrade advice
            </Toggle>
          </Row>
          <Row label="Daily cap" htmlFor="quality_actions.max_per_day" help="Most items moved in 24 hours; each season counts.">
            <NumberField {...numIn('quality_actions', 'max_per_day', { min: 1, max: 50, step: 1 })} unit="items" />
          </Row>
          <Row label="Smaller release" htmlFor="quality_actions.require_smaller_release" help="Move only when Prowlarr lists a release at least 30% smaller than the file.">
            <Toggle id="quality_actions.require_smaller_release" checked={!!form.quality_actions.require_smaller_release}
              onChange={setIn('quality_actions', 'require_smaller_release')}>
              Require one
            </Toggle>
          </Row>
          <Row label="Compact profile" htmlFor="quality_actions.radarr_profile"
            help="Used when the quality sync manages no compact profile: the profile's name in Radarr and in Sonarr. Blank: none.">
            <TextField id="quality_actions.radarr_profile" aria-label="Radarr compact profile" placeholder="Radarr profile"
              value={form.quality_actions.radarr_profile || ''} onChange={setIn('quality_actions', 'radarr_profile')} />
            <TextField id="quality_actions.sonarr_profile" aria-label="Sonarr compact profile" placeholder="Sonarr profile"
              value={form.quality_actions.sonarr_profile || ''} onChange={setIn('quality_actions', 'sonarr_profile')} />
          </Row>
          <Row label="Upgrade searches" htmlFor="upgrade_search.enabled" term="upgrade_search"
            help="Asks Radarr or Sonarr to search items below their cutoff, likeliest watched first, only where the disk forecast has room for the larger file. Never a pinned item, one someone is partway through, one hard to get back, one in the plan or one grabbed again and again. Dry run prints the searches.">
            <Toggle id="upgrade_search.enabled" checked={!!form.upgrade_search?.enabled} onChange={setIn('upgrade_search', 'enabled')}>
              Search for upgrades
            </Toggle>
          </Row>
          <Row label="Searches per day" htmlFor="upgrade_search.max_per_day" help="Most upgrade searches in 24 hours; each season counts.">
            <NumberField {...numIn('upgrade_search', 'max_per_day', { min: 1, max: 50, step: 1 })} unit="items" />
          </Row>
          <Row label="Upgrade churn" htmlFor="upgrade_guard.enabled" term="upgrade_churn"
            help="Flags items Radarr or Sonarr grabbed more often than this in 30 days.">
            <Toggle id="upgrade_guard.enabled" checked={!!form.upgrade_guard.enabled} onChange={setIn('upgrade_guard', 'enabled')}>
              Watch for it
            </Toggle>
            <NumberField {...numIn('upgrade_guard', 'max_grabs_per_item_30d', { min: 1, max: 100, step: 1 })} unit="grabs" />
          </Row>
          <Row label="On churn" htmlFor="upgrade_guard.action"
            help="Unmonitor stops the item downloading at all. Upgrades off applies to every item on its profile, and is skipped for profiles the quality sync manages.">
            <select id="upgrade_guard.action" value={form.upgrade_guard.action || 'flag'} onChange={setIn('upgrade_guard', 'action')}
              className="min-h-[40px] rounded-md border border-line bg-ink-900 px-2 text-[13px] text-fg focus:border-fg-faint focus:outline-none sm:min-h-0">
              <option value="flag">Flag only</option>
              <option value="unmonitor">Unmonitor the item</option>
              <option value="upgrades_off">Turn upgrades off on its profile</option>
            </select>
          </Row>
        </Section>

        <Section title="Archive" term="archive">
          <Row label="Archive tier" htmlFor="archive.enabled"
            help="Moves a movie or a whole series to an archive root folder on another disk instead of deleting it, while that disk stays under its own target. Moves go before deletions; pinned, partway and already handed-over items never move. Dry run prints the moves.">
            <Toggle id="archive.enabled" checked={!!form.archive?.enabled} onChange={setIn('archive', 'enabled')}>
              Archive instead of deleting
            </Toggle>
          </Row>
          <Row label="Root folders" htmlFor="archive.radarr_root"
            help="Paths as Radarr and Sonarr see them. Add each as a root folder in its app, and as a folder of the same Plex or Jellyfin library so archived items stay playable. Blank: that app never archives.">
            <TextField id="archive.radarr_root" aria-label="Radarr archive root" placeholder="/archive/movies" spellCheck={false} autoComplete="off"
              value={form.archive?.radarr_root ?? ''} onChange={setIn('archive', 'radarr_root')} />
            <TextField id="archive.sonarr_root" aria-label="Sonarr archive root" placeholder="/archive/tv" spellCheck={false} autoComplete="off"
              value={form.archive?.sonarr_root ?? ''} onChange={setIn('archive', 'sonarr_root')} />
          </Row>
          <Row label="Moves per run" htmlFor="archive.max_moves_per_run" help="Each move copies a whole movie or series; a move waits for the grace runs like a deletion.">
            <NumberField {...numIn('archive', 'max_moves_per_run', { min: 1, max: 50, step: 1 })} unit="moves" />
          </Row>
          <Row label="This run">
            <span className="text-fg-muted">{archiveSummary(status?.archive)}</span>
          </Row>
        </Section>

        <Section title="Duplicates" term="dupes">
          <Row label="Finder" htmlFor="dupes.enabled"
            help="Lists movies held in several copies (Plex versions, or one per library) and large folders under the Radarr and Sonarr roots that no item owns, with the copy to keep. Choose and confirm on the Overview.">
            <Toggle id="dupes.enabled" checked={!!form.dupes?.enabled} onChange={setIn('dupes', 'enabled')}>
              Find duplicates
            </Toggle>
          </Row>
          <Row label="Keep" htmlFor="dupes.prefer" help="Which copy the recommendation keeps when Radarr and the household's plays do not decide. Quality advice to downgrade prefers HD.">
            <select id="dupes.prefer" value={form.dupes?.prefer || 'highest'} onChange={setIn('dupes', 'prefer')}
              className="min-h-[40px] rounded-md border border-line bg-ink-900 px-2 text-[13px] text-fg focus:border-fg-faint focus:outline-none sm:min-h-0">
              <option value="highest">The highest picture</option>
              <option value="hd">HD over 4K</option>
            </select>
          </Row>
          <Row label="Remove" htmlFor="dupes.act"
            help="Removes the other copies of a confirmed choice: a Plex version through Plex (needs Settings → Library → Allow media deletion), a second Radarr's file through that Radarr. Never the file the only Radarr tracks, nor a pinned item or one someone is partway through. Dry run prints the removals.">
            <Toggle id="dupes.act" checked={!!form.dupes?.act} onChange={setIn('dupes', 'act')}>
              Act on confirmed choices
            </Toggle>
          </Row>
          <Row label="Removals per run" htmlFor="dupes.max_per_run">
            <NumberField {...numIn('dupes', 'max_per_run', { min: 1, max: 50, step: 1 })} unit="copies" />
          </Row>
          <Row label="Unowned folders" htmlFor="dupes.unowned_min_gib" help="Listed from this size; at most this many folders are measured per app each run.">
            <NumberField {...numIn('dupes', 'unowned_min_gib', { min: 1, max: 10000, step: 1 })} unit="GiB" />
            <NumberField {...numIn('dupes', 'unowned_max_folders', { min: 0, max: 200, step: 1 })} unit="folders" />
          </Row>
        </Section>

        <Section title="Quality profiles (TRaSH)" term="trash_sync">
          <Row label="Sync" htmlFor="trash.enabled"
            help="Previews Radarr’s and Sonarr’s custom formats, profiles and quality sizes against TRaSH-Guides on the Quality profiles tab. Nothing is written until you apply there; the planner’s dry run only prints.">
            <Toggle id="trash.enabled" checked={!!form.trash.enabled} onChange={setIn('trash', 'enabled')}>
              Preview the TRaSH sync
            </Toggle>
          </Row>
          <Row label="Apply" htmlFor="trash.apply" help="Apply every previewed change on schedule, like a Recyclarr cron job. Off: only the changes you select are applied.">
            <Toggle id="trash.apply" checked={!!form.trash.apply} onChange={setIn('trash', 'apply')}>
              Apply automatically
            </Toggle>
          </Row>
          <Row label="Schedule" htmlFor="trash.schedule_hours" help="Hours between previews. A saved change or an apply previews sooner.">
            <NumberField id="trash.schedule_hours" value={form.trash.schedule_hours} onChange={setIn('trash', 'schedule_hours')} min={1} max={720} step={1} unit="hours" />
          </Row>
          <Row label="Your formats" htmlFor="trash.delete_unmanaged_custom_formats"
            help="FLINCH deletes only custom formats it created that no synced profile uses. On: also offer to delete every other custom format nobody synced.">
            <Toggle id="trash.delete_unmanaged_custom_formats" checked={!!form.trash.delete_unmanaged_custom_formats}
              onChange={setIn('trash', 'delete_unmanaged_custom_formats')}>
              Offer to delete custom formats FLINCH did not create
            </Toggle>
          </Row>
          <Row label="Guide commit" htmlFor="trash.guide_commit" help="The TRaSH-Guides commit read; a new one is fetched once and cached on the state volume.">
            <TextField id="trash.guide_commit" spellCheck={false} autoComplete="off" value={form.trash.guide_commit || ''} onChange={setIn('trash', 'guide_commit')} />
          </Row>
          <Row label="Unused profiles" htmlFor="trash.delete_unused_profiles"
            help="Offer to delete quality profiles the sync does not manage and no movie or series uses. Never a profile in use, never the quality actions’ compact profile, and only when you select the deletion on the Quality profiles tab.">
            <Toggle id="trash.delete_unused_profiles" checked={!!form.trash.delete_unused_profiles} onChange={setIn('trash', 'delete_unused_profiles')}>
              Offer to delete unused profiles
            </Toggle>
          </Row>
          <Row label="Profilarr database" htmlFor="trash.pcd.repository"
            help="For instances with source pcd: a Profilarr Compliant Database and its schema, as GitHub owner/name at 40-hex commits. Its pcd.json must declare a license, read before anything else is fetched; the default Dictionarry database declares MIT.">
            <TextField id="trash.pcd.repository" aria-label="PCD repository" spellCheck={false} autoComplete="off" value={form.trash.pcd?.repository || ''} onChange={setPcd('repository')} />
            <TextField id="trash.pcd.commit" aria-label="PCD commit" spellCheck={false} autoComplete="off" value={form.trash.pcd?.commit || ''} onChange={setPcd('commit')} />
            <TextField id="trash.pcd.schema_repository" aria-label="PCD schema repository" spellCheck={false} autoComplete="off" value={form.trash.pcd?.schema_repository || ''} onChange={setPcd('schema_repository')} />
            <TextField id="trash.pcd.schema_commit" aria-label="PCD schema commit" spellCheck={false} autoComplete="off" value={form.trash.pcd?.schema_commit || ''} onChange={setPcd('schema_commit')} />
          </Row>
          <Row label="Profiles" htmlFor="trash-radarr" term="format_score"
            help="Recyclarr’s fields: source (trash or pcd), quality_profiles [{ trash_id, name, reset_unmatched_scores, upgrade { allowed, until_quality, until_score }, min_upgrade_format_score, qualities [{ name, qualities, enabled }], compact, score_multiplier }], custom_formats [{ trash_id, score or adjust_score, profiles }] for absolute or relative scores, quality_definition { type, preferred_ratio }, language { prefer: english | original | french, fallback, fallback_penalty } for TRaSH’s language formats. compact marks the profile downgrades move into. A PCD profile’s trash_id is the preview’s id for its name.">
            <TrashInstances form={form.trash} onChange={(trash) => update({ ...form, trash })} />
          </Row>
        </Section>

        <Section title="Notifications" term="notifications">
          <Row label="Channels" htmlFor="notify-name-0"
            help="Discord, ntfy, Apprise or any JSON webhook. Put a URL with a secret in an environment variable of flinch-arrd and name the variable here; the URL field is stored in settings.json and shown on this page.">
            <NotifyChannels channels={form.notify.channels} onChange={(channels) => update({ ...form, notify: { ...form.notify, channels } })} />
          </Row>
          <Row label="FLINCH address" htmlFor="notify.ui_url" help="Where the household opens FLINCH, for the Keep link in Leaving Soon messages. Blank sends no link.">
            <TextField id="notify.ui_url" placeholder="https://flinch.example.com" value={form.notify.ui_url} onChange={setIn('notify', 'ui_url')} />
          </Row>
          <Row label="Digest" htmlFor="notify.digest_hour_utc" help="The daily summary goes out at the first run from this hour (UTC).">
            <NumberField {...numIn('notify', 'digest_hour_utc', { min: 0, max: 23, step: 1 })} unit="h UTC" />
          </Row>
          <Row label="Rate limit" htmlFor="notify.max_per_hour" help="Most messages one channel gets per hour; the rest wait for a later run, none are dropped.">
            <NumberField {...numIn('notify', 'max_per_hour', { min: 1, max: 120, step: 1 })} unit="per hour" />
          </Row>
          <Row label="Test" help={status?.notify?.failures?.length ? `Last run: ${status.notify.failures.join('; ')}` : 'Posts a test message to every saved channel through the daemon.'}>
            <NotifyTest dirty={state === 'dirty' || state === 'error'} saved={form.notify.channels.length > 0} />
          </Row>
        </Section>

        <Section title="Household" term="household">
          <Row label="Keep and remove links" htmlFor="household.enabled"
            help="No-login links in notifications, the newsletter and the Plex Leaving Soon summary. Needs FLINCH_WEB_LINK_SECRET (32+ random characters) in both containers and the FLINCH address above.">
            <Toggle id="household.enabled" checked={!!form.household.enabled} onChange={setIn('household', 'enabled')}>
              Let the household keep titles, and ask for removals
            </Toggle>
            {status?.household && !status.household.links && form.household.enabled && <span className="text-state-warn">Links are off: see the Overview</span>}
          </Row>
          <Row label="Keep for" htmlFor="household.keep_days" help="A requested keep pins the title this long; links without a leave date last the second number.">
            <NumberField {...numIn('household', 'keep_days', { min: 1, max: 365, step: 1 })} unit="days" />
            <NumberField {...numIn('household', 'link_days', { min: 1, max: 90, step: 1 })} unit="days per link" />
          </Row>
          <Row label="Removals" htmlFor="household.allow_remove" help="Requesters get an “I'm done, remove it” link to their own titles; you approve each one, and it goes first when the disk needs space.">
            <Toggle id="household.allow_remove" checked={!!form.household.allow_remove} onChange={setIn('household', 'allow_remove')}>
              Offer removal links
            </Toggle>
          </Row>
          <Row label="Requesters" htmlFor="notify.household.enabled" help="Name and @mention whoever requested a Leaving Soon title, and tell them on their own addresses. Hiding names also stops mentions.">
            <Toggle id="notify.household.enabled" checked={!!form.notify.household.enabled} onChange={setHousehold('enabled')}>
              Tell requesters
            </Toggle>
            <Toggle id="notify.household.hide_requester" checked={!!form.notify.household.hide_requester} onChange={setHousehold('hide_requester')}>
              Hide names
            </Toggle>
          </Row>
          <Row label="Newsletter" htmlFor="notify.household.newsletter" help="Weekly: what is leaving soon with posters and keep links, what left. Channels get it when subscribed to “Newsletter”.">
            <Toggle id="notify.household.newsletter" checked={!!form.notify.household.newsletter} onChange={setHousehold('newsletter')}>
              Send it
            </Toggle>
            <select aria-label="Newsletter day" className="input min-h-[40px] sm:min-h-0" value={form.notify.household.newsletter_weekday} onChange={setHousehold('newsletter_weekday')}>
              {WEEKDAYS.map((name, i) => <option key={name} value={i}>{name}</option>)}
            </select>
            <NumberField id="notify.household.newsletter_hour_utc" value={form.notify.household.newsletter_hour_utc} onChange={setHousehold('newsletter_hour_utc')} min={0} max={23} step={1} unit="h UTC" />
          </Row>
          <Row label="Own addresses" htmlFor="notify.household.ntfy_server" help="An ntfy server for per-person topics, and the Apprise API for per-person Apprise URLs and email (Apprise's mailto://; its URL with the SMTP login goes in the variable).">
            <TextField id="notify.household.ntfy_server" placeholder="https://ntfy.sh/" value={form.notify.household.ntfy_server} onChange={setHousehold('ntfy_server')} />
            <TextField aria-label="ntfy token variable" placeholder="ntfy token variable" value={form.notify.household.ntfy_token_env} onChange={setHousehold('ntfy_token_env')} />
            <TextField aria-label="Apprise API" placeholder="http://apprise:8000" value={form.notify.household.apprise_api} onChange={setHousehold('apprise_api')} />
            <TextField aria-label="Email variable" placeholder="FLINCH_MAILTO_URL" value={form.notify.household.email_url_env} onChange={setHousehold('email_url_env')} />
            <Toggle id="notify.household.email_seerr_users" checked={!!form.notify.household.email_seerr_users} onChange={setHousehold('email_seerr_users')}>
              Email every Seerr user
            </Toggle>
          </Row>
          <Row label="Recipients" htmlFor="household-user-0" help="Per person, matched to a Seerr user by any of their names: a Discord id to mention, an ntfy topic, an Apprise URL variable, another email.">
            <HouseholdRecipients recipients={form.notify.household.recipients}
              onChange={(recipients) => update({ ...form, notify: { ...form.notify, household: { ...form.notify.household, recipients } } })} />
          </Row>
        </Section>

        <div className="sticky bottom-0 -mx-4 mt-5 flex flex-wrap items-center gap-3 rounded-b-xl border-t border-line-soft bg-ink-900 px-4 py-3 sm:-mx-5 sm:px-5">
          <button className="btn btn-primary" onClick={save} disabled={state === 'saving'}>
            {state === 'saving' ? <Loader2 size={14} className="animate-spin" /> : state === 'saved' ? <Check size={14} /> : <Save size={14} />}
            {state === 'saving' ? 'Saving…' : state === 'saved' ? 'Saved' : 'Save settings'}
          </button>
          {state === 'error'
            ? <span className="text-[12px] text-state-bad">{error}</span>
            : <span className="text-[12px] text-fg-faint">{state === 'dirty' ? 'Unsaved changes. ' : ''}Applies on the next run.</span>}
        </div>
      </Card>
    </div>
  );
}

const Section = ({ title, term, children }) => (
  <section className="mt-6 first:mt-0">
    <SectionTitle term={term}>{title}</SectionTitle>
    <div className="divide-y divide-line-soft">{children}</div>
  </section>
);

/** Label column (with an optional glossary explainer), control, one line of help underneath. */
const Row = ({ label, htmlFor, term, help, children }) => (
  <div className="grid gap-x-6 gap-y-1.5 py-3 first:pt-0 sm:grid-cols-[160px_minmax(0,1fr)]">
    <div className="flex items-start gap-1.5 sm:pt-[7px]">
      <label htmlFor={htmlFor} className="text-fg">{label}</label>
      {term && <span className="inline-flex h-[1.5em] items-center"><Explain term={term} /></span>}
    </div>
    <div className="min-w-0">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2">{children}</div>
      {help && <p className="mt-1.5 text-[12px] leading-snug text-fg-faint">{help}</p>}
    </div>
  </div>
);

/** Fixed-width, right-aligned input so values and units line up down the page. */
const NumberField = ({ unit, ...input }) => (
  <span className="inline-flex items-center gap-2">
    <input className="input num min-h-[40px] w-24 text-right sm:min-h-0" type="number" inputMode="decimal" {...input} />
    {unit && <span className="text-fg-muted">{unit}</span>}
  </span>
);

const TextField = (props) => (
  <input className="input min-h-[40px] w-full sm:min-h-0 sm:w-72" {...props} />
);

const Toggle = ({ id, checked, onChange, disabled = false, children }) => (
  <label htmlFor={id} className={`inline-flex min-h-[40px] items-center gap-2 sm:min-h-0 ${disabled ? 'cursor-not-allowed opacity-50' : 'cursor-pointer'}`}>
    <input id={id} type="checkbox" checked={checked} onChange={onChange} disabled={disabled} />
    <span>{children}</span>
  </label>
);

