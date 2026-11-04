// `/api/graph` as nodes and links for the force layout, with the filters the
// Graph tab offers. Pure: the component owns the simulation and the drawing.

import { palette } from '../palette.js';

// `strength` shapes the layout: items cluster on their disk; people and
// safeguards pull lightly, so one person who watches everything cannot fold
// the whole library into a single knot.
export const EDGE_STYLE = {
  WATCHED_BY: { color: palette.state.ok, label: 'watched by', dash: null, distance: 90, strength: 0.02 },
  REQUESTED_BY: { color: palette.state.info, label: 'requested by', dash: '4 3', distance: 90, strength: 0.02 },
  STAGED_ON: { color: palette.line.DEFAULT, label: 'on disk', dash: null, distance: 70, strength: 0.7 },
  BLOCKED_BY_TAG: { color: palette.state.warn, label: 'held by', dash: '2 3', distance: 50, strength: 0.15 },
};

const hex = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));
const mix = (a, b, t) => {
  const [x, y] = [hex(a), hex(b)];
  return `rgb(${x.map((v, i) => Math.round(v + (y[i] - v) * t)).join(',')})`;
};

/**
 * Demand as colour: red where nobody is likely to play it, amber around 15%,
 * green from 50% up. Unknown P(watch) is grey.
 */
export function demandColor(p) {
  if (p == null) return palette.fg.faint;
  if (p <= 0.15) return mix(palette.state.bad, palette.state.warn, p / 0.15);
  return mix(palette.state.warn, palette.state.ok, Math.min(1, (p - 0.15) / 0.35));
}

const GIB = 2 ** 30;
/** Radius by size on disk: area grows with bytes, capped so one box set cannot hide the rest. */
export const itemRadius = (bytes) => Math.min(26, 4 + Math.sqrt((bytes || 0) / GIB) * 1.6);

/** Where the plan stands on an item, in the words the tables use. */
export function planState(item) {
  if (item.decision === 'delete') return item.route === 'leaving_soon' ? 'leaving' : 'evict';
  if (item.decision === 'keep') return 'keep';
  return 'none';
}

export const DEFAULT_FILTERS = { kind: 'all', disk: 'all', inPlanOnly: false, candidatesOnly: true };

/**
 * The visible graph: items passing `filters`, and only the users, disks and
 * guards something visible links to.
 */
export function toGraph(data, filters = DEFAULT_FILTERS) {
  if (!data) return { nodes: [], links: [] };
  const items = (data.items || []).filter((item) => {
    if (filters.candidatesOnly && planState(item) === 'none') return false;
    if (filters.inPlanOnly && !['evict', 'leaving'].includes(planState(item))) return false;
    if (filters.kind !== 'all' && item.kind !== filters.kind) return false;
    if (filters.disk !== 'all' && item.volume !== filters.disk) return false;
    return true;
  });
  const visible = new Set(items.map((item) => `item:${item.id}`));
  const links = (data.edges || []).filter((edge) => visible.has(edge.source)).map((edge) => ({ ...edge }));
  const linked = new Set(links.map((link) => link.target));

  const nodes = items.map((item) => ({ id: `item:${item.id}`, type: 'item', item, r: itemRadius(item.size_bytes), color: demandColor(item.p_watch), plan: planState(item) }));
  for (const user of data.users || []) {
    const id = `user:${user.name}`;
    if (linked.has(id)) nodes.push({ id, type: 'user', user, r: 9, color: palette.state.info });
  }
  for (const disk of data.disks || []) {
    const id = `disk:${disk.path}`;
    if (linked.has(id)) nodes.push({ id, type: 'disk', disk, r: 22, color: diskColor(disk.utilization, data.target_utilization) });
  }
  for (const guard of data.guards || []) {
    const id = `guard:${guard.id}`;
    if (linked.has(id)) nodes.push({ id, type: 'guard', guard, r: 10, color: palette.state.warn });
  }
  return { nodes, links };
}

/** A disk over its target is red, within five points of it amber. */
export function diskColor(utilization, target) {
  if (utilization == null || target == null) return palette.fg.muted;
  if (utilization >= target) return palette.state.bad;
  if (utilization >= target - 0.05) return palette.state.warn;
  return palette.state.ok;
}

/** Every node `id` links to, either direction. */
export function neighbours(links, id) {
  const out = new Set([id]);
  for (const link of links) {
    const [s, t] = [link.source.id ?? link.source, link.target.id ?? link.target];
    if (s === id) out.add(t);
    if (t === id) out.add(s);
  }
  return out;
}
