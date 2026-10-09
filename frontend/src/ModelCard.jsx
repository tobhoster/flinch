import React from 'react';
import { SectionTitle, ago } from './ui.jsx';

/** Below this many out-of-fold rows the scores are noise, and the card says so. */
const TRUSTED_ROWS = 30;

const fixed = (value, digits) => (Number.isFinite(value) ? value.toFixed(digits) : '—');
const signed = (value, digits) => (Number.isFinite(value) ? `${value > 0 ? '+' : value < 0 ? '−' : ''}${Math.abs(value).toFixed(digits)}` : '—');
const nowSecs = () => Math.floor(Date.now() / 1000);
/** How the card names each model kind the daily fit can adopt. */
const KIND = { recalibrated: 'Recalibrated', full: 'Full fit' };

/** The hazard parameters as `[label, hazard key, format]`; priors come from fit.json. */
const PARAMS = [
  ['λ₀ per day', 'lambda0_per_day', (v) => fixed(v, 4)],
  ['Recency', 'beta_recency', (v) => signed(v, 2)],
  ['Viewings', 'beta_scrobbles', (v) => signed(v, 2)],
  ['Show plays', 'beta_velocity', (v) => signed(v, 2)],
  ['Cycle', 'beta_cyclical', (v) => signed(v, 2)],
  ['Finished', 'beta_finished', (v) => signed(v, 2)],
  ['Taste', 'beta_taste', (v) => signed(v, 2)],
];

/** A score with its 95% bootstrap interval, when the fit reported one. */
function Scored({ value, interval, digits }) {
  return (
    <>
      {fixed(value, digits)}
      {Array.isArray(interval) && (
        <span className="text-fg-muted"> ({fixed(interval[0], digits)}–{fixed(interval[1], digits)})</span>
      )}
    </>
  );
}

const th = 'py-1 pr-3 text-right font-normal';
const td = 'num py-1 pr-3 text-right';

/**
 * The P(watch) hazard: which model is in force and why, how it scored on
 * titles it never saw, and its parameters next to the priors. Straight from
 * `status.fit`, which the daemon writes once a day.
 */
export default function ModelCard({ fit }) {
  if (!fit) {
    return (
      <section>
        <SectionTitle term="model">Watch model</SectionTitle>
        <p className="text-fg-muted">Priors. The daemon fits your history once a day, starting after its first run.</p>
      </section>
    );
  }
  const m = fit.metrics || {};
  const h = fit.hazard || {};
  const examples = m.examples ?? 0;
  const played = m.played ?? 0;
  // With one outcome only, AUC is undefined and Brier trivially perfect.
  const scored = played > 0 && played < examples;
  const kind = KIND[fit.kind] ?? KIND.full;
  const rows = [
    [kind, m.auc, m.brier, m.ece, m.spread],
    ['Priors', m.priors_auc, m.priors_brier, m.priors_ece, m.priors_spread],
  ];
  return (
    <section>
      <SectionTitle hint={`fitted ${ago(Math.max(0, nowSecs() - (fit.fitted_at_unix || 0)))}`} term="model">Watch model</SectionTitle>
      <p>
        {fit.adopted ? <span className="text-state-ok">{kind} in force</span> : 'Priors in force'}
        {!fit.adopted && fit.shortfall && <span className="text-fg-muted">: {fit.shortfall}</span>}
        <span className="text-fg-muted">
          {' · '}{examples} rows, {played} played within {m.horizon_days ?? 90} d
          {' '}({m.played_items ?? 0} {m.played_items === 1 ? 'title' : 'titles'})
        </span>
      </p>
      <div className="mt-2 flex flex-wrap items-start gap-x-8 gap-y-3">
        {scored && (
          <table className="text-[13px]">
            <thead>
              <tr className="border-b border-line text-left text-xs text-fg-muted">
                <th className="py-1 pr-3 font-normal">Out of fold</th>
                <th className={th}>AUC ↑</th>
                <th className={th}>Brier ↓</th>
                <th className={th}>ECE ↓</th>
              </tr>
            </thead>
            <tbody>
              {rows.map(([label, auc, brier, ece, spread]) => (
                <tr key={label} className="border-b border-line-soft last:border-0">
                  <td className="py-1 pr-3">{label}</td>
                  <td className={td}><Scored value={auc} interval={spread?.auc} digits={2} /></td>
                  <td className={td}><Scored value={brier} interval={spread?.brier} digits={3} /></td>
                  <td className={td}>{fixed(ece, 3)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        <table className="text-[13px]">
          <thead>
            <tr className="border-b border-line text-left text-xs text-fg-muted">
              <th className="py-1 pr-3 font-normal">Parameter</th>
              <th className={th}>Fitted</th>
              <th className={th}>Prior</th>
            </tr>
          </thead>
          <tbody>
            {PARAMS.map(([label, key, format]) => (
              <tr key={key} className="border-b border-line-soft last:border-0">
                <td className="py-1 pr-3">{label}</td>
                <td className={td}>{format(h[key])}</td>
                <td className={`${td} text-fg-muted`}>{fit.priors ? format(fit.priors[key]) : '—'}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {scored && examples < TRUSTED_ROWS && (
        <p className="mt-1 text-xs text-fg-faint">Only {examples} rows: too few for these scores to mean much yet.</p>
      )}
      {!scored && examples > 0 && (
        <p className="mt-1 text-xs text-fg-faint">Scores appear once the history holds titles both played and not played within the window.</p>
      )}
    </section>
  );
}
