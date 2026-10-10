import React, { useState } from 'react';
import { decideDupe } from './api.js';
import { GiB, SectionTitle, day } from './ui.jsx';
import { instanceKeyLabel } from './arrRef.js';

const LIMIT = 8;

const OUTCOME = { removed: 'removed', simulated: 'dry run: would remove', failed: 'failed' };

/** One copy in a line: where it is, its picture and size, who tracks or played it. */
function label(c) {
  const where = c.rating_key ? `Plex ${c.rating_key}${c.section_id != null ? ` · section ${c.section_id}` : ''}` : instanceKeyLabel(c.owner?.instance || 'radarr');
  const picture = c.resolution ? (c.resolution === 'sd' ? 'SD' : `${c.resolution}p`) : 'picture unknown';
  const tracked = c.owner ? ` · ${instanceKeyLabel(c.owner.instance)}'s file` : '';
  const plays = c.plays ? ` · ${c.plays} play${c.plays === 1 ? '' : 's'}` : '';
  return `${where} · ${picture} · ${GiB(c.bytes)} GiB${tracked}${plays}`;
}

/** A group's copies to choose from, then confirm; the choice lives in `dupes.json`. */
function Group({ group, act }) {
  const [decision, setDecision] = useState(group.decision ?? null);
  const [picked, setPicked] = useState(group.decision?.keep ?? group.recommended);
  const [error, setError] = useState(null);
  const [busy, setBusy] = useState(false);
  const send = async (keep, confirm) => {
    setBusy(true);
    setError(null);
    try {
      setDecision((await decideDupe(group.id, keep, confirm)).decision ?? null);
    } catch (e) {
      setError(e.message);
    } finally {
      setBusy(false);
    }
  };
  const chosen = decision?.keep;
  return (
    <li className="space-y-1 rounded-md border border-line-soft p-2">
      <div className="break-words">
        <span className="text-fg">{group.title}{group.year ? ` (${group.year})` : ''}</span>
        {' · '}<span className="num">{GiB(group.redundant_bytes)}</span> GiB redundant
      </div>
      <fieldset className="space-y-0.5" disabled={busy}>
        {group.copies.map((c) => (
          <label key={c.id} className="flex items-start gap-2 text-fg-muted">
            <input type="radio" name={`keep-${group.id}`} checked={picked === c.id} onChange={() => setPicked(c.id)} className="mt-1" />
            <span className="break-all">
              {label(c)}
              {c.id === group.recommended && <span className="text-state-ok"> · recommended</span>}
            </span>
          </label>
        ))}
      </fieldset>
      <p className="text-xs text-fg-faint">{group.reasons.join('; ')}</p>
      <div className="flex flex-wrap items-center gap-2 text-xs">
        {chosen !== picked || !decision ? (
          <button className="btn px-2" disabled={busy} onClick={() => send(picked, false)}>Keep this copy</button>
        ) : !decision.confirmed ? (
          <button className="btn px-2" disabled={busy} onClick={() => send(picked, true)}>Confirm: remove the others</button>
        ) : (
          <span className="text-fg">Confirmed{act ? '' : ' · removing is off (Settings → Duplicates)'}</span>
        )}
        {decision && <button className="btn px-2" disabled={busy} onClick={() => send(null, false)}>Clear</button>}
        {group.held && <span className="text-state-warn">{group.held}</span>}
        {error && <span className="text-state-warn">{error}</span>}
      </div>
    </li>
  );
}

/**
 * Duplicate copies (`status.dupes`): each group with the copy FLINCH
 * recommends, choose-then-confirm, and the removals made. Absent (finder
 * off, or an older snapshot) renders nothing.
 */
export default function Dupes({ dupes }) {
  if (!dupes) return null;
  const groups = dupes.groups ?? [];
  const unowned = dupes.unowned ?? [];
  const acted = dupes.acted ?? [];
  const problems = dupes.problems ?? [];
  if (!groups.length && !unowned.length && !acted.length && !problems.length) return null;
  return (
    <section className="min-w-0 space-y-3">
      <SectionTitle hint={dupes.act && dupes.dry_run ? 'dry run: printed, not sent' : undefined} term="dupes">Duplicates</SectionTitle>
      {groups.length > 0 && (
        <ul className="space-y-2">
          {groups.slice(0, LIMIT).map((g) => <Group key={g.id} group={g} act={dupes.act} />)}
          {groups.length > LIMIT && <li className="text-fg-faint">+{groups.length - LIMIT} more</li>}
        </ul>
      )}
      {unowned.length > 0 && (
        <div>
          <p className="text-fg-muted">Under a root folder, owned by no item:</p>
          <ul className="space-y-0.5 text-fg-muted">
            {unowned.slice(0, LIMIT).map((f) => (
              <li key={`${f.app}:${f.path}`} className="break-all">
                <span className="text-fg">{f.path}</span> · <span className="num">{GiB(f.bytes)}</span> GiB · {f.files} file{f.files === 1 ? '' : 's'} · {f.app}
              </li>
            ))}
          </ul>
        </div>
      )}
      {acted.length > 0 && (
        <ul className="space-y-0.5 text-fg-muted">
          {acted.slice(0, LIMIT).map((a) => (
            <li key={`${a.copy}:${a.at_unix}`} className="break-words">
              <span className="text-fg">{a.title}</span> · <span className="num">{day(a.at_unix)}</span> · {OUTCOME[a.outcome] ?? a.outcome}
              {' · '}<span className="num">{GiB(a.bytes)}</span> GiB{a.detail ? ` · ${a.detail}` : ''}
            </li>
          ))}
        </ul>
      )}
      {problems.map((p) => <p key={p} className="text-state-warn">{p}</p>)}
    </section>
  );
}
