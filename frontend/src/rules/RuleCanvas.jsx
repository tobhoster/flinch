import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Background, Controls, Handle, MiniMap, Position, ReactFlow, addEdge, applyEdgeChanges, applyNodeChanges } from '@xyflow/react';
import '@xyflow/react/dist/style.css';
import { Filter, Gauge, LayoutGrid, LogOut, Plus, ShieldCheck, Zap } from 'lucide-react';
import { EFFECTS, EVENTS, effectLabel, effectOf, rulesPayload } from '../RuleSettings.jsx';
import { CONDITIONS, ROW, canConnect, canonical, conditionDef, flowToRules, isEvict, newNode, rulesToFlow } from './flow.js';
import { palette } from '../../tailwind.config.js';

// The Visual Rules Workspace: the rule list as chains of trigger → conditions
// → action. It edits the same `rules` the form and YAML views edit; Settings
// still saves them only after a preview. See flow.js for the grammar.

const selectClass = 'rounded border border-line bg-ink-900 px-1.5 py-0.5 text-[12px] text-fg focus:border-fg-faint focus:outline-none';
const tone = {
  trigger: 'border-l-state-info',
  condition: 'border-l-fg-faint',
  guard: 'border-l-state-warn',
  keep: 'border-l-state-ok',
  evict: 'border-l-state-bad',
  route: 'border-l-line',
};

function Shell({ kind, icon: Icon, title, children, selected, inbound = true, outbound = true }) {
  return (
    <div className={`w-[200px] rounded-md border border-l-[3px] bg-ink-900 px-2.5 py-1.5 text-[12px] ${tone[kind]} ${selected ? 'border-fg-faint shadow-ring' : 'border-line'}`}>
      {inbound && <Handle type="target" position={Position.Left} className="!h-2 !w-2 !border-line !bg-ink-700" />}
      <div className="flex items-center gap-1.5 text-[11px] uppercase tracking-wide text-fg-faint">
        <Icon size={11} /> {title}
      </div>
      <div className="mt-0.5 text-fg">{children}</div>
      {outbound && <Handle type="source" position={Position.Right} className="!h-2 !w-2 !border-line !bg-ink-700" />}
    </div>
  );
}

const pctOf = (t) => (t == null ? '—' : `${Math.round(t * 100)}%`);

function TriggerNode({ data, selected }) {
  return (
    <Shell kind="trigger" icon={Gauge} title={data.enabled ? 'Trigger' : 'Trigger · off'} selected={selected} inbound={false}>
      <div className={`truncate font-medium ${data.enabled ? '' : 'text-fg-faint line-through'}`}>{data.name || 'Unnamed rule'}</div>
      <div className="text-fg-muted">
        {data.evict ? <>Disk forecast over target <span className="num">{pctOf(data.target)}</span></> : 'Every plan'}
      </div>
    </Shell>
  );
}

export function conditionText(key, value) {
  const def = conditionDef(key);
  if (!def) return key;
  if (def.shape === 'kind') return value === 'movie' ? 'Movies' : 'Seasons';
  if (def.shape === 'played') return value ? 'Played' : 'Never played';
  if (def.shape === 'range') {
    const { min, max } = value || {};
    const span = min != null && max != null ? `${min}–${max}` : min != null ? `≥ ${min}` : max != null ? `≤ ${max}` : 'any';
    return <><span className="text-fg-muted">{def.label}</span> <span className="num">{span}</span> <span className="text-fg-faint">{def.unit}</span></>;
  }
  const list = Array.isArray(value) ? value : [];
  return <><span className="text-fg-muted">{def.label}</span> {list.length ? list.join(', ') : <span className="text-state-warn">empty</span>}</>;
}

function ConditionNode({ data, selected }) {
  return (
    <Shell kind="condition" icon={Filter} title="Condition" selected={selected}>
      <div className="truncate">{conditionText(data.key, data.value)}</div>
    </Shell>
  );
}

function GuardNode({ selected }) {
  return (
    <Shell kind="guard" icon={ShieldCheck} title="Safeguards · always" selected={selected}>
      <div className="text-fg-muted">Keep tag, your keeps, grace, partway, seeding torrents</div>
    </Shell>
  );
}

function ActionNode({ data, selected }) {
  return (
    <Shell kind={isEvict(data.effect) ? 'evict' : 'keep'} icon={Zap} title="Action" selected={selected}>
      <div className="font-medium">{effectLabel(data.effect)}</div>
    </Shell>
  );
}

function RouteNode({ data, selected }) {
  return (
    <Shell kind="route" icon={LogOut} title="Leaves by" selected={selected} outbound={false}>
      <div className="text-fg-muted">Finished → delete · unfinished → Leaving Soon{data.leavingSoonDays != null && <> <span className="num">{data.leavingSoonDays}</span> d</>} first</div>
    </Shell>
  );
}

const NODE_TYPES = { trigger: TriggerNode, condition: ConditionNode, guard: GuardNode, action: ActionNode, route: RouteNode };

const listOf = (text, numeric) => text.split(',').map((part) => part.trim()).filter(Boolean).map((part) => (numeric ? Number(part) : part)).filter((v) => !numeric || Number.isFinite(v));
const bound = (raw) => (raw.trim() === '' || !Number.isFinite(Number(raw)) ? undefined : Number(raw));

/** The selected node's form: the only place its data is edited. */
function Inspector({ node, onData }) {
  if (!node) return <p className="text-[12px] text-fg-faint">Select a node to edit it. Drag from a node's right handle to the next one to chain them; a chain's conditions must all hold.</p>;
  const { type, data } = node;
  if (type === 'trigger') {
    return (
      <div className="space-y-2">
        <input aria-label="Rule name" className="input w-full" placeholder="Rule name" value={data.name} onChange={(e) => onData({ name: e.target.value })} />
        <label className="flex items-center gap-2 text-[12px] text-fg-muted">
          <input type="checkbox" checked={data.enabled} onChange={(e) => onData({ enabled: e.target.checked })} /> Enabled
        </label>
        <p className="text-[12px] text-fg-faint">When it acts follows from the action: a keep holds on every plan; prefer and must evict act only on a disk whose forecast is over the target, and never past a safeguard.</p>
      </div>
    );
  }
  if (type === 'condition') {
    const def = conditionDef(data.key);
    return (
      <div className="space-y-2">
        <select aria-label="Condition" className={selectClass} value={data.key}
          onChange={(e) => onData({ key: e.target.value, value: newNode('condition', { x: 0, y: 0 }, { key: e.target.value }).data.value })}>
          {CONDITIONS.map((c) => <option key={c.key} value={c.key}>{c.label}</option>)}
        </select>
        {def?.shape === 'kind' && (
          <select aria-label="Kind" className={selectClass} value={data.value} onChange={(e) => onData({ value: e.target.value })}>
            <option value="movie">Movies</option><option value="season">Seasons</option>
          </select>
        )}
        {def?.shape === 'played' && (
          <select aria-label="Played" className={selectClass} value={String(data.value)} onChange={(e) => onData({ value: e.target.value === 'true' })}>
            <option value="true">Played</option><option value="false">Never played</option>
          </select>
        )}
        {(def?.shape === 'list' || def?.shape === 'numbers') && (
          <input key={node.id + data.key} aria-label={def.label} className="input w-full" placeholder={def.placeholder}
            defaultValue={(data.value || []).join(', ')} onChange={(e) => onData({ value: listOf(e.target.value, def.shape === 'numbers') })} />
        )}
        {def?.shape === 'range' && (
          <div className="flex items-center gap-2">
            {['min', 'max'].map((end) => (
              <input key={node.id + end} aria-label={`${def.label} ${end}`} placeholder={end} className="input w-20 text-right tabular-nums"
                defaultValue={data.value?.[end] ?? ''} onChange={(e) => onData({ value: { ...data.value, [end]: bound(e.target.value) } })} />
            ))}
            <span className="text-[12px] text-fg-faint">{def.unit}</span>
          </div>
        )}
        <p className="text-[12px] text-fg-faint">A fact FLINCH could not read this cycle keeps the item and never evicts it.</p>
      </div>
    );
  }
  if (type === 'action') {
    const effect = data.effect;
    return (
      <div className="space-y-2">
        <select aria-label="Effect" className={selectClass} value={effect.type} onChange={(e) => onData({ effect: effectOf(e.target.value, effect) })}>
          {EFFECTS.map(([key, label]) => <option key={key} value={key}>{label}</option>)}
        </select>
        {effect.type === 'keep_until' && (
          <div className="flex flex-wrap items-center gap-2 text-[12px]">
            <input aria-label="Days" type="number" min={1} max={3650} className="input w-20 text-right tabular-nums" value={effect.days}
              onChange={(e) => onData({ effect: { ...effect, days: Number(e.target.value) } })} />
            <span className="text-fg-muted">days after</span>
            <select aria-label="Event" className={selectClass} value={effect.after} onChange={(e) => onData({ effect: { ...effect, after: e.target.value } })}>
              {EVENTS.map(([key, label]) => <option key={key} value={key}>{label}</option>)}
            </select>
          </div>
        )}
        {effect.type === 'keep_latest_seasons' && (
          <input aria-label="Seasons" type="number" min={1} max={100} className="input w-20 text-right tabular-nums" value={effect.seasons}
            onChange={(e) => onData({ effect: { ...effect, seasons: Number(e.target.value) } })} />
        )}
        <p className="text-[12px] text-fg-faint">How a picked item leaves is not a rule's choice: finished goes straight to deletion, unfinished through Leaving Soon first.</p>
      </div>
    );
  }
  return <p className="text-[12px] text-fg-faint">{type === 'guard' ? 'No rule overrides these; they are shown so the chain reads as the planner runs it.' : 'Set by the executor, not by the rule.'}</p>;
}

/**
 * `rules` in, `onChange(rules)` out whenever the flow says a valid list.
 * `context`: `{ target, leavingSoonDays }` for the info nodes.
 */
export default function RuleCanvas({ rules, onChange, context }) {
  const ctx = useMemo(() => ({ target: context?.target, leavingSoonDays: context?.leavingSoonDays }), [context?.target, context?.leavingSoonDays]);
  const emitted = useRef(canonical(rulesPayload(rules)));
  const [initial] = useState(() => rulesToFlow(rules, ctx));
  const [nodes, setNodes] = useState(initial.nodes);
  const [edges, setEdges] = useState(initial.edges);
  const [selected, setSelected] = useState(null);
  const [errors, setErrors] = useState([]);
  const instance = useRef(null);
  const refit = () => requestAnimationFrame(() => instance.current?.fitView({ padding: 0.15, maxZoom: 1, duration: 200 }));

  // A change from the form or YAML view, or a save, redraws the flow.
  useEffect(() => {
    const incoming = canonical(rulesPayload(rules));
    if (incoming === emitted.current) return;
    emitted.current = incoming;
    const flow = rulesToFlow(rules, ctx);
    setNodes(flow.nodes); setEdges(flow.edges); setErrors([]);
  }, [rules, ctx]);

  /** Read the flow back; a valid one is emitted and redrawn in its derived layout. */
  const commit = useCallback((nextNodes, nextEdges) => {
    const { rules: next, errors: problems } = flowToRules(nextNodes, nextEdges);
    setErrors(problems);
    if (problems.length) return;
    const payload = canonical(rulesPayload(next));
    const flow = rulesToFlow(next, ctx);
    setNodes(flow.nodes); setEdges(flow.edges);
    refit();
    if (payload !== emitted.current) {
      emitted.current = payload;
      onChange(next);
    }
  }, [ctx, onChange]);

  const onNodesChange = useCallback((changes) => {
    const next = applyNodeChanges(changes, nodes);
    const gone = new Set(changes.filter((c) => c.type === 'remove').map((c) => c.id));
    if (!gone.size) {
      setNodes(next);
      return;
    }
    // A removed link in a chain is bridged: what led into it now leads to what followed it.
    let nextEdges = edges.filter((e) => !gone.has(e.source) && !gone.has(e.target));
    for (const id of gone) {
      const from = edges.find((e) => e.target === id && !gone.has(e.source))?.source;
      let to = edges.find((e) => e.source === id)?.target;
      while (to && gone.has(to)) to = edges.find((e) => e.source === to)?.target;
      if (from && to && canConnect({ source: from, target: to }, next, nextEdges)) nextEdges = [...nextEdges, { id: `${from}->${to}`, source: from, target: to }];
    }
    setNodes(next);
    setEdges(nextEdges);
    if (gone.has(selected)) setSelected(null);
    commit(next, nextEdges);
  }, [nodes, edges, selected, commit]);

  const onEdgesChange = useCallback((changes) => {
    const next = applyEdgeChanges(changes, edges);
    setEdges(next);
    if (changes.some((c) => c.type === 'remove')) commit(nodes, next);
  }, [edges, nodes, commit]);

  const onConnect = useCallback((connection) => {
    const next = addEdge({ ...connection, id: `${connection.source}->${connection.target}` }, edges);
    setEdges(next);
    commit(nodes, next);
  }, [edges, nodes, commit]);

  const isValidConnection = useCallback((connection) => canConnect(connection, nodes, edges), [nodes, edges]);

  const onData = (patch) => {
    const next = nodes.map((n) => (n.id === selected ? { ...n, data: { ...n.data, ...patch } } : n));
    setNodes(next);
    commit(next, edges);
  };

  /**
   * A new trigger starts a row below the others. A new condition or action
   * goes on the end of the selected node's chain and is linked to it when the
   * grammar allows; otherwise it waits beside it to be connected by hand.
   */
  const spawn = (type, extra) => {
    const out = new Map(edges.map((e) => [e.source, e.target]));
    let end = nodes.find((n) => n.id === selected);
    for (let steps = 0; end && out.has(end.id) && steps <= nodes.length; steps += 1) {
      const next = nodes.find((n) => n.id === out.get(end.id));
      if (!next || next.type === 'guard' || next.type === 'route') break;
      end = next;
    }
    const position = type === 'trigger' || !end
      ? { x: 0, y: nodes.length ? Math.max(...nodes.map((n) => n.position.y)) + ROW : 0 }
      : { x: end.position.x + 230, y: end.position.y };
    const node = newNode(type, position, extra);
    const nextNodes = [...nodes, node];
    const link = type !== 'trigger' && end && canConnect({ source: end.id, target: node.id }, nextNodes, edges);
    const nextEdges = link ? [...edges, { id: `${end.id}->${node.id}`, source: end.id, target: node.id }] : edges;
    setNodes(nextNodes);
    setEdges(nextEdges);
    setSelected(node.id);
    if (type === 'action' && link) commit(nextNodes, nextEdges);
    else setErrors(flowToRules(nextNodes, nextEdges).errors);
  };

  const selectedNode = nodes.find((n) => n.id === selected) || null;
  const [conditionKey, setConditionKey] = useState('p_watch');
  const [effectKey, setEffectKey] = useState('keep');

  return (
    <div className="flex h-full flex-col gap-2">
      <div className="flex flex-wrap items-center gap-2">
        <button className="btn" onClick={() => spawn('trigger')}><Plus size={13} /> Rule</button>
        <span className="inline-flex items-center gap-1">
          <select aria-label="Condition to add" className={selectClass} value={conditionKey} onChange={(e) => setConditionKey(e.target.value)}>
            {CONDITIONS.map((c) => <option key={c.key} value={c.key}>{c.label}</option>)}
          </select>
          <button className="btn" onClick={() => spawn('condition', { key: conditionKey })}><Filter size={13} /> Condition</button>
        </span>
        <span className="inline-flex items-center gap-1">
          <select aria-label="Action to add" className={selectClass} value={effectKey} onChange={(e) => setEffectKey(e.target.value)}>
            {EFFECTS.map(([key, label]) => <option key={key} value={key}>{label}</option>)}
          </select>
          <button className="btn" onClick={() => spawn('action', { effect: effectKey })}><Zap size={13} /> Action</button>
        </span>
        <button className="btn ml-auto" title="Lay every chain out again in list order" onClick={() => commit(nodes, edges)}><LayoutGrid size={13} /> Tidy</button>
      </div>
      <div className="grid min-h-0 flex-1 gap-2 md:grid-cols-[minmax(0,1fr)_280px]">
        <div className="h-full min-h-[420px] overflow-hidden rounded-md border border-line bg-ink-950">
          <ReactFlow
            nodes={nodes}
            edges={edges}
            nodeTypes={NODE_TYPES}
            onNodesChange={onNodesChange}
            onEdgesChange={onEdgesChange}
            onConnect={onConnect}
            isValidConnection={isValidConnection}
            onNodeClick={(_, node) => setSelected(node.id)}
            onPaneClick={() => setSelected(null)}
            onNodeDragStop={() => commit(nodes, edges)}
            colorMode="dark"
            onInit={(rf) => { instance.current = rf; }}
            fitView
            fitViewOptions={{ padding: 0.15, maxZoom: 1 }}
            minZoom={0.3}
            proOptions={{ hideAttribution: true }}
            defaultEdgeOptions={{ style: { stroke: palette.fg.faint, strokeWidth: 1.25 } }}
          >
            <Background gap={20} color={palette.line.soft} />
            <MiniMap pannable zoomable maskColor={`${palette.ink[950]}b3`} nodeColor={palette.line.DEFAULT} />
            <Controls showInteractive={false} />
          </ReactFlow>
        </div>
        <div className="space-y-3 rounded-md border border-line bg-ink-900 p-3">
          <div className="text-[11px] uppercase tracking-wide text-fg-faint">{selectedNode ? selectedNode.type : 'Inspector'}</div>
          <Inspector node={selectedNode} onData={onData} />
        </div>
      </div>
      {errors.length > 0 && (
        <ul className="space-y-0.5 text-[12px] text-state-warn">
          {errors.map((e) => <li key={e}>{e}</li>)}
          <li className="text-fg-faint">The rules stay as they were until the flow is complete.</li>
        </ul>
      )}
    </div>
  );
}
