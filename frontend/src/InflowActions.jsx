import React from 'react';
import { instanceLabel, parseArrRef } from './arrRef.js';

// Settings > Rules > Inflow actions: the approval list for acting on inflow
// advice (crates/flinch-archive/src/inflow/act.rs). The operator ticks the
// shows whose future seasons Sonarr may stop fetching, and the import lists
// whose automatic add may go off; the daemon writes only while a disk is
// over its target, prints instead in a dry run, and switches the lists back
// on afterwards. The UI holds no *arr key: shows come from `status.inflow`,
// lists from what the daemon read (`status.inflow_actions.import_lists`).
// Shows of a named Sonarr (`sonarr@anime-3`) are approved the same way; import
// lists cover the default instances only.

/** The form's copy of `settings.inflow_actions`. */
export function inflowActionsForm(config) {
  return { enabled: !!config?.enabled, approved: config?.approved || [], import_lists: config?.import_lists || [] };
}

const sameList = (a, b) => a.app === b.app && a.id === b.id;
const listLabel = (list) => `${instanceLabel(list.app)} list ${list.id}`;
/** ` · Sonarr anime` for a show of a named instance; nothing for the default. */
const instanceNote = (subject) => {
  const ref = parseArrRef(subject);
  return ref?.instance ? ` · ${instanceLabel(ref.app, ref.instance)}` : '';
};

function Check({ checked, onChange, children }) {
  return (
    <label className="flex min-h-[40px] cursor-pointer items-start gap-2 sm:min-h-0">
      <input type="checkbox" className="mt-[3px]" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      <span className="min-w-0 break-words">{children}</span>
    </label>
  );
}

/** Ticks for approved shows and chosen import lists. */
export function InflowApprovals({ config, onChange, suggestions, status }) {
  const shows = (suggestions || []).filter((s) => parseArrRef(s.subject)?.app === 'sonarr');
  const lapsed = config.approved.filter((subject) => !shows.some((s) => s.subject === subject));
  const approve = (subject, on) => onChange({
    ...config, approved: on ? [...new Set([...config.approved, subject])] : config.approved.filter((s) => s !== subject),
  });
  const known = status?.import_lists || [];
  const unknown = config.import_lists.filter((list) => !known.some((k) => sameList(k, list)));
  const choose = (list, on) => onChange({
    ...config,
    import_lists: on ? [...config.import_lists.filter((l) => !sameList(l, list)), { app: list.app, id: list.id }] : config.import_lists.filter((l) => !sameList(l, list)),
  });
  const held = (list) => (status?.lists_off || []).some((l) => sameList(l, list));
  return (
    <div className="w-full space-y-3">
      <div>
        <div className="mb-1 text-fg-muted">Shows: unmonitor future seasons</div>
        {!shows.length && !lapsed.length && <p className="text-fg-faint">No show is suggested right now.</p>}
        {shows.map((s) => (
          <Check key={s.subject} checked={config.approved.includes(s.subject)} onChange={(on) => approve(s.subject, on)}>
            <span className="text-fg">{s.title}</span>
            <span className="text-fg-faint">{instanceNote(s.subject)} · {s.why}{(status?.unmonitored || []).includes(s.subject) ? ' · done' : ''}</span>
          </Check>
        ))}
        {lapsed.map((subject) => (
          <Check key={subject} checked onChange={(on) => approve(subject, on)}>
            <span className="text-fg">{subject}</span><span className="text-fg-faint"> · no longer suggested: left alone</span>
          </Check>
        ))}
      </div>
      <div>
        <div className="mb-1 text-fg-muted">Import lists: automatic add off while over target</div>
        {!known.length && !unknown.length && <p className="text-fg-faint">The daemon lists them once inflow actions are on and it ran.</p>}
        {known.map((list) => (
          <Check key={`${list.app}-${list.id}`} checked={config.import_lists.some((l) => sameList(l, list))} onChange={(on) => choose(list, on)}>
            <span className="text-fg">{list.name || listLabel(list)}</span>
            <span className="text-fg-faint"> · {listLabel(list)}{list.auto_add === false ? ' · automatic add off' : ''}{held(list) ? ' · held off by FLINCH' : ''}</span>
          </Check>
        ))}
        {unknown.map((list) => (
          <Check key={`${list.app}-${list.id}`} checked onChange={(on) => choose(list, on)}>
            <span className="text-fg">{listLabel(list)}</span><span className="text-fg-faint"> · not listed this run</span>
          </Check>
        ))}
      </div>
    </div>
  );
}

/** One line on what the last run did. */
export function inflowActionsSummary(status) {
  const run = status?.inflow_actions;
  if (!run) return 'Off. With it on, nothing is written until a disk is over its target.';
  const parts = [run.over_target ? 'a disk is over its target' : 'no disk over its target: nothing written'];
  if (run.acted?.length) parts.push(run.acted.join('; '));
  if (run.problems?.length) parts.push(`problems: ${run.problems.join('; ')}`);
  if (run.dry_run) parts.push('dry run: printed, not sent');
  return parts.join(' · ');
}
