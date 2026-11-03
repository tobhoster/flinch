import React, { useState } from 'react';
import { Check, Eye, Loader2, Save } from 'lucide-react';
import { loadSettings, previewRules, saveSettings } from '../api.js';
import { PreviewResult, rulesPayload } from '../RuleSettings.jsx';

// Overriding one title from the graph. Regret and P(watch) stay the model's
// (rules never change a score, see docs/how-it-works.md#rules-hard-constraints-never-a-score);
// what the operator can change is whether the plan may or must take it. Each
// choice is a rule scoped to this title's id, previewed on the daemon's last
// inputs before it is saved, exactly as Settings › Rules does it.

const CHOICES = [
  { type: 'keep', verb: 'Keep', help: 'Never taken while the rule stands.' },
  { type: 'prefer_evict', verb: 'Prefer evicting', help: 'Taken before untouched items on its disk, only as far as the target needs; safeguards still hold.' },
];

const SETTINGS_VIEW_ONLY = ['plex_token_set', 'jellyfin_token_set'];

function draftWith(rules, item, type) {
  const verb = CHOICES.find((c) => c.type === type).verb;
  const base = `${verb}: ${item.title}`;
  const names = new Set(rules.map((r) => r.name));
  let name = base;
  for (let n = 2; names.has(name); n += 1) name = `${base} (${n})`;
  return [...rules, { name, enabled: true, scope: { ids: [item.id] }, effect: { type } }];
}

export default function RuleOverride({ item }) {
  const [state, setState] = useState({ step: 'idle' });

  const existing = state.settings?.rules?.filter((r) => r.scope?.ids?.includes(item.id)) || [];

  const preview = async (type) => {
    setState({ step: 'previewing', type });
    try {
      const settings = await loadSettings();
      const draft = rulesPayload(draftWith(settings.rules || [], item, type));
      const diff = await previewRules(draft);
      setState({ step: 'previewed', type, settings, draft, diff });
    } catch (err) {
      setState({ step: 'error', error: String(err.message || err) });
    }
  };

  const save = async () => {
    setState((s) => ({ ...s, step: 'saving' }));
    try {
      const payload = Object.fromEntries(Object.entries(state.settings).filter(([key]) => !SETTINGS_VIEW_ONLY.includes(key)));
      await saveSettings({ ...payload, rules: state.draft });
      setState({ step: 'saved', type: state.type });
    } catch (err) {
      setState({ step: 'error', error: String(err.message || err) });
    }
  };

  return (
    <div className="space-y-2">
      <div className="text-[11px] uppercase tracking-wide text-fg-faint">Override</div>
      <div className="flex flex-wrap gap-2">
        {CHOICES.map((c) => (
          <button key={c.type} className="btn" title={c.help} disabled={state.step === 'previewing' || state.step === 'saving'} onClick={() => preview(c.type)}>
            {state.step === 'previewing' && state.type === c.type ? <Loader2 size={13} className="animate-spin" /> : <Eye size={13} />} {c.verb}…
          </button>
        ))}
      </div>
      <p className="text-[11px] text-fg-faint">Scores stay the model's. An override is a rule on this title, previewed before it is saved; remove it in Settings › Rules.</p>
      {existing.length > 0 && <p className="text-[12px] text-fg-muted">Already named by: {existing.map((r) => r.name).join(', ')}</p>}
      {state.step === 'error' && <p className="text-[12px] text-state-bad">{state.error}</p>}
      {(state.step === 'previewed' || state.step === 'saving') && (
        <>
          <PreviewResult diff={state.diff} />
          <button className="btn btn-primary" disabled={state.step === 'saving'} onClick={save}>
            {state.step === 'saving' ? <Loader2 size={13} className="animate-spin" /> : <Save size={13} />} Save rule
          </button>
        </>
      )}
      {state.step === 'saved' && <p className="inline-flex items-center gap-1 text-[12px] text-state-ok"><Check size={13} /> Saved; the next run plans with it.</p>}
    </div>
  );
}
