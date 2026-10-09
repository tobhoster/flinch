import React, { useCallback, useEffect, useRef, useState } from 'react';
import { searchItems } from './api.js';
import { Explain } from './Explain.jsx';

/** Matches a search by meaning shows: enough to scan, few enough to mean something. */
const LIMIT = 50;

/**
 * Search by meaning for one table (`/api/search`). Each query costs the
 * server a model run, so one goes out per Enter, never per keystroke; a newer
 * query or leaving the table cancels the one in flight. `hits` maps the
 * matched ids to their score, best first; null until a search succeeds.
 */
export function useMeaningSearch(kind) {
  const [hits, setHits] = useState(null);
  const [unranked, setUnranked] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(null);
  const pending = useRef(null);

  const clear = useCallback(() => {
    pending.current?.abort();
    pending.current = null;
    setHits(null);
    setBusy(false);
    setError(null);
  }, []);
  useEffect(() => () => pending.current?.abort(), []);

  /** Resolves true once `query`'s matches are in `hits`. */
  const run = useCallback(async (query) => {
    pending.current?.abort();
    const controller = new AbortController();
    pending.current = controller;
    setBusy(true);
    setError(null);
    try {
      const found = await searchItems(query, kind, LIMIT, controller.signal);
      if (controller.signal.aborted) return false;
      setHits(new Map(found.results.map((hit) => [hit.id, hit.score])));
      setUnranked(found.unranked || 0);
      return true;
    } catch (e) {
      if (controller.signal.aborted) return false;
      setHits(null);
      setError(e.message);
      return false;
    } finally {
      if (pending.current === controller) {
        pending.current = null;
        setBusy(false);
      }
    }
  }, [kind]);

  return { hits, unranked, busy, error, run, clear };
}

/** Under the toolbar: searching, why it failed, or what the matches are. */
export function MeaningStatus({ search, noun, byRelevance, onSortByRelevance }) {
  if (search.busy) return <p className="text-xs text-fg-muted">Searching by meaning…</p>;
  if (search.error) return <p role="alert" className="text-xs text-state-bad">{search.error}</p>;
  if (!search.hits) return <p className="text-xs text-fg-muted">Describe what you are looking for and press Enter.</p>;
  return (
    <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-fg-muted">
      <span className="inline-flex items-center gap-1">
        The <span className="num">{search.hits.size}</span> {noun} closest in meaning <Explain term="meaning_search" />
      </span>
      {search.unranked > 0 && (
        <span>· <span className="num">{search.unranked}</span> not embedded yet, so not searched</span>
      )}
      {!byRelevance && (
        <button className="text-fg underline-offset-2 hover:underline" onClick={onSortByRelevance}>Sort by relevance</button>
      )}
    </p>
  );
}
