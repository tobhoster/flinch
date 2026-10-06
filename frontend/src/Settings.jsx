import React, { useEffect, useState } from 'react';
import { Check, Loader2, Plus, Save, X } from 'lucide-react';
import { Card, SectionTitle, every } from './ui.jsx';
import { loadSettings, saveSettings } from './api.js';
import { Explain } from './Explain.jsx';

// Settings are written to `state/settings.json` on the shared volume and
// re-read by the daemon each cycle, so changes apply on the next run.

const GIB = 2 ** 30;
const PERCENT = { scale: 100, ok: (v) => v > 0 && v <= 100 };
const BYTES = { scale: 1 / GIB, digits: 2 };
const inRange = (lo, hi) => ({ ok: (v) => v >= lo && v <= hi });

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
];

const groupOf = (obj, group) => (group ? obj[group] : obj);

/** Settings as the form shows them: scaled numbers, user weights as editable rows. */
function toForm(settings) {
  const form = { ...settings, capacity: { ...settings.capacity }, planner: { ...settings.planner } };
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
  const payload = { ...form, capacity: { ...form.capacity }, planner: { ...form.planner } };
  delete payload.plex_token_set;
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
  return { payload, invalid };
}

export default function Settings({ status }) {
  const [form, setForm] = useState(null);
  const [loadError, setLoadError] = useState('');
  const [state, setState] = useState('idle'); // idle | dirty | saving | saved | error
  const [error, setError] = useState('');

  useEffect(() => {
    loadSettings().then((s) => setForm(toForm(s))).catch((err) => setLoadError(String(err.message || err)));
  }, []);

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
    setState('saving');
    try {
      await saveSettings(payload);
      setForm(toForm(await loadSettings()));
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
          <Row label="Dry run" htmlFor="planner.dry_run" term="dry_run" help="Items are handed to Maintainerr only when this is off.">
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
        </Section>

        <Section title="Maintainerr collections">
          <Row label="Movies" htmlFor="collection_movie" help="Delete collection for movies.">
            <TextField id="collection_movie" value={form.collection_movie} onChange={set('collection_movie')} />
          </Row>
          <Row label="Seasons" htmlFor="collection_season" help="Delete collection for seasons.">
            <TextField id="collection_season" value={form.collection_season} onChange={set('collection_season')} />
          </Row>
          <Row label="Leaving Soon" htmlFor="collection_leaving" term="leaving_soon"
            help="Announces items nobody finished before deletion. Blank keeps never-played reclaim off.">
            <TextField id="collection_leaving" value={form.collection_leaving} onChange={set('collection_leaving')} />
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

const Toggle = ({ id, checked, onChange, children }) => (
  <label htmlFor={id} className="inline-flex min-h-[40px] cursor-pointer items-center gap-2 sm:min-h-0">
    <input id={id} type="checkbox" checked={checked} onChange={onChange} />
    <span>{children}</span>
  </label>
);

