import React, { useEffect, useState } from 'react';
import { Check, Loader2, Save } from 'lucide-react';
import { Card, SectionTitle } from './ui.jsx';
import { loadSettings, saveSettings } from './api.js';
import { Explain } from './Explain.jsx';

// Settings are written to `state/settings.json` on the shared volume and
// re-read by the daemon each cycle, so changes apply on the next run.

/** Every numeric key, with the label used when its value will not parse. */
const NUMERIC = {
  capacity_ceiling: 'Ceiling (1–100 %)',
  capacity_release: 'Release mark (at least 1 %, below the ceiling)',
  interval_s: 'Scan interval',
  grace_runs: 'Grace runs',
  max_items: 'Items per run',
  max_gib: 'Size per run',
  score_floor: 'Score floor',
  score_temperature: 'Temperature',
  unwatched_reclaim_floor: 'Never-played floor',
  unwatched_reclaim_dwell_days: 'Days on disk',
};

/** Keys the operator edits in percent but the daemon stores as a fraction. */
const PERCENT = ['capacity_ceiling', 'capacity_release'];

/** Settings as the form shows them: fractions become percentages. */
function toForm(settings) {
  const form = { ...settings };
  for (const key of PERCENT) form[key] = Math.round(Number(form[key]) * 1e4) / 100;
  return form;
}

/**
 * Inputs hold whatever the user typed; numbers are coerced once, here, so the
 * PUT body never carries a string for a numeric field. Percent keys must lie
 * in 1..100 and leave as fractions; the release mark must sit below the
 * ceiling, or eviction would never unlatch. The token field starts blank (the
 * server never sends it back), and a blank one keeps the saved token.
 */
function toPayload(form) {
  const payload = { ...form };
  delete payload.plex_token_set;
  const invalid = [];
  for (const [key, label] of Object.entries(NUMERIC)) {
    const raw = form[key];
    const value = typeof raw === 'string' && raw.trim() === '' ? NaN : Number(raw);
    const percent = PERCENT.includes(key);
    if (!Number.isFinite(value) || (percent && (value < 1 || value > 100))) invalid.push(label);
    else payload[key] = percent ? value / 100 : value;
  }
  if (!invalid.length && payload.capacity_release >= payload.capacity_ceiling) invalid.push(NUMERIC.capacity_release);
  return { payload, invalid };
}

const every = (s) => {
  const n = Number(s);
  if (!Number.isFinite(n) || n <= 0) return null;
  if (n % 3600 === 0) return `every ${n / 3600} h`;
  if (n % 60 === 0) return `every ${n / 60} min`;
  return `every ${n} s`;
};

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

  const set = (key) => (event) => {
    const value = event.target.type === 'checkbox' ? event.target.checked : event.target.value;
    setForm({ ...form, [key]: value });
    setState('dirty');
  };

  const save = async () => {
    const { payload, invalid } = toPayload(form);
    if (invalid.length) {
      setError(`Enter a number for: ${invalid.join(', ')}.`);
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
  const ev = status?.evidence;
  const sync = status?.sync;
  // The daemon may take Plex from its environment, so its own report counts too.
  const plexReady = Boolean(form.plex_url && (form.plex_token || form.plex_token_set)) || Boolean(ev?.plex_configured);
  const [tautulliTone, tautulli] = !ev ? ['warn', 'no data']
    : !ev.tautulli_configured ? ['muted', 'not configured']
      : ev.tautulli_complete ? ['ok', 'read fully'] : ['warn', 'incomplete'];

  return (
    <div className="mt-6 grid grid-cols-1 gap-6 text-[13px] lg:grid-cols-[minmax(0,1fr)_280px]">
      <Card className="px-4 pt-4 sm:px-5 sm:pt-5">
        <Section title="Storage ceiling">
          <Row label="Watermarks" htmlFor="capacity_ceiling" term="watermarks"
            help="Below the ceiling nothing is deleted. Once disk use reaches the ceiling, eviction latches: the lowest expected regret per GiB goes first until use is back at the release mark, then it stops.">
            <NumberField {...num('capacity_ceiling', { min: 1, max: 100, step: 1 })} unit="% ceiling" />
            <NumberField {...num('capacity_release', { min: 1, max: 99, step: 1 })} unit="% release" />
          </Row>
          <Row label="While evicting" htmlFor="capacity_arm_never_played" term="never_played"
            help="Only while eviction is latched (from the ceiling down to the release mark), arms the calibrated never-played rule as an extra candidate source. It still needs its own floor and time on disk; the floor is never lowered. Idle, this has no effect.">
            <Toggle id="capacity_arm_never_played" checked={!!form.capacity_arm_never_played} onChange={set('capacity_arm_never_played')}>
              Arm never-played reclaim
            </Toggle>
          </Row>
        </Section>

        <Section title="Schedule">
          <Row label="Scan interval" htmlFor="interval_s" help="Time between runs. A run can also be started manually.">
            <NumberField {...num('interval_s', { min: 300, step: 300 })} unit="seconds" />
            {every(form.interval_s) && <span className="text-fg-faint">{every(form.interval_s)}</span>}
          </Row>
          <Row label="Grace runs" htmlFor="grace_runs" term="grace_runs" help="Consecutive runs an item must stay a candidate before it is scheduled.">
            <NumberField {...num('grace_runs', { min: 1, max: 20 })} unit="runs" />
          </Row>
          <Row label="Per-run cap" htmlFor="max_items" help="Most a single run may schedule. Whichever limit is hit first applies. An item larger than the size cap still goes, alone, as a run's first item; otherwise it could never be freed.">
            <NumberField {...num('max_items', { min: 0 })} unit="items" />
            <NumberField {...num('max_gib', { min: 0 })} unit="GiB" />
          </Row>
        </Section>

        <Section title="Safety">
          <Row label="Enforcement" htmlFor="enforce" term="dry_run"
            help="Off: classify only, nothing is sent to Maintainerr. On: candidates past the grace runs are added to the Maintainerr collections below, and Maintainerr deletes them on its own schedule.">
            <Toggle id="enforce" checked={!!form.enforce} onChange={set('enforce')}>
              Schedule deletions
            </Toggle>
            <span className="text-fg-muted">
              {form.enforce
                ? <><span className="text-state-bad">Enforced</span> — deletions are scheduled</>
                : 'Dry run — nothing is scheduled'}
            </span>
          </Row>
          <Row label="Keep tag" htmlFor="keep_tag" help="Radarr/Sonarr tag, Plex label or Plex collection name that makes an item untouchable, like a favorite. Empty disables it.">
            <TextField id="keep_tag" placeholder="flinch-keep" autoComplete="off" spellCheck={false} value={form.keep_tag} onChange={set('keep_tag')} />
          </Row>
        </Section>

        <Section title="Scoring" term="p_safe">
          <Row label="Score floor" htmlFor="score_floor" term="score_floor" help="Minimum P(safe) for an item to become a candidate. Policy must also allow it.">
            <NumberField {...num('score_floor', { min: 0, max: 1, step: 0.05 })} unit="P(safe)" />
          </Row>
          <Row label="Temperature" htmlFor="score_temperature" term="temperature" help="Scales raw scores before calibration. Above 1 makes them less extreme.">
            <NumberField {...num('score_temperature', { min: 0.1, step: 0.1 })} />
          </Row>
        </Section>

        <Section title="Never-played reclaim">
          <Row label="Never-played items" htmlFor="unwatched_reclaim_enabled" term="never_played"
            help="Off: anything nobody has played is held. On: never-played items that pass the floor and time on disk can become candidates.">
            <Toggle id="unwatched_reclaim_enabled" checked={!!form.unwatched_reclaim_enabled} onChange={set('unwatched_reclaim_enabled')}>
              Allow as candidates
            </Toggle>
            <span className="text-fg-muted">
              {form.unwatched_reclaim_enabled
                ? <><span className="text-state-warn">On</span> — never-played items can be deleted</>
                : 'Off — never-played items are held'}
            </span>
          </Row>
          <Row label="Floor" htmlFor="unwatched_reclaim_floor" term="p_safe" help="Minimum P(safe) for a never-played item.">
            <NumberField {...num('unwatched_reclaim_floor', { min: 0, max: 1, step: 0.05 })} unit="P(safe)" />
          </Row>
          <Row label="Time on disk" htmlFor="unwatched_reclaim_dwell_days" help="Minimum age before an unplayed item counts as never played.">
            <NumberField {...num('unwatched_reclaim_dwell_days', { min: 0, step: 1 })} unit="days" />
          </Row>
        </Section>

        <Section title="Maintainerr collections">
          <Row label="Movies" htmlFor="collection_movie" help="Delete collection for watched movies and duplicates.">
            <TextField id="collection_movie" value={form.collection_movie} onChange={set('collection_movie')} />
          </Row>
          <Row label="Seasons" htmlFor="collection_season" help="Delete collection for finished seasons.">
            <TextField id="collection_season" value={form.collection_season} onChange={set('collection_season')} />
          </Row>
          <Row label="Leaving Soon" htmlFor="collection_leaving" term="leaving_soon"
            help="Announces items nobody finished, in both libraries, before they are deleted. Blank sends them to the delete collections.">
            <TextField id="collection_leaving" value={form.collection_leaving} onChange={set('collection_leaving')} />
          </Row>
        </Section>

        <Section title="Plex">
          <Row label="URL" htmlFor="plex_url" help="Source of watch state. Without URL and token every item is held.">
            <TextField id="plex_url" placeholder="https://plex:32400" value={form.plex_url} onChange={set('plex_url')} />
          </Row>
          <Row label="Token" htmlFor="plex_token" help="Kept on the state volume and never sent back to the browser. Leave blank to keep the saved one.">
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

      <aside className="space-y-6">
        <div>
          <SectionTitle>Connections</SectionTitle>
          <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1.5">
            <Conn label="Radarr" value={status?.movies} unit="movies" />
            <Conn label="Sonarr" value={status?.seasons} unit="seasons" />
            <ConnText label="Plex" tone={plexReady ? 'ok' : 'warn'}>
              {plexReady ? 'configured' : 'not configured — all items held'}
            </ConnText>
            <ConnText label="Tautulli" tone={tautulliTone}>{tautulli}</ConnText>
            <ConnText label="Maintainerr" tone={!sync || sync.error ? 'warn' : 'ok'}>
              {!sync ? 'no data' : sync.error || (sync.version ? `v${sync.version.replace(/^v/, '')}` : 'read')}
            </ConnText>
          </dl>
        </div>
      </aside>
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

const Conn = ({ label, value, unit }) => (
  <>
    <dt className="text-fg-muted">{label}</dt>
    <dd className={value > 0 ? 'text-fg' : 'text-state-warn'}>
      {value > 0 ? <><span className="num">{value}</span> {unit}</> : 'no data'}
    </dd>
  </>
);

const CONN_TONES = { ok: 'text-fg', warn: 'text-state-warn', muted: 'text-fg-muted' };

const ConnText = ({ label, tone, children }) => (
  <>
    <dt className="text-fg-muted">{label}</dt>
    <dd className={`break-words ${CONN_TONES[tone]}`}>{children}</dd>
  </>
);
