import React, { useEffect, useMemo, useRef, useState } from 'react';
import { forceCollide, forceLink, forceManyBody, forceSimulation, forceX, forceY } from 'd3-force';
import { HardDrive, Loader2, Maximize2, Search, Shield, User, X } from 'lucide-react';
import { loadGraph } from '../api.js';
import { Card, EmptyState, GiB, day, pct } from '../ui.jsx';
import { DEFAULT_FILTERS, EDGE_STYLE, neighbours, toGraph } from './model.js';
import { palette } from '../../tailwind.config.js';
import RuleOverride from './RuleOverride.jsx';

// The Graph tab: items, the people who watch and request them, the disks they
// fill and what holds them, as a force layout. Size is bytes on disk, colour is
// P(watch), a ring marks what the plan takes. Click anything to inspect it.

const W = 1100;
const H = 680;

const PLAN_LABEL = { evict: 'In the plan: deleted', leaving: 'In the plan: Leaving Soon first', keep: 'Kept', none: 'Not a candidate' };
const ADVICE_LABEL = { keep_original: 'keep at this quality', downgrade_quality: 'downgrade to the compact profile', eligible_for_eviction: 'evict rather than downgrade' };

function useSimulation(graph) {
  const [, setTick] = useState(0);
  const sim = useRef(null);
  useEffect(() => {
    const { nodes, links } = graph;
    const s = forceSimulation(nodes)
      .force('link', forceLink(links).id((n) => n.id).distance((l) => EDGE_STYLE[l.kind]?.distance ?? 80).strength((l) => EDGE_STYLE[l.kind]?.strength ?? 0.3))
      .force('charge', forceManyBody().strength((n) => (n.type === 'disk' ? -900 : n.type === 'item' ? -40 : -120)))
      .force('collide', forceCollide((n) => n.r + 2))
      .force('x', forceX(W / 2).strength(0.04))
      .force('y', forceY(H / 2).strength(0.06))
      .alphaDecay(0.035);
    let frame = 0;
    s.on('tick', () => {
      // One React render per animation frame, however often d3 ticks.
      if (!frame) frame = requestAnimationFrame(() => { frame = 0; setTick((t) => t + 1); });
    });
    sim.current = s;
    return () => { s.stop(); cancelAnimationFrame(frame); };
  }, [graph]);
  return sim;
}

/** Wheel to zoom about the cursor, drag the background to pan. */
function useViewport(svg) {
  const [view, setView] = useState({ x: 0, y: 0, k: 1 });
  useEffect(() => {
    const el = svg.current;
    if (!el) return undefined;
    const onWheel = (e) => {
      e.preventDefault();
      const rect = el.getBoundingClientRect();
      const px = ((e.clientX - rect.left) / rect.width) * W;
      const py = ((e.clientY - rect.top) / rect.height) * H;
      setView((v) => {
        const k = Math.min(4, Math.max(0.3, v.k * (e.deltaY < 0 ? 1.12 : 1 / 1.12)));
        return { k, x: px - ((px - v.x) * k) / v.k, y: py - ((py - v.y) * k) / v.k };
      });
    };
    el.addEventListener('wheel', onWheel, { passive: false });
    return () => el.removeEventListener('wheel', onWheel);
  }, [svg]);
  return [view, setView];
}

const toSvg = (svg, e, view) => {
  const rect = svg.getBoundingClientRect();
  return { x: (((e.clientX - rect.left) / rect.width) * W - view.x) / view.k, y: (((e.clientY - rect.top) / rect.height) * H - view.y) / view.k };
};

function NodeShape({ node, dim, selected, onDown, onEnter, onLeave }) {
  const common = { onPointerDown: onDown, onPointerEnter: onEnter, onPointerLeave: onLeave, style: { cursor: 'pointer' }, opacity: dim ? 0.15 : 1 };
  const ring = selected ? palette.fg.DEFAULT : null;
  if (node.type === 'disk') {
    const w = node.r * 2.6;
    const h = node.r * 1.3;
    const fill = Math.min(1, node.disk.utilization || 0);
    return (
      <g transform={`translate(${node.x},${node.y})`} {...common}>
        <rect x={-w / 2} y={-h / 2} width={w} height={h} rx={4} fill={palette.ink[800]} stroke={ring || node.color} strokeWidth={selected ? 2 : 1.25} />
        <rect x={-w / 2 + 3} y={h / 2 - 7} width={(w - 6) * fill} height={4} rx={1} fill={node.color} />
        <text y={-1} textAnchor="middle" fontSize={9} fill={palette.fg.DEFAULT} className="tabular-nums">{pct(node.disk.utilization)}</text>
      </g>
    );
  }
  if (node.type === 'guard') {
    const s = node.r;
    return (
      <g transform={`translate(${node.x},${node.y})`} {...common}>
        <path d={`M0 ${-s} L${s} 0 L0 ${s} L${-s} 0 Z`} fill={palette.ink[800]} stroke={ring || node.color} strokeWidth={1.5} />
      </g>
    );
  }
  if (node.type === 'user') {
    return (
      <g transform={`translate(${node.x},${node.y})`} {...common}>
        <circle r={node.r} fill={palette.ink[800]} stroke={ring || node.color} strokeWidth={1.5} />
        <text y={3} textAnchor="middle" fontSize={9} fill={node.color}>{node.user.name.slice(0, 1).toUpperCase()}</text>
      </g>
    );
  }
  const taken = node.plan === 'evict' || node.plan === 'leaving';
  return (
    <g transform={`translate(${node.x},${node.y})`} {...common}>
      {taken && <circle r={node.r + 3} fill="none" stroke={node.plan === 'evict' ? palette.state.bad : palette.state.warn} strokeWidth={1.5} strokeDasharray={node.plan === 'leaving' ? '3 2' : null} />}
      <circle r={node.r} fill={node.color} fillOpacity={0.85} stroke={ring || palette.ink[950]} strokeWidth={selected ? 2 : 1} />
    </g>
  );
}

const label = (node) => (node.type === 'item' ? node.item.title : node.type === 'user' ? node.user.name : node.type === 'disk' ? node.disk.path : node.guard.label);

function Row({ k, children }) {
  return (
    <div className="grid grid-cols-[96px_minmax(0,1fr)] gap-2 py-0.5">
      <span className="text-fg-faint">{k}</span>
      <span className="min-w-0 break-words text-fg">{children}</span>
    </div>
  );
}

function linkedTo(graph, id, kind, side = 'target') {
  return graph.links.filter((l) => l.kind === kind && (side === 'target' ? l.source.id === id : l.target.id === id)).map((l) => (side === 'target' ? l.target : l.source));
}

function Inspector({ node, graph, onSelect, onClose }) {
  const Chip = ({ n }) => (
    <button className="mr-1 mt-1 inline-flex max-w-full items-center rounded border border-line px-1.5 py-0.5 text-[12px] text-fg hover:bg-ink-700" onClick={() => onSelect(n.id)}>
      <span className="truncate">{label(n)}</span>
    </button>
  );
  const list = (nodes) => (nodes.length ? nodes.map((n) => <Chip key={n.id} n={n} />) : <span className="text-fg-faint">none</span>);
  let body;
  if (node.type === 'item') {
    const it = node.item;
    body = (
      <>
        <Row k="Plan"><span className={node.plan === 'evict' ? 'text-state-bad' : node.plan === 'leaving' ? 'text-state-warn' : ''}>{PLAN_LABEL[node.plan]}</span></Row>
        <Row k="Why">{it.reason || '—'}</Row>
        <Row k="Size"><span className="num">{GiB(it.size_bytes)} GiB</span></Row>
        <Row k="P(watch)"><span className="num">{pct(it.p_watch)}</span></Row>
        <Row k="Regret"><span className="num">{it.regret == null ? '—' : it.regret.toFixed(3)}</span></Row>
        <Row k="Quality">{it.quality || '—'}{it.advice && <span className="text-fg-muted"> · advice: {ADVICE_LABEL[it.advice] || it.advice}</span>}</Row>
        {it.explanation && <Row k="Advice">{it.explanation}</Row>}
        {it.leaves_at && <Row k="Leaves"><span className="num">{day(it.leaves_at)}</span></Row>}
        <Row k="Disk">{it.volume || '—'}</Row>
        <Row k="Path"><span className="font-mono text-[11px]">{it.path || '—'}</span></Row>
        {it.tags?.length > 0 && <Row k="Tags">{it.tags.join(', ')}</Row>}
        <Row k="Watched by">{list(linkedTo(graph, node.id, 'WATCHED_BY'))}</Row>
        <Row k="Requested by">{list(linkedTo(graph, node.id, 'REQUESTED_BY'))}</Row>
        <Row k="Held by">{list(linkedTo(graph, node.id, 'BLOCKED_BY_TAG'))}</Row>
        <div className="mt-3 border-t border-line-soft pt-3">
          <RuleOverride item={it} />
        </div>
      </>
    );
  } else if (node.type === 'user') {
    body = (
      <>
        <Row k="Plays"><span className="num">{node.user.plays}</span></Row>
        <Row k="Watched">{list(linkedTo(graph, node.id, 'WATCHED_BY', 'source'))}</Row>
        <Row k="Requested">{list(linkedTo(graph, node.id, 'REQUESTED_BY', 'source'))}</Row>
      </>
    );
  } else if (node.type === 'disk') {
    const d = node.disk;
    const on = linkedTo(graph, node.id, 'STAGED_ON', 'source');
    const taken = on.filter((n) => n.plan === 'evict' || n.plan === 'leaving');
    body = (
      <>
        <Row k="Used"><span className="num">{pct(d.utilization)} · {GiB(d.used_bytes)} of {GiB(d.capacity_bytes)} GiB</span></Row>
        <Row k="To free"><span className="num">{GiB(d.target_reclaim_bytes)} GiB</span></Row>
        <Row k="Plan takes"><span className="num">{taken.length} items · {GiB(taken.reduce((s, n) => s + n.item.size_bytes, 0))} GiB</span></Row>
        <Row k="Largest">{list([...on].sort((a, b) => b.item.size_bytes - a.item.size_bytes).slice(0, 8))}</Row>
      </>
    );
  } else {
    body = <Row k="Holds">{list(linkedTo(graph, node.id, 'BLOCKED_BY_TAG', 'source'))}</Row>;
  }
  return (
    <div className="space-y-1 text-[12px]">
      <div className="flex items-start gap-2">
        <div className="min-w-0 flex-1 text-[13px] font-medium text-fg">{label(node)}</div>
        <button className="btn px-1.5" aria-label="Close" onClick={onClose}><X size={12} /></button>
      </div>
      {body}
    </div>
  );
}

function Legend({ data }) {
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-[11px] text-fg-muted">
      <span className="inline-flex items-center gap-1">
        <span className="h-2 w-16 rounded-sm" style={{ background: `linear-gradient(90deg, ${palette.state.bad}, ${palette.state.warn}, ${palette.state.ok})` }} />
        P(watch) <span className="num">0 → 15% → 50%</span>
      </span>
      <span>size = GiB on disk</span>
      <span className="inline-flex items-center gap-1"><span className="h-2.5 w-2.5 rounded-full border border-state-bad" /> deleted</span>
      <span className="inline-flex items-center gap-1"><span className="h-2.5 w-2.5 rounded-full border border-dashed border-state-warn" /> Leaving Soon</span>
      {Object.entries(EDGE_STYLE).map(([kind, s]) => (
        <span key={kind} className="inline-flex items-center gap-1">
          <svg width="18" height="6"><line x1="0" y1="3" x2="18" y2="3" stroke={s.color} strokeWidth="1.5" strokeDasharray={s.dash} /></svg>{s.label}
        </span>
      ))}
      <span className="inline-flex items-center gap-1"><User size={11} /> person</span>
      <span className="inline-flex items-center gap-1"><HardDrive size={11} /> disk</span>
      <span className="inline-flex items-center gap-1"><Shield size={11} /> safeguard</span>
      {data && !data.sources?.tautulli && <span className="text-state-warn">No Tautulli history: watch links are missing, not absent</span>}
    </div>
  );
}

export default function RelationshipGraph() {
  const [data, setData] = useState(undefined);
  const [error, setError] = useState(null);
  const [filters, setFilters] = useState(DEFAULT_FILTERS);
  const [selected, setSelected] = useState(null);
  const [hover, setHover] = useState(null);
  const [query, setQuery] = useState('');
  const svg = useRef(null);
  const drag = useRef(null);
  const [view, setView] = useViewport(svg);

  useEffect(() => {
    loadGraph().then(setData).catch((err) => setError(String(err.message || err)));
  }, []);

  const graph = useMemo(() => toGraph(data, filters), [data, filters]);
  const sim = useSimulation(graph);
  const focus = hover || selected;
  const near = useMemo(() => (focus ? neighbours(graph.links, focus) : null), [graph, focus]);

  const disks = data?.disks || [];
  const byId = useMemo(() => new Map(graph.nodes.map((n) => [n.id, n])), [graph]);

  const centre = (id) => {
    const n = byId.get(id);
    if (!n) return;
    setSelected(id);
    setView((v) => ({ ...v, x: W / 2 - n.x * v.k, y: H / 2 - n.y * v.k }));
  };

  const onFind = (e) => {
    e.preventDefault();
    const q = query.trim().toLowerCase();
    const hit = graph.nodes.find((n) => label(n).toLowerCase().includes(q));
    if (hit) centre(hit.id);
  };

  const onNodeDown = (node) => (e) => {
    e.stopPropagation();
    e.currentTarget.ownerSVGElement.setPointerCapture(e.pointerId);
    drag.current = { node, moved: false };
    node.fx = node.x; node.fy = node.y;
    sim.current?.alphaTarget(0.2).restart();
  };
  const onBackgroundDown = (e) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current = { pan: { x: e.clientX, y: e.clientY, view } };
  };
  const onMove = (e) => {
    const d = drag.current;
    if (!d) return;
    if (d.node) {
      const p = toSvg(svg.current, e, view);
      d.node.fx = p.x; d.node.fy = p.y; d.moved = true;
    } else if (d.pan) {
      const rect = svg.current.getBoundingClientRect();
      setView({ ...d.pan.view, x: d.pan.view.x + ((e.clientX - d.pan.x) / rect.width) * W, y: d.pan.view.y + ((e.clientY - d.pan.y) / rect.height) * H });
      d.moved = d.moved || Math.abs(e.clientX - d.pan.x) + Math.abs(e.clientY - d.pan.y) > 3;
    }
  };
  const onUp = () => {
    const d = drag.current;
    drag.current = null;
    if (!d) return;
    if (d.node) {
      d.node.fx = null; d.node.fy = null;
      sim.current?.alphaTarget(0);
      if (!d.moved) setSelected(d.node.id);
    } else if (!d.moved) setSelected(null);
  };

  if (error) return <EmptyState icon={X} title="The graph could not be loaded" body={error} />;
  if (data === undefined) return <p className="flex items-center gap-2 text-fg-muted"><Loader2 size={13} className="animate-spin" /> Loading the graph…</p>;
  if (!data || !data.items?.length) return <EmptyState icon={HardDrive} title="Nothing to draw yet" body="The graph joins what the daemon publishes each cycle; it appears after the first run." />;

  const selectedNode = selected ? byId.get(selected) : null;
  const shown = graph.nodes.filter((n) => n.type === 'item');
  const taken = shown.filter((n) => n.plan === 'evict' || n.plan === 'leaving');

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2 text-[12px]">
        <form onSubmit={onFind} className="inline-flex items-center gap-1">
          <input aria-label="Find a node" className="input w-48" placeholder="Find title, person, disk" value={query} onChange={(e) => setQuery(e.target.value)} />
          <button className="btn px-2" aria-label="Find"><Search size={13} /></button>
        </form>
        <select aria-label="Kind" className="rounded-md border border-line bg-ink-900 px-2 py-1 text-fg" value={filters.kind} onChange={(e) => setFilters({ ...filters, kind: e.target.value })}>
          <option value="all">Movies and seasons</option><option value="movie">Movies</option><option value="season">Seasons</option>
        </select>
        <select aria-label="Disk" className="rounded-md border border-line bg-ink-900 px-2 py-1 text-fg" value={filters.disk} onChange={(e) => setFilters({ ...filters, disk: e.target.value })}>
          <option value="all">Every disk</option>
          {disks.map((d) => <option key={d.path} value={d.path}>{d.path}</option>)}
        </select>
        <label className="inline-flex items-center gap-1.5 text-fg-muted"><input type="checkbox" checked={filters.inPlanOnly} onChange={(e) => setFilters({ ...filters, inPlanOnly: e.target.checked })} /> In the plan only</label>
        <label className="inline-flex items-center gap-1.5 text-fg-muted"><input type="checkbox" checked={!filters.candidatesOnly} onChange={(e) => setFilters({ ...filters, candidatesOnly: !e.target.checked })} /> Include non-candidates</label>
        <span className="ml-auto text-fg-muted num">{shown.length} items · {taken.length} in the plan · {GiB(taken.reduce((s, n) => s + n.item.size_bytes, 0))} GiB</span>
        <button className="btn px-2" title="Reset the view" aria-label="Reset the view" onClick={() => setView({ x: 0, y: 0, k: 1 })}><Maximize2 size={13} /></button>
      </div>
      <div className="grid gap-3 lg:grid-cols-[minmax(0,1fr)_300px]">
        <Card className="overflow-hidden">
          <svg ref={svg} viewBox={`0 0 ${W} ${H}`} className="block h-[640px] w-full touch-none select-none bg-ink-950"
            onPointerDown={onBackgroundDown} onPointerMove={onMove} onPointerUp={onUp} onPointerCancel={onUp}>
            <g transform={`translate(${view.x},${view.y}) scale(${view.k})`}>
              {graph.links.map((l) => {
                const s = EDGE_STYLE[l.kind];
                const dim = near && !(near.has(l.source.id) && near.has(l.target.id));
                return <line key={`${l.source.id}-${l.target.id}-${l.kind}`} x1={l.source.x} y1={l.source.y} x2={l.target.x} y2={l.target.y}
                  stroke={s.color} strokeWidth={1} strokeDasharray={s.dash} opacity={dim ? 0.06 : 0.55} />;
              })}
              {graph.nodes.map((n) => (
                <NodeShape key={n.id} node={n} dim={near && !near.has(n.id)} selected={n.id === selected}
                  onDown={onNodeDown(n)} onEnter={() => setHover(n.id)} onLeave={() => setHover(null)} />
              ))}
              {focus && byId.get(focus) && (() => {
                const n = byId.get(focus);
                return <text x={n.x} y={n.y - n.r - 6} textAnchor="middle" fontSize={11} fill={palette.fg.DEFAULT} paintOrder="stroke" stroke={palette.ink[950]} strokeWidth={3}>{label(n)}</text>;
              })()}
            </g>
          </svg>
        </Card>
        <Card className="max-h-[640px] overflow-y-auto p-3">
          {selectedNode
            ? <Inspector node={selectedNode} graph={graph} onSelect={centre} onClose={() => setSelected(null)} />
            : <p className="text-[12px] text-fg-faint">Click a title, a person, a disk or a safeguard to inspect it. Drag to move, scroll to zoom, drag the background to pan.</p>}
        </Card>
      </div>
      <Legend data={data} />
    </div>
  );
}
