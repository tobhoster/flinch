import React from 'react';
import { Plus, X } from 'lucide-react';

// Settings > Household: who gets told about their own titles, and where.
// An address with a secret in it (an Apprise URL, the email sender's SMTP
// login) lives in an environment variable of flinch-arrd; only its name is
// stored here.

export const WEEKDAYS = ['Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday', 'Sunday'];

const NEW_RECIPIENT = { user: '', discord_id: '', ntfy_topic: '', apprise_env: '', email: '', newsletter: true, muted: false };

const inputClass = 'input min-h-[40px] w-full sm:min-h-0';

/** `notify.household` as the form edits it: blanks for absent text. */
export function householdForm(household = {}) {
  return {
    ...household,
    ntfy_server: household.ntfy_server || '',
    ntfy_token_env: household.ntfy_token_env || '',
    apprise_api: household.apprise_api || '',
    email_url_env: household.email_url_env || '',
    newsletter_weekday: household.newsletter_weekday ?? 4,
    newsletter_hour_utc: household.newsletter_hour_utc ?? 17,
    recipients: (household.recipients || []).map((r) => ({ ...NEW_RECIPIENT, ...r })),
  };
}

/** The PUT body's `notify.household`, and the labels of fields that are off. */
export function householdPayload(form) {
  const invalid = [];
  const hour = Number(form.newsletter_hour_utc);
  if (!Number.isInteger(hour) || hour < 0 || hour > 23) invalid.push('Newsletter hour (0–23 UTC)');
  const trim = (value) => String(value || '').trim();
  const recipients = form.recipients.map((r) => ({
    ...r, user: trim(r.user), discord_id: trim(r.discord_id), ntfy_topic: trim(r.ntfy_topic), apprise_env: trim(r.apprise_env), email: trim(r.email),
  }));
  const users = recipients.map((r) => r.user.toLowerCase());
  if (users.some((user, i) => !user || users.indexOf(user) !== i)) invalid.push('Household recipients (one row per user)');
  if (recipients.some((r) => r.discord_id && !/^\d{17,20}$/.test(r.discord_id))) invalid.push('Discord user ids (17–20 digits)');
  const household = {
    ...form,
    ntfy_server: trim(form.ntfy_server),
    ntfy_token_env: trim(form.ntfy_token_env),
    apprise_api: trim(form.apprise_api),
    email_url_env: trim(form.email_url_env),
    newsletter_weekday: Number(form.newsletter_weekday),
    newsletter_hour_utc: hour,
    recipients,
  };
  return { household, invalid };
}

/** The editable recipient overrides; `onChange` gets the whole new list. */
export function HouseholdRecipients({ recipients, onChange }) {
  const set = (index, patch) => onChange(recipients.map((r, i) => (i === index ? { ...r, ...patch } : r)));
  const text = (index, key) => (event) => set(index, { [key]: event.target.value });
  const flag = (index, key) => (event) => set(index, { [key]: event.target.checked });
  return (
    <div className="w-full space-y-3">
      {recipients.map((r, i) => (
        <div key={i} className="space-y-2 rounded-md border border-line-soft p-2">
          <div className="flex items-center gap-2">
            <input id={`household-user-${i}`} aria-label="Seerr user" className={inputClass} placeholder="Seerr name, username, Plex name or email"
              value={r.user} onChange={text(i, 'user')} />
            <button className="btn px-2" aria-label={`Remove ${r.user || 'recipient'}`} onClick={() => onChange(recipients.filter((_, j) => j !== i))}>
              <X size={13} />
            </button>
          </div>
          <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
            <input aria-label="Discord user id" className={inputClass} placeholder="Discord user id (mentions)" value={r.discord_id} onChange={text(i, 'discord_id')} />
            <input aria-label="ntfy topic" className={inputClass} placeholder="Own ntfy topic" value={r.ntfy_topic} onChange={text(i, 'ntfy_topic')} />
            <input aria-label="Apprise URL variable" className={inputClass} placeholder="Variable with their Apprise URL" value={r.apprise_env} onChange={text(i, 'apprise_env')} />
            <input aria-label="Email" className={inputClass} placeholder="Email (instead of Seerr's)" value={r.email} onChange={text(i, 'email')} />
          </div>
          <div className="flex flex-wrap gap-4 text-fg-muted">
            <label className="flex items-center gap-2"><input type="checkbox" checked={!!r.newsletter} onChange={flag(i, 'newsletter')} /> Newsletter</label>
            <label className="flex items-center gap-2"><input type="checkbox" checked={!!r.muted} onChange={flag(i, 'muted')} /> Never message them</label>
          </div>
        </div>
      ))}
      <button className="btn px-2.5 text-xs" onClick={() => onChange([...recipients, { ...NEW_RECIPIENT }])}>
        <Plus size={13} /> Add recipient
      </button>
    </div>
  );
}
