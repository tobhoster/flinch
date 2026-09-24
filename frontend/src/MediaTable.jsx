import React, { useEffect, useMemo, useRef, useState } from 'react';
import { ArrowDown, ArrowUp, Copy, ExternalLink, Lock, PanelRightOpen, Search } from 'lucide-react';
import { Card, DropdownMenu, EmptyState, GiB, Sheet, Tooltip } from './ui.jsx';
import { Explain } from './Explain.jsx';
import { Disk, PlexIds, QualityTier } from './DetailCells.jsx';
import { loadSettings } from './api.js';

const isNum = (v) => v !== null && v !== undefined;
// Truncated, not rounded: a value just under the floor must never read as the floor.
const pct = (p) => `${Math.floor(p * 100 + 1e-9)}%`;

/** Sources that can testify to absence of playback (as opposed to only presence). */
const ABSENCE_SOURCES = ['plex', 'plex_show', 'export', 'tautulli_no_stream'];

const onDisk = (i) => (i.size_bytes || 0) > 0;

/**
 * Row filters. All but the last count only files that exist: a season Sonarr
 * monitors but never downloaded has nothing to decide, so it waits under
 * "Not on disk" instead of filling the default view.
 */
const FILTERS = [
  { key: 'on_disk', label: 'On disk', test: onDisk },
  { key: 'delete', label: 'Flagged', test: (i) => onDisk(i) && i.decision === 'delete' },
  { key: 'protected', label: 'Protected', test: (i) => onDisk(i) && i.protected },
  { key: 'rule', label: 'Rule-held', test: (i) => onDisk(i) && i.decision === 'keep' && !i.protected },
  {
    key: 'unwatched', label: 'Never played',
    test: (i) => onDisk(i) && ABSENCE_SOURCES.includes(i.watch_source) && !isNum(i.last_watched_days),
  },
  { key: 'missing', label: 'Not on disk', test: (i) => !onDisk(i) },
];

const SOURCE_LABEL = {
  plex: 'plex',
  plex_show: 'show-level',
  plex_history: 'history',
  tautulli: 'tautulli',
  tautulli_no_stream: 'tautulli',
  export: 'export',
};

const GUARD_LABEL = { 'newest-season': 'Newest aired season' };
const guardLabel = (g) => GUARD_LABEL[g] ?? g.replace(/[-_]/g, ' ').replace(/^./, (c) => c.toUpperCase());

/**
 * Watch state as text. Every contract state has its own branch:
 * no source → "no data"; dated → "Nd ago"; fraction ≥ 1 without a date →
 * "watched (no date)"; 0 < fraction < 1 → "partly played (x%)"; else "never played".
 */
function watchState(item) {
  const { last_watched_days: days, watch_source: source, watched_fraction: fraction } = item;
  if (!source) return 'no data';
  if (isNum(days)) return `${Math.floor(days)}d ago`;
  if (isNum(fraction) && fraction >= 1) return 'watched (no date)';
  if (isNum(fraction) && fraction > 0) return `partly played (${Math.min(99, Math.max(1, Math.round(fraction * 100)))}%)`;
  return 'never played';
}

const leaveDate = (unix) => new Date(unix * 1000).toLocaleDateString([], { month: 'short', day: 'numeric' });

/**
 * The decision as the row states it, from what Maintainerr holds first: a
 * membership leaves on its collection's schedule whatever this run decided.
 */
function decisionOf(item) {
  const announced = item.route === 'leaving_soon';
  if (item.handed_at && item.decision !== 'delete') {
    return {
      text: 'Still in collection',
      tone: 'text-state-bad',
      title: 'FLINCH keeps it, but it stays in its Maintainerr collection until an enforced run takes it back',
    };
  }
  if (item.decision === 'delete') {
    if (item.handed_at) {
      return {
        text: item.leaves_at ? `Leaves ${leaveDate(item.leaves_at)}` : 'Handed over',
        tone: 'text-state-warn',
        title: announced ? 'In Leaving Soon: Plex shows it, and playing it takes it back' : 'In its delete collection',
      };
    }
    if (announced) return { text: 'Leaving soon', tone: 'text-state-warn', title: 'Announced in Leaving Soon before it is deleted' };
    return { text: 'Candidate', tone: 'text-state-warn' };
  }
  if (item.protected) return { text: 'Protected', tone: 'text-fg' };
  if (item.decision === 'keep') return { text: 'Held', tone: 'text-fg-muted' };
  return { text: item.size_bytes ? 'No action' : 'Not on disk', tone: 'text-fg-faint' };
}

/**
 * One definition of what a row shows. The desktop table renders these as
 * cells; the phone card renders the same nodes as a key/value grid.
 * `key` is the sort field (null = not sortable).
 */
const COLUMNS = [
  { key: 'size_bytes', label: 'Size', right: true, render: (i) => <Size bytes={i.size_bytes} /> },
  { key: 'age_days', label: 'On disk', right: true, render: (i) => <Days value={i.age_days} /> },
  { key: 'forecast', label: 'P(safe)', term: 'p_safe', right: true, render: (i, ctx) => <PSafe item={i} floor={ctx.floor} /> },
  { key: 'last_watched_days', label: 'Watched', wide: true, render: (i) => <Watched item={i} /> },
  { key: null, label: 'Decision', render: (i) => <Decision item={i} /> },
];

const SORTABLE = [
  { key: 'title', label: 'Title' },
  ...COLUMNS.filter((c) => c.key).map(({ key, label }) => ({ key, label })),
];

export default function MediaTable({ items, kind }) {
  const [q, setQ] = useState('');
  const [filter, setFilter] = useState('on_disk');
  const [sort, setSort] = useState({ key: 'size_bytes', dir: -1 });
  const [sel, setSel] = useState(null);
  const [floor, setFloor] = useState(0.75);
  const inputRef = useRef(null);

  useEffect(() => {
    let live = true;
    // Without settings the default floor stays: it only tints the P(safe) column.
    loadSettings().then((s) => {
      const f = Number(s?.score_floor);
      if (live && Number.isFinite(f)) setFloor(f);
    }).catch(() => {});
    return () => { live = false; };
  }, []);

  useEffect(() => {
    const onKey = (e) => {
      const t = e.target;
      const typing = t instanceof HTMLElement && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName));
      if (e.key === '/' && !typing) { e.preventDefault(); inputRef.current?.focus(); }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  const searched = useMemo(() => {
    const needle = q.trim().toLowerCase();
    return items.filter((i) => i.kind === kind && (!needle || i.title.toLowerCase().includes(needle)));
  }, [items, kind, q]);

  const counts = useMemo(
    () => Object.fromEntries(FILTERS.map((f) => [f.key, searched.filter(f.test).length])),
    [searched],
  );

  const rows = useMemo(() => {
    const test = FILTERS.find((f) => f.key === filter).test;
    const missing = sort.dir === -1 ? -Infinity : Infinity;
    return searched.filter(test).sort((a, b) => {
      const av = a[sort.key] ?? missing;
      const bv = b[sort.key] ?? missing;
      if (typeof av === 'string' && typeof bv === 'string') return av.localeCompare(bv) * sort.dir;
      return av > bv ? sort.dir : av < bv ? -sort.dir : 0;
    });
  }, [searched, filter, sort]);

  const toggle = (key) => setSort((s) => ({ key, dir: s.key === key ? -s.dir : (key === 'title' ? 1 : -1) }));
  const ctx = { floor };
  const noun = kind === 'movie' ? 'movies' : 'seasons';
  const totalBytes = rows.reduce((sum, i) => sum + (i.size_bytes || 0), 0);

  const emptyState = <EmptyState title={`No matching ${noun}`} body="Change the filter or clear the search." />;
  const actions = (i) => actionsFor(i, () => setSel(i));

  return (
    <>
      <div className="mt-6 space-y-3">
        <div className="flex flex-wrap items-center gap-2">
          {/* sm+: segmented control. Phones: the same state through a native select. */}
          <div role="radiogroup" aria-label="Filter" className="hidden items-center rounded-md border border-line bg-ink-900 p-0.5 sm:inline-flex">
            {FILTERS.map((f) => (
              <button key={f.key} role="radio" aria-checked={filter === f.key} onClick={() => setFilter(f.key)}
                className={`rounded px-2.5 py-1 text-xs transition-colors ${filter === f.key ? 'bg-ink-700 text-fg' : 'text-fg-muted hover:text-fg'}`}>
                {f.label} <span className="num text-fg-faint">{counts[f.key]}</span>
              </button>
            ))}
          </div>
          <select aria-label="Filter" value={filter} onChange={(e) => setFilter(e.target.value)}
            className={`${selectCls} flex-1 sm:hidden`}>
            {FILTERS.map((f) => <option key={f.key} value={f.key}>{f.label} ({counts[f.key]})</option>)}
          </select>

          <span className="hidden text-xs text-fg-muted sm:ml-auto sm:inline">
            <span className="num">{rows.length}</span> {noun} · <span className="num">{GiB(totalBytes)}</span> GiB
          </span>
          <div className="relative w-full sm:w-56">
            <Search size={13} className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-fg-faint" />
            <input ref={inputRef} value={q} onChange={(e) => setQ(e.target.value)} placeholder="Search titles"
              className="min-h-[40px] w-full rounded-md border border-line bg-ink-900 py-1.5 pl-8 pr-8 text-[13px] placeholder:text-fg-faint focus:border-fg-faint focus:outline-none sm:min-h-0" />
            <kbd className="absolute right-2 hidden sm:block top-1/2 -translate-y-1/2 rounded border border-line px-1 text-[10px] text-fg-faint">/</kbd>
          </div>
        </div>

        {/* Phones: sort lives here; from md up the table header drives the same state. */}
        <div className="flex items-center gap-2 md:hidden">
          <select aria-label="Sort by" value={sort.key}
            onChange={(e) => setSort({ key: e.target.value, dir: e.target.value === 'title' ? 1 : -1 })}
            className={`${selectCls} flex-1`}>
            {SORTABLE.map((c) => <option key={c.key} value={c.key}>Sort: {c.label}</option>)}
          </select>
          <button className="btn min-h-[40px] min-w-[40px] justify-center px-2"
            onClick={() => setSort((s) => ({ ...s, dir: -s.dir }))}
            aria-label={sort.dir === 1 ? 'Ascending' : 'Descending'}>
            {sort.dir === 1 ? <ArrowUp size={14} /> : <ArrowDown size={14} />}
          </button>
          <span className="shrink-0 text-xs text-fg-muted">
            <span className="num">{rows.length}</span> · <span className="num">{GiB(totalBytes)}</span> GiB
          </span>
        </div>

        <Card className="overflow-hidden">
          <table className="hidden w-full text-[13px] md:table">
            <thead>
              <tr className="border-b border-line text-left">
                <Th sortKey="title" sort={sort} onSort={toggle}>Title</Th>
                {COLUMNS.map((c) => (
                  <Th key={c.label} sortKey={c.key} sort={sort} onSort={toggle} right={c.right} term={c.term}>{c.label}</Th>
                ))}
                <th className="w-10" aria-label="Actions" />
              </tr>
            </thead>
            <tbody>
              {rows.length === 0 && (
                <tr><td colSpan={COLUMNS.length + 2} className="p-4">{emptyState}</td></tr>
              )}
              {rows.map((i) => (
                <tr key={i.id} tabIndex={0} onClick={() => setSel(i)}
                  onKeyDown={(e) => e.key === 'Enter' && setSel(i)}
                  className="cursor-pointer border-b border-line-soft last:border-0 hover:bg-ink-800/60 focus:bg-ink-800/60 focus:outline-none">
                  <td className="px-3 py-1"><TitleCell item={i} /></td>
                  {COLUMNS.map((c) => (
                    <td key={c.label} className={`whitespace-nowrap px-3 py-1 ${c.right ? 'text-right' : ''}`}>{c.render(i, ctx)}</td>
                  ))}
                  <td className="px-2 py-1 text-right" onClick={(e) => e.stopPropagation()}>
                    <DropdownMenu items={actions(i)} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>

          {/* Phones: one card per row, same columns in the same order. */}
          <ul className="divide-y divide-line-soft md:hidden">
            {rows.length === 0 && <li className="p-3">{emptyState}</li>}
            {rows.map((i) => (
              <li key={i.id} onClick={() => setSel(i)} className="cursor-pointer p-3 active:bg-ink-800/60">
                <div className="flex items-start gap-2">
                  <div className="min-w-0 flex-1"><TitleCell item={i} /></div>
                  <div onClick={(e) => e.stopPropagation()}><DropdownMenu items={actions(i)} /></div>
                </div>
                <dl className="mt-2 grid grid-cols-3 gap-x-3 gap-y-2 text-[13px]">
                  {COLUMNS.map((c) => (
                    <div key={c.label} className={`min-w-0 ${c.wide ? 'col-span-2' : ''}`}>
                      <dt className="text-xs text-fg-muted">{c.label}</dt>
                      <dd className="break-words">{c.render(i, ctx)}</dd>
                    </div>
                  ))}
                </dl>
              </li>
            ))}
          </ul>
        </Card>
      </div>

      {/* Outside the spacing container so the fixed overlay inherits no margins. */}
      <Sheet open={!!sel} onClose={() => setSel(null)} title={sel?.title || ''}>
        {sel && <ItemDetail item={sel} floor={floor} actions={actions(sel)} />}
      </Sheet>
    </>
  );
}

const selectCls = 'min-h-[40px] rounded-md border border-line bg-ink-900 px-2 text-[13px] text-fg focus:border-fg-faint focus:outline-none';

function Th({ children, sortKey, sort, onSort, right, term }) {
  const active = sortKey && sort.key === sortKey;
  const cls = `px-3 py-2 text-xs font-normal text-fg-muted ${right ? 'text-right' : ''}`;
  if (!sortKey) return <th className={cls}>{children}</th>;
  return (
    <th className={cls} aria-sort={active ? (sort.dir === 1 ? 'ascending' : 'descending') : 'none'}>
      <span className="inline-flex items-center gap-1.5">
        <button onClick={() => onSort(sortKey)} className={`inline-flex items-center gap-1 hover:text-fg ${active ? 'text-fg' : ''}`}>
          {children}
          {active && (sort.dir === 1 ? <ArrowUp size={11} /> : <ArrowDown size={11} />)}
        </button>
        {term && <Explain term={term} />}
      </span>
    </th>
  );
}

function Poster({ item, large = false }) {
  const [failed, setFailed] = useState(false);
  const size = large ? 'h-[90px] w-[60px]' : 'h-9 w-6';
  if (!item.poster_url || failed) {
    return <div className={`${size} shrink-0 rounded-sm border border-line bg-ink-800`} aria-hidden />;
  }
  return (
    <img src={item.poster_url} alt="" loading="lazy" onError={() => setFailed(true)}
      className={`${size} shrink-0 rounded-sm border border-line object-cover`} />
  );
}

/** Identifying meta line: year · quality · season · episodes. */
function metaOf(item) {
  return [
    item.year,
    item.season_label,
    item.episodes ? `${item.episodes} eps` : null,
    item.quality,
  ].filter(Boolean).join(' · ');
}

function TitleCell({ item }) {
  const meta = metaOf(item);
  return (
    <div className="flex min-w-0 items-center gap-2.5">
      <Poster item={item} />
      <div className="min-w-0 flex-1">
        <div className="truncate-1 text-fg" title={item.title}>{item.title}</div>
        {meta && <div className="truncate-1 text-xs text-fg-faint">{meta}</div>}
      </div>
    </div>
  );
}

function Size({ bytes }) {
  return <span className="num">{GiB(bytes)} <span className="text-fg-faint">GiB</span></span>;
}

function Days({ value }) {
  if (!isNum(value)) return <span className="text-fg-faint">—</span>;
  return <span className="num text-fg-muted">{Math.round(value)}d</span>;
}

/**
 * The forecast, from evidence alone. A rule that keeps the item is marked
 * beside it, never folded into the number; with no evidence there is nothing
 * to forecast. Snapshots from before the split carry only `p_safe`.
 */
function PSafe({ item, floor }) {
  const p = isNum(item.forecast) ? item.forecast : item.p_safe;
  if (!isNum(p)) return <span className="text-fg-faint">—</span>;
  if (!item.watch_source) {
    return (
      <Tooltip text="No watch evidence, so there is nothing to forecast from.">
        <span className="text-fg-faint">—</span>
      </Tooltip>
    );
  }
  if (item.hard_guard) {
    return (
      <Tooltip text={`Kept by rule (${guardLabel(item.hard_guard).toLowerCase()}), whatever the forecast says.`}>
        <span className="num inline-flex items-center gap-0.5 text-fg-muted">
          <Lock size={10} aria-label="kept by rule" />{pct(p)}
        </span>
      </Tooltip>
    );
  }
  return <span className={`num ${p >= floor ? 'text-state-warn' : ''}`}>{pct(p)}</span>;
}

function Watched({ item }) {
  const state = watchState(item);
  const source = SOURCE_LABEL[item.watch_source] ?? item.watch_source;
  return (
    <span className={item.watch_source ? 'text-fg-muted' : 'text-fg-faint'}>
      <span className={isNum(item.last_watched_days) ? 'num' : ''}>{state}</span>
      {source && <span className="ml-1.5 text-xs text-fg-faint">{source}</span>}
    </span>
  );
}

function Decision({ item }) {
  const { text, tone, title } = decisionOf(item);
  return <span className={tone} title={title}>{text}</span>;
}

/**
 * Link to the item in Radarr/Sonarr. Their routers key items by `titleSlug`
 * (`/series/for-all-mankind`, `/movie/872585`); a numeric id resolves to
 * "that series cannot be found". The apps sit beside Flinch under the same
 * parent domain (flinch.example.com → sonarr.example.com). Returns null when
 * there is no slug or no sibling host to point at (local dev).
 */
function arrUrl(item) {
  const app = item.id.startsWith('radarr-') ? 'radarr' : 'sonarr';
  const labels = window.location.hostname.split('.');
  if (!item.title_slug || labels.length < 2) return null;
  const host = [app, ...labels.slice(1)].join('.');
  const slug = encodeURIComponent(item.title_slug);
  return `https://${host}${app === 'radarr' ? `/movie/${slug}` : `/series/${slug}`}`;
}

function actionsFor(item, openDetails) {
  const url = arrUrl(item);
  const app = item.id.startsWith('radarr-') ? 'Radarr' : 'Sonarr';
  return [
    { label: 'Details', icon: <PanelRightOpen size={13} />, onSelect: openDetails },
    ...(url ? [{ label: `Open in ${app}`, icon: <ExternalLink size={13} />, onSelect: () => window.open(url, '_blank', 'noopener') }] : []),
    { label: 'Copy library id', icon: <Copy size={13} />, onSelect: () => navigator.clipboard?.writeText(item.id) },
  ];
}

/** Factual reasons: the hard guard first, then the decision reason, then scoring evidence. */
function whyOf(item) {
  const lines = [];
  const seen = new Set();
  const add = (s) => {
    const k = s.trim().toLowerCase();
    if (k && !seen.has(k)) { seen.add(k); lines.push(s.charAt(0).toUpperCase() + s.slice(1)); }
  };
  if (item.hard_guard) {
    const g = guardLabel(item.hard_guard);
    lines.push(`${g} — kept by rule, whatever the forecast says`);
    seen.add(g.toLowerCase());
  }
  if (item.reason) add(item.reason);
  for (const r of item.reasons || []) add(r);
  if (item.protected) add('Maintainerr exclusion written');
  if (item.handed_at) {
    add(`${item.route === 'leaving_soon' ? 'In Leaving Soon' : 'In its delete collection'} since ${leaveDate(item.handed_at)}`);
  } else if (item.route === 'leaving_soon') {
    add('Nobody finished it, so it goes to Leaving Soon first and playing it takes it back');
  }
  return lines;
}

function ItemDetail({ item, floor, actions }) {
  const { text, tone, title } = decisionOf(item);
  const meta = metaOf(item);
  const why = whyOf(item);
  const rows = [
    ['Decision', <span className={tone} title={title}>{text}</span>],
    ['P(safe)', isNum(item.forecast ?? item.p_safe)
      ? <><PSafe item={item} floor={floor} /> <span className="text-fg-faint">floor {pct(floor)}</span></>
      : <span className="text-fg-faint">—</span>],
    ['Watched', <Watched item={item} />],
    ['Size', <Size bytes={item.size_bytes} />],
    ['On disk', <Days value={item.age_days} />],
    ['Disk', <Disk volume={item.volume} />],
    ['Plex', <PlexIds plex={item.plex} />],
    ['Quality tier', <QualityTier inflow={item.inflow} />],
    ['Library id', <span className="num break-all text-fg-muted">{item.id}</span>],
  ];
  return (
    <div className="space-y-5 text-[13px]">
      <div className="flex gap-3">
        <Poster item={item} large />
        <div className="min-w-0 space-y-2">
          {meta && <div className="text-fg-muted">{meta}</div>}
          <div className="flex flex-wrap gap-2">
            {actions.slice(1).map((a) => (
              <button key={a.label} className="btn min-h-[40px] px-2.5 py-1 text-xs sm:min-h-0" onClick={a.onSelect}>
                {a.icon}{a.label}
              </button>
            ))}
          </div>
        </div>
      </div>

      <dl className="grid grid-cols-[96px_minmax(0,1fr)] gap-y-1.5">
        {rows.map(([k, v]) => (
          <React.Fragment key={k}>
            <dt className="flex items-center gap-1.5 text-fg-muted">{k}{TERMS[k] && <Explain term={TERMS[k]} />}</dt>
            <dd>{v}</dd>
          </React.Fragment>
        ))}
      </dl>

      <section>
        <h4 className="mb-1.5 text-sm font-medium text-fg">Why</h4>
        {why.length === 0
          ? <p className="text-fg-faint">No reasons recorded.</p>
          : (
            <ul className="list-disc space-y-1 pl-4 text-fg-muted marker:text-fg-faint">
              {why.map((w) => <li key={w}>{w}</li>)}
            </ul>
          )}
      </section>

      <details className="text-xs">
        <summary className="cursor-pointer text-fg-muted hover:text-fg">Raw JSON</summary>
        <pre className="mt-2 overflow-x-auto rounded-md border border-line bg-ink-950 p-3 text-[11.5px] leading-relaxed text-fg-muted">
          {JSON.stringify(item, null, 2)}
        </pre>
      </details>
    </div>
  );
}

/** Detail rows whose label has a glossary entry. */
const TERMS = { 'P(safe)': 'p_safe', Watched: 'evidence', 'Quality tier': 'quality_tier' };
