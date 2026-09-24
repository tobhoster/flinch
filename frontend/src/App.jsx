import React, { useCallback, useEffect, useState } from 'react';
import { Loader2, Play } from 'lucide-react';
import { loadStatus, loadItems, loadHistory, triggerRun } from './api.js';
import { Dot, Tabs, ago } from './ui.jsx';
import Overview from './Overview.jsx';
import MediaTable from './MediaTable.jsx';
import Settings from './Settings.jsx';

const TABS = [['overview', 'Overview'], ['series', 'Series'], ['movies', 'Movies'], ['settings', 'Settings']];

export default function App() {
  const [tab, setTab] = useState('overview');
  const [status, setStatus] = useState(null);
  const [items, setItems] = useState([]);
  const [history, setHistory] = useState([]);
  const [loading, setLoading] = useState(true);
  const [running, setRunning] = useState(false);

  const refresh = useCallback(async () => {
    const [st, it, hi] = await Promise.all([loadStatus(), loadItems(), loadHistory()]);
    setStatus(st); setItems(it); setHistory(hi); setLoading(false);
  }, []);

  useEffect(() => {
    refresh();
    const id = setInterval(refresh, 15000);
    return () => clearInterval(id);
  }, [refresh]);

  const onTrigger = async () => {
    setRunning(true);
    try {
      await triggerRun();
      // The daemon picks the request up within ~5s and the next poll shows it.
      await new Promise((r) => setTimeout(r, 6000));
      await refresh();
    } finally {
      setRunning(false);
    }
  };

  return (
    <div className="mx-auto max-w-[1200px] px-4 pb-12 pt-3 sm:px-6">
      <header>
        {/* Phones: brand and run button share the first row; status and services
            each take a full row below. From `sm` up it is one line in DOM order. */}
        <div className="flex flex-wrap items-center gap-x-4 gap-y-1 py-2">
          <span className="inline-flex items-center gap-2">
            <img src="/logo-dark.png" alt="" className="h-6 w-6" />
            <span className="text-sm font-semibold tracking-wide text-fg">FLINCH</span>
          </span>
          <div className="order-2 basis-full sm:order-none sm:basis-auto">
            <StatusLine status={status} loading={loading} />
          </div>
          <div className="order-3 basis-full sm:order-none sm:ml-auto sm:basis-auto">
            <Connections status={status} />
          </div>
          <button onClick={onTrigger} disabled={running} className="btn order-1 ml-auto sm:order-none sm:ml-0">
            {running ? <Loader2 size={13} className="animate-spin" /> : <Play size={13} />}
            {running ? 'Running…' : 'Trigger run'}
          </button>
        </div>
        {/* Tabs never wrap; on phones they scroll as their own strip. */}
        <div className="-mx-4 overflow-x-auto px-4 sm:mx-0 sm:px-0">
          <Tabs tabs={TABS} value={tab} onChange={setTab} />
        </div>
      </header>

      <main className="pt-4">
        {tab === 'overview' && <Overview status={status} items={items} history={history} loading={loading} />}
        {tab === 'series' && <MediaTable items={items} kind="season" />}
        {tab === 'movies' && <MediaTable items={items} kind="movie" />}
        {tab === 'settings' && <Settings status={status} />}
      </main>
    </div>
  );
}

function every(intervalS) {
  if (!intervalS) return 'manual only';
  return intervalS < 3600 ? `every ${Math.round(intervalS / 60)} min` : `every ${Math.round(intervalS / 3600)} h`;
}

const clock = (unix) => new Date(unix * 1000).toLocaleTimeString();

/**
 * Health of the run loop. A failed cycle keeps the last good snapshot, so the
 * failure is stated first; a scheduled loop that has not finished a cycle in two
 * intervals is overdue. Manual-only mode is never overdue.
 */
function runHealth(status, now) {
  const ranAt = status.ran_at_unix || 0;
  const lastAttempt = Math.max(ranAt, status.last_error_at || 0);
  if (status.last_error) {
    const good = ranAt ? `\nFigures shown are from the last good run, ${ago(Math.max(0, now - ranAt))}.` : '';
    return {
      tone: 'bad',
      text: `Last run failed ${ago(Math.max(0, now - lastAttempt))}`,
      title: `${status.last_error}${good}`,
    };
  }
  if (!ranAt) return { tone: null, text: 'No run yet' };
  const secs = Math.max(0, now - ranAt);
  const due = status.next_run_unix || ranAt + (status.interval_s || 0);
  if (status.interval_s > 0 && now - lastAttempt > 2 * status.interval_s) {
    return { tone: 'bad', text: `Overdue: last run ${ago(secs)}`, title: `Next run was due ${clock(due)}` };
  }
  return { tone: 'ok', text: `Last run ${ago(secs)}`, title: status.next_run_unix ? `Next run ${clock(status.next_run_unix)}` : undefined };
}

/** "● Last run 2 min ago · every 5 min · Enforced". The model has its own card. */
function StatusLine({ status, loading }) {
  if (loading) return <span className="text-xs text-fg-muted">Loading…</span>;
  if (!status) return <span className="text-xs text-fg-muted">No run recorded</span>;
  const health = runHealth(status, Math.floor(Date.now() / 1000));
  return (
    <span className="text-xs text-fg-muted">
      <span title={health.title} className={`inline-flex items-center gap-1.5 ${health.tone === 'bad' ? 'text-state-bad' : ''}`}>
        {health.tone && <Dot tone={health.tone} />}
        {health.text}
      </span>
      {' · '}{every(status.interval_s)}
      {' · '}
      {status.dry_run
        ? <span title="Plans only; nothing is scheduled for deletion">Dry run</span>
        : <span className="text-state-bad" title="Candidates are handed to Maintainerr for deletion">Enforced</span>}
    </span>
  );
}

const NOT_REPORTED = 'not reported in this snapshot';

/** Plex, from the daemon's own account of what it read. */
function plexHealth(ev) {
  if (!ev) return { state: 'unknown', why: NOT_REPORTED };
  if (!ev.plex_configured) return { state: 'down', why: 'not configured — nothing can be matched, so nothing is freed' };
  const gaps = [
    !ev.plex_items_ok && 'library items not read completely',
    !ev.plex_history_complete && 'watch history incomplete',
  ].filter(Boolean);
  return gaps.length ? { state: 'down', why: gaps.join('; ') } : { state: 'up', why: 'read fully' };
}

/** An *arr is down when the last run read nothing from it. */
const arrHealth = (count, one, many) => ((count ?? 0) > 0
  ? { state: 'up', why: `${count} ${count === 1 ? one : many} read` }
  : { state: 'down', why: `no ${many} read in the last run` });

/**
 * Services as the last run reported them. Down carries a red dot and the reason;
 * unknown (an older snapshot without the field) is muted and claims nothing.
 */
function Connections({ status }) {
  if (!status) return null;
  const ev = status.evidence;
  const sync = status.sync;
  const services = [
    { name: 'Radarr', ...arrHealth(status.movies, 'movie', 'movies') },
    { name: 'Sonarr', ...arrHealth(status.seasons, 'season', 'seasons') },
    {
      name: 'Maintainerr',
      ...(!sync ? { state: 'unknown', why: NOT_REPORTED }
        : sync.error ? { state: 'down', why: sync.error }
          : { state: 'up', why: sync.version ? `v${sync.version.replace(/^v/, '')}` : 'read' }),
    },
    { name: 'Plex', ...plexHealth(ev) },
  ];
  if (!ev) services.push({ name: 'Tautulli', state: 'unknown', why: NOT_REPORTED });
  else if (ev.tautulli_configured) {
    services.push({
      name: 'Tautulli',
      ...(ev.tautulli_complete ? { state: 'up', why: 'read fully' }
        : { state: 'down', why: 'history incomplete, not recording, or not kept for every user and library' }),
    });
  }
  return (
    <span className="inline-flex flex-wrap items-center text-xs text-fg-faint">
      {services.map((s, i) => (
        <span key={s.name} className="inline-flex items-center gap-1" title={`${s.name}: ${s.why}`}>
          {i > 0 && <span className="px-1">·</span>}
          {s.state === 'down' && <span aria-label="down" className="inline-block h-1.5 w-1.5 rounded-full bg-state-bad" />}
          <span className={s.state === 'up' ? '' : 'text-fg-muted'}>{s.name}</span>
        </span>
      ))}
    </span>
  );
}
