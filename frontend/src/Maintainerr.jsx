import React from 'react';
import { GiB, SectionTitle } from './ui.jsx';
import { Explain } from './Explain.jsx';

/** What this run's sync did, as `[count, words]` pairs; zero counts are dropped. */
const tallies = (s) => [
  [s.exclusions_added, 'protected'],
  [s.scheduled, `scheduled (${GiB(s.scheduled_bytes)} GiB)`],
  [s.exclusions_removed, 'released'],
  [s.unscheduled, 'unscheduled'],
  [s.deferred, 'deferred by caps'],
  [s.skipped, 'skipped after an earlier failure'],
  [s.already_protected, 'already protected'],
  [s.already_scheduled, 'already scheduled'],
].filter(([n]) => n > 0);

/**
 * The Maintainerr hand-off from `status.sync`. `dryRun` is `status.dry_run`:
 * writes are shown, none sent. An absent block (older snapshot) renders nothing.
 */
export default function MaintainerrSync({ sync: s, dryRun }) {
  if (!s) return null;
  const done = tallies(s);
  return (
    <section className="min-w-0">
      <SectionTitle hint={s.version ? `v${s.version.replace(/^v/, '')}` : undefined} term="maintainerr">Maintainerr</SectionTitle>
      <ul className="space-y-1 text-fg-muted">
        {s.error ? (
          <li className="text-state-bad">
            Unreadable: <span className="text-fg">{s.error}</span>. Nothing synced.
          </li>
        ) : dryRun ? (
          <li>Dry run: <span className="num text-fg">{s.simulated ?? 0}</span> {s.simulated === 1 ? 'write' : 'writes'} shown, none sent.</li>
        ) : (
          <li>
            {done.length === 0
              ? 'Nothing to change this run.'
              : done.map(([n, words], i) => (
                <React.Fragment key={words}>{i > 0 && ' · '}<span className="num text-fg">{n}</span> {words}</React.Fragment>
              ))}
          </li>
        )}
        {s.announced > 0 && (
          <li>
            <span className="num text-fg">{s.announced}</span> {s.announced === 1 ? 'item' : 'items'} ({GiB(s.announced_bytes)} GiB)
            {dryRun ? ' would go' : ' went'} to Leaving Soon.
            {' '}<Explain term="leaving_soon" />
          </li>
        )}
        {s.failures > 0 && (
          <li className="text-state-warn">
            <span className="num">{s.failures}</span> {s.failures === 1 ? 'write' : 'writes'} failed, retried next run.
          </li>
        )}
        {s.unresolved > 0 && (
          <li className="text-state-warn">
            <span className="num">{s.unresolved}</span> not matched in Plex.
            {' '}<Explain term="unresolved" />
          </li>
        )}
        {s.operator_keeps > 0 && (
          <li>
            <span className="num text-fg">{s.operator_keeps}</span> kept by your own exclusions. <Explain term="operator_keeps" />
          </li>
        )}
        {s.released_gone > 0 && (
          <li>
            <span className="num text-fg">{s.released_gone}</span> gone from Plex; exclusions released.
            {' '}<Explain term="released_gone" />
          </li>
        )}
        {(s.problems || []).map((problem) => (
          <li key={problem} className="break-words text-state-warn">{problem}</li>
        ))}
        {(s.warnings || []).map((warning) => (
          <li key={warning} className="break-words text-state-warn">
            <span className="text-fg-muted">Advice:</span> {warning}
          </li>
        ))}
      </ul>
    </section>
  );
}
