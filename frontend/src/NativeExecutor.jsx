import React, { useState } from 'react';
import { GiB, SectionTitle, day } from './ui.jsx';
import { Explain } from './Explain.jsx';
import { restoreItem } from './api.js';

/** This run's native actions, as `[count, words]` pairs; zero counts are dropped. */
const tallies = (n, dryRun) => [
  [n.deleted, `${dryRun ? 'would be ' : ''}deleted (${GiB(n.deleted_bytes)} GiB)`],
  [n.announced, `${dryRun ? 'would go' : 'went'} to Leaving Soon (${GiB(n.announced_bytes)} GiB)`],
  [n.withdrawn, dryRun ? 'would be taken back' : 'taken back'],
  [n.deferred, 'deferred by caps'],
].filter(([count]) => count > 0);

/** At most this many held items are named; the rest are counted. */
const HELD_SHOWN = 5;

/** One recent delete with its undo: queued, done, or a button. */
function Recent({ item, asked, onRestore }) {
  const state = item.restored_at ? 'Restored'
    : item.restore_pending || asked === 'queued' ? 'Restore queued'
      : null;
  return (
    <li className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
      <span className="min-w-0 break-words">
        <span className="text-fg">{item.title}</span>
        <span className="text-fg-faint"> · {GiB(item.bytes)} GiB · {day(item.deleted_at)}{item.announced ? ' · after Leaving Soon' : ''}</span>
        {item.seerr && <span className="block text-fg-faint">{item.seerr}</span>}
        {asked && asked !== 'queued' && <span className="block text-state-bad">{asked}</span>}
      </span>
      {state
        ? <span className="text-fg-faint">{state}</span>
        : <button className="btn px-2 text-xs" onClick={() => onRestore(item.id)}>Restore</button>}
    </li>
  );
}

/**
 * The native executor from `status.native`: what this run deleted, announced
 * and held, the Leaving Soon shelf, and the deletes of the last 30 days with
 * an undo (monitor and search again, on the daemon's next run).
 */
export default function NativeExecutor({ native: n, dryRun }) {
  const [asked, setAsked] = useState({});
  const restore = async (id) => {
    try {
      await restoreItem(id);
      setAsked((current) => ({ ...current, [id]: 'queued' }));
    } catch (error) {
      setAsked((current) => ({ ...current, [id]: error.message }));
    }
  };
  const done = tallies(n, dryRun);
  const held = n.held || [];
  return (
    <section className="min-w-0">
      <SectionTitle hint="native" term="executor">Executor</SectionTitle>
      <ul className="space-y-1 text-fg-muted">
        <li>
          {dryRun && <>Dry run: <span className="num text-fg">{n.simulated ?? 0}</span> {n.simulated === 1 ? 'write' : 'writes'} shown, none sent. </>}
          {done.length === 0
            ? 'Nothing to delete this run.'
            : done.map(([count, words], i) => (
              <React.Fragment key={words}>{i > 0 && ' · '}<span className="num text-fg">{count}</span> {words}</React.Fragment>
            ))}
        </li>
        {n.failures > 0 && (
          <li className="text-state-warn">
            <span className="num">{n.failures}</span> {n.failures === 1 ? 'write' : 'writes'} failed, retried next run.
          </li>
        )}
        {held.slice(0, HELD_SHOWN).map((line) => <li key={line} className="break-words">Held: {line}</li>)}
        {held.length > HELD_SHOWN && <li>…and {held.length - HELD_SHOWN} more held.</li>}
        {(n.problems || []).map((problem) => <li key={problem} className="break-words text-state-warn">{problem}</li>)}
      </ul>
      {(n.leaving || []).length > 0 && (
        <>
          <h4 className="mt-3 text-fg">Leaving Soon <Explain term="leaving_soon" /></h4>
          <ul className="mt-1 space-y-1 text-fg-muted">
            {n.leaving.map((item) => (
              <li key={item.id} className="break-words">
                <span className="text-fg">{item.title}</span> · {GiB(item.bytes)} GiB · leaves {day(item.until)} unless someone plays it
              </li>
            ))}
          </ul>
        </>
      )}
      {(n.recent || []).length > 0 && (
        <>
          <h4 className="mt-3 text-fg">Deleted, last 30 days <Explain term="restore" /></h4>
          <ul className="mt-1 space-y-1.5 text-fg-muted">
            {n.recent.map((item) => (
              <Recent key={`${item.id}-${item.deleted_at}`} item={item} asked={asked[item.id]} onRestore={restore} />
            ))}
          </ul>
        </>
      )}
    </section>
  );
}
