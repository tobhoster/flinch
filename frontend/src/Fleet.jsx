import React, { useState } from 'react';
import { Activity, Ban, FlaskConical, Loader2 } from 'lucide-react';
import { removeQueueItem, testIndexer } from './api.js';
import { Card, Dot, EmptyState, GiB, SectionTitle, ago, pct } from './ui.jsx';

// The *arr fleet as the daemon last read it (state/arr-fleet-status.json):
// a one-line strip under the header on every tab, and the Fleet tab with the
// health matrix, the stuck queue, Prowlarr's indexers and FLINCH's own fleet
// actions. Disk pressure is FLINCH's own measurement (status.json), because
// Sonarr leaves its NFS library mounts out of its disk report.

const APP_NAME = { sonarr: 'Sonarr', radarr: 'Radarr', prowlarr: 'Prowlarr' };
const name = (i) => `${APP_NAME[i.app] || i.app}${i.instance ? ` ${i.instance}` : ''}`;
const key = (i) => `${i.app}${i.instance ? `@${i.instance}` : ''}`;
/** "3 min ago" for a unix time. */
const since = (unix) => ago(Math.max(0, Math.floor(Date.now() / 1000) - unix));

const REASON = {
  no_space: { label: 'waits for space', tone: 'text-state-warn', why: 'Nothing is wrong with the release: the library disk it imports to is below Sonarr/Radarr’s free-space floor. It imports by itself once FLINCH frees room; removing it would only download it again.' },
  unimportable: { label: 'cannot import', tone: 'text-state-bad', why: 'Missing or fake files, a sample only, an unknown title, or a failed repair: it will never import. Remove it and blocklist the release so a different one is grabbed.' },
  stalled_download: { label: 'stalled', tone: 'text-state-warn', why: 'Downloading with no progress for longer than the stall limit.' },
  other: { label: 'flagged', tone: 'text-fg-muted', why: 'The app flags it for a reason FLINCH does not judge.' },
};

const unixOf = (iso) => (iso ? Math.floor(Date.parse(iso) / 1000) : null);
const warnings = (i) => (i.health || []).filter((h) => h.type === 'warning' || h.type === 'error');

/** ok, warn or bad for one instance: unreachable is bad, any warning or stuck item is warn. */
export function instanceTone(i) {
  if (!i.reachable) return 'bad';
  if ((i.health || []).some((h) => h.type === 'error')) return 'bad';
  if (warnings(i).length || i.queue?.stalled?.length) return 'warn';
  return 'ok';
}

/** The one-line telemetry strip; click to open the Fleet tab. */
export function FleetStrip({ fleet, status, onOpen }) {
  const volumes = status?.capacity?.volumes || [];
  const target = status?.capacity?.target_utilization;
  if (!fleet?.instances?.length && !volumes.length) return null;
  const failing = (fleet?.indexers || []).filter((x) => x.failing);
  return (
    <button onClick={onOpen} title="Open the Fleet tab"
      className="flex w-full flex-wrap items-center gap-x-4 gap-y-1 rounded-md border border-line-soft bg-ink-900 px-3 py-1.5 text-left text-[12px] text-fg-muted hover:bg-ink-800">
      {(fleet?.instances || []).map((i) => (
        <span key={key(i)} className="inline-flex items-center gap-1.5">
          <Dot tone={instanceTone(i)} />
          <span className="text-fg">{name(i)}</span>
          {i.version && <span className="num text-fg-faint">{i.version}</span>}
          {i.queue && <span className="num">q {i.queue.total}{i.queue.stalled.length > 0 && <span className="text-state-warn"> · {i.queue.stalled.length} stuck</span>}</span>}
          {warnings(i).length > 0 && <span className="num text-state-warn">{warnings(i).length} warn</span>}
        </span>
      ))}
      {fleet?.indexers?.length > 0 && (
        <span className="num">{fleet.indexers.length} indexers{failing.length > 0 && <span className="text-state-bad"> · {failing.length} failing</span>}</span>
      )}
      {volumes.length > 0 && <span className="h-3 w-px bg-line" aria-hidden />}
      {volumes.map((v) => (
        <span key={v.path} className="inline-flex items-center gap-1">
          <span className="font-mono text-[11px] text-fg-faint">{v.path}</span>
          <span className={`num ${target != null && v.utilization >= target ? 'text-state-bad' : 'text-fg'}`}>{pct(v.utilization)}</span>
        </span>
      ))}
      {fleet?.checked_at > 0 && <span className="ml-auto text-fg-faint">read {since(fleet.checked_at)}</span>}
    </button>
  );
}

function useAction() {
  const [state, setState] = useState({});
  const run = async (id, fn) => {
    setState((s) => ({ ...s, [id]: { busy: true } }));
    try {
      const { queued } = await fn();
      setState((s) => ({ ...s, [id]: { note: `queued (${queued}); runs on the next cycle` } }));
    } catch (err) {
      setState((s) => ({ ...s, [id]: { error: String(err.message || err) } }));
    }
  };
  return [state, run];
}

function ActionNote({ s }) {
  if (!s) return null;
  if (s.error) return <div className="text-[11px] text-state-bad">{s.error}</div>;
  if (s.note) return <div className="text-[11px] text-state-ok">{s.note}</div>;
  return null;
}

const th = 'px-2 py-1.5 text-left text-[11px] font-medium uppercase tracking-wide text-fg-faint';
const td = 'px-2 py-1.5 align-top';

function HealthMatrix({ fleet }) {
  return (
    <Card className="overflow-x-auto">
      <table className="w-full text-[12px]">
        <thead className="border-b border-line-soft"><tr>
          <th className={th}>App</th><th className={th}>Version</th><th className={th}>Up since</th>
          <th className={`${th} text-right`}>Queue</th><th className={`${th} text-right`}>Stuck</th><th className={th}>Health</th>
        </tr></thead>
        <tbody className="divide-y divide-line-soft">
          {fleet.instances.map((i) => (
            <tr key={key(i)}>
              <td className={td}><span className="inline-flex items-center gap-1.5"><Dot tone={instanceTone(i)} /><span className="text-fg">{name(i)}</span></span>
                <div className="font-mono text-[11px] text-fg-faint">{i.url}</div></td>
              <td className={td}><span className="num text-fg">{i.version || '—'}</span>{i.branch && <span className="text-fg-faint"> · {i.branch}</span>}{i.os && <span className="text-fg-faint"> · {i.os}</span>}</td>
              <td className={`${td} text-fg-muted`}>{i.started_at ? since(unixOf(i.started_at)) : '—'}</td>
              <td className={`${td} num text-right`}>{i.queue ? i.queue.total : '—'}</td>
              <td className={`${td} num text-right ${i.queue?.stalled?.length ? 'text-state-warn' : ''}`}>{i.queue ? i.queue.stalled.length : '—'}</td>
              <td className={td}>
                {!i.reachable && <div className="text-state-bad">does not answer{i.error ? `: ${i.error}` : ''}</div>}
                {warnings(i).length === 0 && i.reachable && <span className="text-fg-faint">no warnings</span>}
                {warnings(i).map((h) => (
                  <div key={h.source + h.message} className={h.type === 'error' ? 'text-state-bad' : 'text-state-warn'}>
                    {h.message}{h.wiki_url && <> · <a className="underline decoration-line underline-offset-2 hover:text-fg" href={h.wiki_url} target="_blank" rel="noreferrer">wiki</a></>}
                  </div>
                ))}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </Card>
  );
}

function StuckQueue({ fleet, status }) {
  const [state, run] = useAction();
  const free = new Map((status?.capacity?.volumes || []).map((v) => [v.path, v]));
  const rows = fleet.instances.flatMap((i) => (i.queue?.stalled || []).map((item) => ({ i, item })));
  if (!rows.length) return <p className="text-[12px] text-fg-faint">Nothing stuck in any queue.</p>;
  return (
    <Card className="overflow-x-auto">
      <table className="w-full text-[12px]">
        <thead className="border-b border-line-soft"><tr>
          <th className={th}>Release</th><th className={th}>Why</th><th className={`${th} text-right`}>Size</th><th className={th}>Imports to</th><th className={th}>Since</th><th className={th} />
        </tr></thead>
        <tbody className="divide-y divide-line-soft">
          {rows.map(({ i, item }) => {
            const r = REASON[item.reason] || REASON.other;
            const id = `${key(i)}-${item.id}`;
            const disk = item.target_path && [...free.values()].filter((v) => item.target_path === v.path || item.target_path.startsWith(`${v.path}/`)).sort((a, b) => b.path.length - a.path.length)[0];
            return (
              <tr key={id}>
                <td className={td}><div className="max-w-[360px] truncate text-fg" title={item.title}>{item.title}</div>
                  <div className="text-fg-faint">{name(i)}{item.series_or_movie ? ` · ${item.series_or_movie}` : ''}</div></td>
                <td className={td}><span className={r.tone} title={r.why}>{r.label}</span>
                  <div className="max-w-[280px] text-[11px] text-fg-faint">{(item.messages || []).join(' · ')}</div></td>
                <td className={`${td} num text-right`}>{GiB(item.size_bytes)} GiB</td>
                <td className={td}><span className="font-mono text-[11px]">{item.target_path || '—'}</span>
                  {disk && <div className="num text-[11px] text-fg-faint">{pct(disk.utilization)} used · {GiB(Math.max(0, disk.capacity_bytes - disk.used_bytes))} GiB free</div>}</td>
                <td className={`${td} text-fg-muted`}>{item.since ? since(item.since) : '—'}</td>
                <td className={`${td} text-right`}>
                  {item.removable ? (
                    <button className="btn" disabled={state[id]?.busy} onClick={() => run(id, () => removeQueueItem(key(i), item.id))}
                      title="Remove from the download client and blocklist the release, so a different release is grabbed">
                      {state[id]?.busy ? <Loader2 size={13} className="animate-spin" /> : <Ban size={13} />} Remove + blocklist
                    </button>
                  ) : <span className="text-[11px] text-fg-faint" title={r.why}>leave it</span>}
                  <ActionNote s={state[id]} />
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </Card>
  );
}

function Indexers({ fleet }) {
  const [state, run] = useAction();
  const rows = [...(fleet.indexers || [])].sort((a, b) => Number(b.failing) - Number(a.failing) || a.name.localeCompare(b.name));
  if (!rows.length) return <p className="text-[12px] text-fg-faint">No Prowlarr read: set PROWLARR_URL and PROWLARR_API_KEY.</p>;
  return (
    <Card className="overflow-x-auto">
      <table className="w-full text-[12px]">
        <thead className="border-b border-line-soft"><tr>
          <th className={th}>Indexer</th><th className={th}>State</th><th className={`${th} text-right`}>Success</th><th className={`${th} text-right`}>Avg ms</th>
          <th className={`${th} text-right`}>Queries</th><th className={`${th} text-right`}>Grabs</th><th className={th} />
        </tr></thead>
        <tbody className="divide-y divide-line-soft">
          {rows.map((x) => {
            const id = `idx-${x.id}`;
            return (
              <tr key={x.id}>
                <td className={td}><span className="text-fg">{x.name}</span> <span className="text-fg-faint">{x.protocol}</span></td>
                <td className={td}>
                  {!x.enabled ? <span className="text-fg-faint">disabled</span>
                    : x.failing ? <span className="text-state-bad">failing{x.initial_failure ? ` since ${since(unixOf(x.initial_failure))}` : ''}{x.disabled_till ? ` · paused until ${new Date(x.disabled_till).toLocaleString()}` : ''}</span>
                      : <span className="text-state-ok">ok</span>}
                </td>
                <td className={`${td} num text-right`}>{x.success_rate == null ? '—' : pct(x.success_rate)}</td>
                <td className={`${td} num text-right`}>{x.avg_response_ms || '—'}</td>
                <td className={`${td} num text-right`}>{x.queries}</td>
                <td className={`${td} num text-right`}>{x.grabs}</td>
                <td className={`${td} text-right`}>
                  <button className="btn" disabled={state[id]?.busy} onClick={() => run(id, () => testIndexer(x.id))} title="Run Prowlarr's own test of this indexer">
                    {state[id]?.busy ? <Loader2 size={13} className="animate-spin" /> : <FlaskConical size={13} />} Test
                  </button>
                  <ActionNote s={state[id]} />
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </Card>
  );
}

function Actions({ fleet }) {
  const actions = [...(fleet.actions || [])].reverse();
  if (!actions.length) return <p className="text-[12px] text-fg-faint">FLINCH has not acted on the fleet yet.</p>;
  return (
    <ul className="space-y-1 text-[12px]">
      {actions.slice(0, 20).map((a) => (
        <li key={`${a.at}-${a.target}-${a.kind}`} className="flex gap-3">
          <span className="w-20 shrink-0 text-fg-faint">{since(a.at)}</span>
          <span className="text-fg">{a.kind.replace('_', ' ')}</span>
          <span className="min-w-0 truncate text-fg-muted">{a.target}</span>
          <span className={a.result === 'done' ? 'text-state-ok' : a.result.startsWith('failed') ? 'text-state-bad' : 'text-fg-faint'}>{a.result}</span>
        </li>
      ))}
    </ul>
  );
}

export default function Fleet({ fleet, status }) {
  if (!fleet?.instances?.length) {
    return <EmptyState icon={Activity} title="No fleet read yet" body="The daemon reads Sonarr, Radarr and Prowlarr each cycle; this tab fills after its first run with the fleet monitor on (Settings: fleet.enabled)." />;
  }
  const dry = status?.dry_run;
  return (
    <div className="space-y-6">
      <section className="space-y-2">
        <SectionTitle hint={`read ${since(fleet.checked_at)}`}>Health</SectionTitle>
        <HealthMatrix fleet={fleet} />
      </section>
      <section className="space-y-2">
        <SectionTitle hint={dry ? 'dry run: removals are printed, not sent' : 'removals run on the next cycle'}>Stuck in a queue</SectionTitle>
        <StuckQueue fleet={fleet} status={status} />
      </section>
      <section className="space-y-2">
        <SectionTitle hint="Prowlarr">Indexers</SectionTitle>
        <Indexers fleet={fleet} />
      </section>
      <section className="space-y-2">
        <SectionTitle hint="newest first">Fleet actions</SectionTitle>
        <Actions fleet={fleet} />
      </section>
    </div>
  );
}
