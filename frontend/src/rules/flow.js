// The rule list (settings.json `rules`, crates/flinch-archive/src/rules.rs) as
// a flow and back. The flow is a view of the list, never a second store: the
// canvas edits the same `rules` the form and YAML views edit, and Settings
// saves them only after a preview, as before.
//
// One rule is one chain:
//
//   trigger ─▶ condition ─▶ … ─▶ [safeguards] ─▶ action ─▶ [route]
//
// - trigger: when the rule can act, which follows from its effect: keeps hold
//   on every plan; prefer/must evict act only on a disk with a target. It
//   carries the rule's name and switch.
// - condition: one scope condition; a chain's conditions must all hold (AND).
// - safeguards (evict rules only, not editable): what no rule overrides — keep
//   tag, your keeps, the grace period, partway, torrents still seeding.
// - action: the rule's effect.
// - route (evict rules only, not editable): how a picked item leaves —
//   finished goes straight to deletion, unfinished through Leaving Soon.
//
// Positions are derived (one row per rule), so nothing but the rules is saved.

import { EFFECTS, LISTS, RANGES, clean, effectOf } from '../RuleSettings.jsx';

export const ROW = 150;
const COL = 230;

const EVICT = new Set(['prefer_evict', 'must_evict']);
export const isEvict = (effect) => EVICT.has(effect?.type);

/** Every condition the scope can hold: `{ key, label, shape }`. */
export const CONDITIONS = [
  { key: 'kind', label: 'Kind', shape: 'kind' },
  { key: 'ids', label: 'Titles (FLINCH ids)', shape: 'list', placeholder: 'radarr-7, sonarr-12-s3' },
  ...LISTS.map(([key, label, placeholder, numeric]) => ({ key, label, shape: numeric ? 'numbers' : 'list', placeholder })),
  { key: 'played', label: 'Played', shape: 'played' },
  ...RANGES.map(([key, label, unit]) => ({ key, label, shape: 'range', unit })),
];

const CONDITION_KEYS = CONDITIONS.map((c) => c.key);
export const conditionDef = (key) => CONDITIONS.find((c) => c.key === key);
export const effectName = (type) => EFFECTS.find(([key]) => key === type)?.[1] || type;

/** JSON with object keys sorted, so two rule lists compare by meaning, not key order. */
export function canonical(value) {
  return JSON.stringify(value, (_, v) => (v && typeof v === 'object' && !Array.isArray(v) ? Object.fromEntries(Object.entries(v).sort(([a], [b]) => a.localeCompare(b))) : v));
}

/** The node ids of rule `index`'s chain. */
const ids = (index) => ({
  trigger: `r${index}-trigger`,
  cond: (key) => `r${index}-c-${key}`,
  guard: `r${index}-guard`,
  action: `r${index}-action`,
  route: `r${index}-route`,
});

const edge = (source, target) => ({ id: `${source}->${target}`, source, target });

/**
 * The flow of `rules`. `context` names live numbers the info nodes show:
 * `{ target, leavingSoonDays }`.
 */
export function rulesToFlow(rules, context = {}) {
  const nodes = [];
  const edges = [];
  rules.forEach((rule, index) => {
    const id = ids(index);
    const y = index * ROW;
    let x = 0;
    const chain = [];
    const push = (node) => {
      nodes.push({ ...node, position: { x, y } });
      chain.push(node.id);
      x += COL;
    };
    push({ id: id.trigger, type: 'trigger', data: { name: rule.name, enabled: rule.enabled !== false, evict: isEvict(rule.effect), ...context } });
    const scope = clean(rule.scope);
    for (const key of CONDITION_KEYS) {
      if (scope[key] !== undefined) push({ id: id.cond(key), type: 'condition', data: { key, value: scope[key] } });
    }
    if (isEvict(rule.effect)) push({ id: id.guard, type: 'guard', data: {}, deletable: false });
    push({ id: id.action, type: 'action', data: { effect: rule.effect } });
    if (isEvict(rule.effect)) push({ id: id.route, type: 'route', data: { ...context }, deletable: false });
    for (let i = 1; i < chain.length; i += 1) edges.push(edge(chain[i - 1], chain[i]));
  });
  return { nodes, edges };
}

/** Which node types may follow which: the chain's grammar. */
const NEXT = {
  trigger: ['condition', 'guard', 'action'],
  condition: ['condition', 'guard', 'action'],
  guard: ['action'],
  action: ['route'],
  route: [],
};

/**
 * Whether `connection` keeps every chain a single line in the grammar's order:
 * an allowed step, no second way out of the source or into the target, and no
 * loop back into its own chain.
 */
export function canConnect(connection, nodes, edges) {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const source = byId.get(connection.source);
  const target = byId.get(connection.target);
  if (!source || !target || source.id === target.id) return false;
  if (!NEXT[source.type]?.includes(target.type)) return false;
  if (edges.some((e) => e.source === source.id || e.target === target.id)) return false;
  // Following the target's chain must not lead back to the source.
  const out = new Map(edges.map((e) => [e.source, e.target]));
  for (let at = target.id, steps = 0; at && steps <= nodes.length; at = out.get(at), steps += 1) {
    if (at === source.id) return false;
  }
  return true;
}

/**
 * The rule list the flow says, or the reasons it says none. Rules come out in
 * the triggers' top-to-bottom order, which is the list's order.
 */
export function flowToRules(nodes, edges) {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const out = new Map(edges.map((e) => [e.source, e.target]));
  const errors = [];
  const reached = new Set();
  const triggers = nodes.filter((n) => n.type === 'trigger').sort((a, b) => a.position.y - b.position.y || a.position.x - b.position.x);
  const rules = [];
  for (const trigger of triggers) {
    const name = trigger.data.name?.trim() || '';
    const label = name || 'An unnamed rule';
    const scope = {};
    let effect = null;
    reached.add(trigger.id);
    for (let at = out.get(trigger.id); at; at = out.get(at)) {
      const node = byId.get(at);
      if (!node || reached.has(at)) break;
      reached.add(at);
      if (node.type === 'condition') {
        if (scope[node.data.key] !== undefined) errors.push(`${label}: “${conditionDef(node.data.key)?.label}” appears twice`);
        scope[node.data.key] = node.data.value;
      } else if (node.type === 'action') {
        effect = node.data.effect;
      }
    }
    if (!effect) {
      errors.push(`${label}: the chain does not end in an action`);
      continue;
    }
    if (!name) errors.push('Every rule needs a name (select its trigger)');
    rules.push({ name, enabled: trigger.data.enabled !== false, scope: clean(scope), effect });
  }
  const stray = nodes.filter((n) => !reached.has(n.id) && n.type !== 'guard' && n.type !== 'route');
  if (stray.length) errors.push(`${stray.length} node(s) are not on a chain from a trigger: connect or remove them`);
  return { rules, errors };
}

/**
 * Bring the read-only nodes in line with each chain's action: an evict rule
 * passes the safeguards and leaves by its route, a keep rule has neither.
 * Re-deriving the whole flow from the rules does exactly that.
 */
export function normalise(nodes, edges, context) {
  const { rules, errors } = flowToRules(nodes, edges);
  if (errors.length) return null;
  return rulesToFlow(rules, context);
}

/** A fresh node of `type` at `position`, with sensible starting data. */
export function newNode(type, position, extra = {}) {
  const id = `${type}-${Math.random().toString(36).slice(2, 9)}`;
  if (type === 'trigger') return { id, type, position, data: { name: '', enabled: true, evict: false, ...extra } };
  if (type === 'condition') return { id, type, position, data: { key: extra.key, value: startValue(extra.key) } };
  return { id, type: 'action', position, data: { effect: effectOf(extra.effect || 'keep', {}) } };
}

function startValue(key) {
  const shape = conditionDef(key)?.shape;
  if (shape === 'kind') return 'movie';
  if (shape === 'played') return false;
  if (shape === 'range') return { min: 0 };
  return [];
}
