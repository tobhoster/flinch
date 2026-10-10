import React, { useEffect, useRef, useState } from 'react';
import { Check, Loader2, Plus, Send, X } from 'lucide-react';
import { loadNotifyTest, requestNotifyTest } from './api.js';

// The notification channels of Settings > Notifications. A channel's URL is
// usually a secret (a Discord webhook always is), so the daemon reads it from
// an environment variable; only a URL without a secret belongs in the URL
// field, which is stored in settings.json and shown here.

const KINDS = [
  ['discord', 'Discord webhook'],
  ['ntfy', 'ntfy topic'],
  ['apprise', 'Apprise API'],
  ['webhook', 'JSON webhook'],
];

const EVENTS = [
  ['leaving_soon', 'Leaving Soon'],
  ['deleted', 'Deletions'],
  ['problem', 'Problems'],
  ['digest', 'Daily digest'],
  ['newsletter', 'Newsletter'],
];

const PLACEHOLDER = {
  discord: 'https://discord.com/api/webhooks/…',
  ntfy: 'https://ntfy.sh/flinch',
  apprise: 'http://apprise:8000/notify/flinch',
  webhook: 'http://automation:8080/flinch',
};

// The newsletter is for the household; a new channel is the admin's.
const NEW_CHANNEL = { name: '', kind: 'discord', url_env: '', url: '', token_env: '', events: EVENTS.map(([key]) => key).filter((key) => key !== 'newsletter') };

const inputClass = 'input min-h-[40px] w-full sm:min-h-0';
const selectClass = 'min-h-[40px] rounded-md border border-line bg-ink-900 px-2 text-[13px] text-fg focus:border-fg-faint focus:outline-none sm:min-h-0';

/** The editable list of channels; `onChange` gets the whole new list. */
export function NotifyChannels({ channels, onChange }) {
  const set = (index, patch) => onChange(channels.map((channel, i) => (i === index ? { ...channel, ...patch } : channel)));
  const toggle = (index, event) => {
    const events = channels[index].events.includes(event)
      ? channels[index].events.filter((e) => e !== event)
      : [...channels[index].events, event];
    set(index, { events });
  };
  return (
    <div className="w-full space-y-3">
      {channels.map((channel, i) => (
        <div key={i} className="space-y-2 rounded-md border border-line-soft p-3">
          <div className="flex flex-wrap items-center gap-2">
            <input id={`notify-name-${i}`} aria-label="Channel name" className={`${inputClass} sm:w-40`} placeholder="Name"
              value={channel.name} onChange={(e) => set(i, { name: e.target.value })} />
            <select aria-label="Channel kind" className={selectClass} value={channel.kind}
              onChange={(e) => set(i, { kind: e.target.value, token_env: ['ntfy', 'webhook'].includes(e.target.value) ? channel.token_env : '' })}>
              {KINDS.map(([key, label]) => <option key={key} value={key}>{label}</option>)}
            </select>
            <button className="btn ml-auto px-2" aria-label={`Remove ${channel.name || 'channel'}`}
              onClick={() => onChange(channels.filter((_, j) => j !== i))}>
              <X size={13} />
            </button>
          </div>
          <input aria-label="URL variable" className={`${inputClass} sm:w-72`} placeholder="URL variable, e.g. FLINCH_DISCORD_URL"
            autoComplete="off" spellCheck={false} value={channel.url_env} onChange={(e) => set(i, { url_env: e.target.value.toUpperCase() })} />
          <input aria-label="URL" className={inputClass} placeholder={`or a URL without a secret: ${PLACEHOLDER[channel.kind]}`}
            autoComplete="off" spellCheck={false} value={channel.url} onChange={(e) => set(i, { url: e.target.value })} />
          {['ntfy', 'webhook'].includes(channel.kind) && (
            <input aria-label="Token variable" className={`${inputClass} sm:w-72`} placeholder="Token variable (optional bearer token)"
              autoComplete="off" spellCheck={false} value={channel.token_env} onChange={(e) => set(i, { token_env: e.target.value.toUpperCase() })} />
          )}
          <div className="flex flex-wrap gap-x-4 gap-y-1">
            {EVENTS.map(([key, label]) => (
              <label key={key} className="inline-flex cursor-pointer items-center gap-1.5">
                <input type="checkbox" checked={channel.events.includes(key)} onChange={() => toggle(i, key)} />
                <span>{label}</span>
              </label>
            ))}
          </div>
        </div>
      ))}
      <button className="btn px-2.5 text-xs" disabled={channels.length >= 10} onClick={() => onChange([...channels, { ...NEW_CHANNEL }])}>
        <Plus size={13} /> Add channel
      </button>
    </div>
  );
}

const POLL_MS = 2000;
const GIVE_UP_MS = 60000;

/**
 * "Send test": the daemon posts to every saved channel (only it holds their
 * secrets) and writes its answer, which this polls for. Unsaved changes are
 * not tested, so the button waits for a save.
 */
export function NotifyTest({ dirty, saved }) {
  const [state, setState] = useState('idle'); // idle | waiting | done | error
  const [result, setResult] = useState(null);
  const [error, setError] = useState('');
  const timer = useRef(null);
  useEffect(() => () => clearTimeout(timer.current), []);

  const send = async () => {
    setState('waiting');
    setResult(null);
    try {
      const id = await requestNotifyTest();
      const started = Date.now();
      const poll = async () => {
        const answer = await loadNotifyTest().catch(() => null);
        if (answer && answer.id === id) {
          setResult(answer);
          setState('done');
        } else if (Date.now() - started > GIVE_UP_MS) {
          setError('The daemon did not answer within a minute. Is flinch-arrd running?');
          setState('error');
        } else {
          timer.current = setTimeout(poll, POLL_MS);
        }
      };
      timer.current = setTimeout(poll, POLL_MS);
    } catch (err) {
      setError(String(err.message || err));
      setState('error');
    }
  };

  return (
    <div className="w-full space-y-2">
      <div className="flex flex-wrap items-center gap-3">
        <button className="btn" onClick={send} disabled={dirty || !saved || state === 'waiting'}>
          {state === 'waiting' ? <Loader2 size={13} className="animate-spin" /> : <Send size={13} />}
          {state === 'waiting' ? 'Waiting for the daemon…' : 'Send test'}
        </button>
        {dirty && <span className="text-[12px] text-fg-faint">Save first: the test uses the saved channels.</span>}
        {!dirty && !saved && <span className="text-[12px] text-fg-faint">Add and save a channel first.</span>}
      </div>
      {state === 'error' && <p className="text-[12px] text-state-bad">{error}</p>}
      {state === 'done' && result?.error && <p className="text-[12px] text-state-bad">{result.error}</p>}
      {state === 'done' && (
        <ul className="space-y-1 text-[12px]">
          {result.channels.map((channel) => (
            <li key={channel.name} className={channel.ok ? 'text-fg-muted' : 'text-state-bad'}>
              {channel.ok ? <Check size={12} className="mr-1 inline" /> : <X size={12} className="mr-1 inline" />}
              {channel.name}: {channel.detail}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
