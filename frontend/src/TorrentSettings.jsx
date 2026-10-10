import React from 'react';
import { Plus, X } from 'lucide-react';

// The torrent clients of Settings > Torrents. FLINCH reads them for seed
// goals and hardlinks; a password never enters settings.json, only the name
// of the flinch-arrd environment variable holding it.

const KINDS = [
  ['qbittorrent', 'qBittorrent'],
  ['transmission', 'Transmission'],
];

const PLACEHOLDER = {
  qbittorrent: 'http://qbittorrent:8080',
  transmission: 'http://transmission:9091',
};

const NEW_CLIENT = { kind: 'qbittorrent', url: '', username: '', password_env: '' };

const inputClass = 'input min-h-[40px] w-full sm:min-h-0';
const selectClass = 'min-h-[40px] rounded-md border border-line bg-ink-900 px-2 text-[13px] text-fg focus:border-fg-faint focus:outline-none sm:min-h-0';

/** The editable list of clients; `onChange` gets the whole new list. */
export function TorrentClients({ clients, onChange }) {
  const set = (index, patch) => onChange(clients.map((client, i) => (i === index ? { ...client, ...patch } : client)));
  return (
    <div className="w-full space-y-3">
      {clients.map((client, i) => (
        <div key={i} className="space-y-2 rounded-md border border-line-soft p-3">
          <div className="flex flex-wrap items-center gap-2">
            <select id={`torrents-kind-${i}`} aria-label="Client kind" className={selectClass} value={client.kind}
              onChange={(e) => set(i, { kind: e.target.value })}>
              {KINDS.map(([key, label]) => <option key={key} value={key}>{label}</option>)}
            </select>
            <button className="btn ml-auto px-2" aria-label="Remove client" onClick={() => onChange(clients.filter((_, j) => j !== i))}>
              <X size={13} />
            </button>
          </div>
          <input aria-label="URL" className={inputClass} placeholder={PLACEHOLDER[client.kind]} autoComplete="off" spellCheck={false}
            value={client.url} onChange={(e) => set(i, { url: e.target.value })} />
          <div className="flex flex-wrap gap-2">
            <input aria-label="Username" className={`${inputClass} sm:w-40`} placeholder="Username" autoComplete="off" spellCheck={false}
              value={client.username} onChange={(e) => set(i, { username: e.target.value })} />
            <input aria-label="Password variable" className={`${inputClass} sm:w-72`} placeholder="Password variable, e.g. FLINCH_QBIT_PASSWORD"
              autoComplete="off" spellCheck={false} value={client.password_env} onChange={(e) => set(i, { password_env: e.target.value.toUpperCase() })} />
          </div>
        </div>
      ))}
      <button className="btn px-2.5 text-xs" disabled={clients.length >= 8} onClick={() => onChange([...clients, { ...NEW_CLIENT }])}>
        <Plus size={13} /> Add client
      </button>
    </div>
  );
}

/** `from → to` prefix rows: how a client's or the *arrs' paths read inside flinch-arrd. */
export function PathMap({ rows, onChange }) {
  const set = (index, patch) => onChange(rows.map((row, i) => (i === index ? { ...row, ...patch } : row)));
  return (
    <div className="w-full space-y-2">
      {rows.map((row, i) => (
        <div key={i} className="flex flex-wrap items-center gap-2">
          <input id={`torrents-from-${i}`} aria-label="Client path" className={`${inputClass} sm:w-56`} placeholder="/data/torrents"
            spellCheck={false} value={row.from} onChange={(e) => set(i, { from: e.target.value })} />
          <span className="text-fg-faint">→</span>
          <input aria-label="Path in flinch-arrd" className={`${inputClass} sm:w-56`} placeholder="/mnt/torrents"
            spellCheck={false} value={row.to} onChange={(e) => set(i, { to: e.target.value })} />
          <button className="btn px-2" aria-label="Remove mapping" onClick={() => onChange(rows.filter((_, j) => j !== i))}>
            <X size={13} />
          </button>
        </div>
      ))}
      <button className="btn px-2.5 text-xs" onClick={() => onChange([...rows, { from: '', to: '' }])}>
        <Plus size={13} /> Add mapping
      </button>
    </div>
  );
}

const gib = (bytes) => `${(bytes / 2 ** 30).toFixed(1)} GiB`;

/** One line on the last run's read, from status.json `torrents`. */
export function torrentSummary(status) {
  const torrents = status?.torrents;
  if (!torrents) return 'Not read yet: add a client and save; the next run reads it.';
  const clients = torrents.clients.map((c) => (c.error ? `${c.host}: ${c.error}` : `${c.host}: ${c.torrents} torrents`));
  const held = [
    [torrents.below_goal, 'below their seed goal'],
    [torrents.held_by_torrent, 'hardlinked to a torrent that stays'],
    [torrents.links_unverified, 'with unverified hardlinks'],
    [torrents.client_unreadable, 'on an unreadable client'],
  ].filter(([h]) => h.items > 0).map(([h, label]) => `${h.items} ${label} (${gib(h.bytes)})`);
  const spared = torrents.spared?.items ? ` ${torrents.spared.items} below the desired ratio drawn on last (${gib(torrents.spared.bytes)}).` : '';
  return `Last run: ${clients.join('; ')}. Kept ${held.length ? held.join(', ') : 'nothing for its torrents'}.${spared}`;
}

/** Trimmed rows for the PUT body; blank variable names are sent as none. */
export function torrentsPayload(torrents) {
  return {
    ...torrents,
    clients: torrents.clients.map((client) => ({
      ...client, url: client.url.trim(), username: client.username.trim(), password_env: client.password_env.trim() || null,
    })),
    path_map: torrents.path_map.map((row) => ({ from: row.from.trim(), to: row.to.trim() })).filter((row) => row.from || row.to),
  };
}
