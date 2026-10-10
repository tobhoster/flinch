import React, { useEffect, useState } from 'react';
import { decideRequest, loadRequests } from './api.js';
import { SectionTitle, day } from './ui.jsx';

const LIMIT = 10;

/** One request with the decisions still open for it. */
function Row({ request, onDecided }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(null);
  const send = async (decision) => {
    setBusy(true);
    setError(null);
    try {
      onDecided(await decideRequest(request.id, decision));
    } catch (e) {
      setError(e.message);
    } finally {
      setBusy(false);
    }
  };
  const until = request.until ? ` until ${day(request.until)}` : '';
  return (
    <li className="break-words">
      <span className="text-fg">{request.title}</span>
      {' · '}{request.by}{' · '}<span className="num">{day(request.at)}</span>
      {request.kind === 'keep' && <span> · kept{until}</span>}
      {request.status === 'approved' && <span className="text-state-warn"> · approved: goes first when space is needed</span>}
      <span className="ml-2 inline-flex flex-wrap gap-2 text-xs">
        {request.status === 'pending' && (
          <>
            <button className="btn px-2" disabled={busy} onClick={() => send('approve')}>Approve</button>
            <button className="btn px-2" disabled={busy} onClick={() => send('deny')}>Deny</button>
          </>
        )}
        {(request.status === 'approved' || request.status === 'active') && (
          <button className="btn px-2" disabled={busy} onClick={() => send('cancel')}>{request.kind === 'keep' ? 'End keep' : 'Withdraw'}</button>
        )}
        {error && <span className="text-state-warn">{error}</span>}
      </span>
    </li>
  );
}

/**
 * Removal requests (`/api/requests`): what the household asked to remove,
 * waiting for approval; approved removals; keeps in force. Shown while the
 * status carries a `household` block.
 */
export default function HouseholdRequests({ household }) {
  const [book, setBook] = useState(null);
  const [error, setError] = useState(null);
  useEffect(() => {
    if (!household) return;
    loadRequests().then(setBook).catch((e) => setError(e.message));
  }, [household?.pending_removals, household?.approved_removals, household?.active_keeps]);
  if (!household) return null;
  const requests = book?.requests ?? [];
  const decided = (changed) => setBook((old) => ({ ...old, requests: old.requests.map((r) => (r.id === changed.id ? changed : r)) }));
  const open = requests.filter((r) => r.status === 'pending' || r.status === 'approved');
  const keeps = requests.filter((r) => r.status === 'active');
  const problems = household.problems ?? [];
  return (
    <section className="min-w-0 space-y-3">
      <SectionTitle hint={household.links ? undefined : 'links off'} term="household">Removal requests</SectionTitle>
      {open.length === 0 && <p className="text-fg-muted">No removal request is waiting.</p>}
      {open.length > 0 && (
        <ul className="space-y-1">
          {open.slice(0, LIMIT).map((r) => <Row key={r.id} request={r} onDecided={decided} />)}
          {open.length > LIMIT && <li className="text-fg-faint">+{open.length - LIMIT} more</li>}
        </ul>
      )}
      {keeps.length > 0 && (
        <div>
          <p className="text-fg-muted">Kept on request:</p>
          <ul className="space-y-1 text-fg-muted">
            {keeps.slice(0, LIMIT).map((r) => <Row key={r.id} request={r} onDecided={decided} />)}
            {keeps.length > LIMIT && <li className="text-fg-faint">+{keeps.length - LIMIT} more</li>}
          </ul>
        </div>
      )}
      {household.recipients > 0 && <p className="text-fg-faint">Requester messages reach {household.recipients} people.</p>}
      {problems.map((p) => <p key={p} className="text-state-warn">{p}</p>)}
      {error && <p className="text-state-warn">{error}</p>}
    </section>
  );
}
