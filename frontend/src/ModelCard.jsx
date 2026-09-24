import React from 'react';
import { SectionTitle, ago } from './ui.jsx';

/** Below this many out-of-fold rows the scores are noise, and the card says so. */
const TRUSTED_ROWS = 30;

const fixed = (value, digits) => (Number.isFinite(value) ? value.toFixed(digits) : '—');
const nowSecs = () => Math.floor(Date.now() / 1000);
/** How the card names each model kind the daily fit can adopt. */
const KIND = { recalibrated: 'recalibrated priors', full: 'full fit' };

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

const confidentText = (spread) => `${spread.confident ?? 0}, ${spread.confident_wrong ?? 0} wrong`;

/** A paired interval (FLINCH minus the other model) as the side it favours. */
function verdict(interval, lowerWins, other) {
  if (!interval) return 'too few titles to say';
  const [low, high] = interval;
  if (lowerWins ? high < 0 : low > 0) return 'FLINCH better';
  if (lowerWins ? low > 0 : high < 0) return `${other} better`;
  return 'no clear difference';
}

/**
 * The last `flinch-fit --against --write` run: an external model and FLINCH's
 * two forecasts scored on the same joined rows, so the ranking is like for like.
 */
function Benchmark({ benchmark }) {
  const result = benchmark.result || {};
  const joined = result.join?.joined ?? 0;
  const secs = Math.max(0, nowSecs() - (benchmark.scored_at_unix || 0));
  const rows = [
    [benchmark.model, result.external],
    ['FLINCH priors', result.priors],
    ...(result.recalibrated?.n ? [['FLINCH recalibrated', result.recalibrated]] : []),
    ['FLINCH full fit', result.fitted],
  ];
  return (
    <div className="mt-4">
      <SectionTitle hint={`scored ${ago(secs)} · ${joined} rows`}>Against {benchmark.model}</SectionTitle>
      <table className="w-full max-w-md text-[13px]">
        <thead>
          <tr className="border-b border-line text-left text-xs text-fg-muted">
            <th className="py-1 pr-3 font-normal">Same rows</th>
            <th className="py-1 pr-3 text-right font-normal">AUC ↑</th>
            <th className="py-1 pr-3 text-right font-normal">Brier ↓</th>
            <th className="py-1 pr-3 text-right font-normal">Log-loss ↓</th>
            <th className="py-1 text-right font-normal">ECE ↓</th>
          </tr>
        </thead>
        <tbody>
          {rows.map(([label, card]) => (
            <tr key={label} className="border-b border-line-soft last:border-0">
              <td className="py-1 pr-3">{label}</td>
              <td className="num py-1 pr-3 text-right">{fixed(card?.auc, 2)}</td>
              <td className="num py-1 pr-3 text-right">{fixed(card?.brier, 3)}</td>
              <td className="num py-1 pr-3 text-right">{fixed(card?.log_loss, 3)}</td>
              <td className="num py-1 text-right">{fixed(card?.ece, 3)}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {result.versus?.recalibrated && (
        <p className="mt-1 text-xs text-fg-muted">
          Recalibrated FLINCH against {benchmark.model}, 95% over resampled titles:
          {' '}Brier {verdict(result.versus.recalibrated.brier, true, benchmark.model)}
          {' · '}log-loss {verdict(result.versus.recalibrated.log_loss, true, benchmark.model)}
          {' · '}AUC {verdict(result.versus.recalibrated.auc, false, benchmark.model)}
        </p>
      )}
      {joined < TRUSTED_ROWS && (
        <p className="mt-1 text-xs text-fg-faint">Only {joined} rows: too few to rank models yet.</p>
      )}
    </div>
  );
}

/**
 * Which forecast runs, what it has learnt from, and how it scored on titles it
 * never saw. Straight from `status.fit`, which the daemon writes once a day,
 * and `status.benchmark`, the last head-to-head against an external model.
 */
export default function ModelCard({ fit, benchmark }) {
  if (!fit) {
    return (
      <section>
        <SectionTitle term="model">Forecast model</SectionTitle>
        <p className="text-fg-muted">No fit yet. The daemon fits your history once a day, starting after its first run.</p>
        {benchmark && <Benchmark benchmark={benchmark} />}
      </section>
    );
  }
  const m = fit.metrics || {};
  const played = Math.max(0, (m.examples ?? 0) - (m.positives ?? 0));
  // With one outcome only, a fit that always answers it scores a "perfect"
  // Brier and a meaningless AUC: nothing to show until both outcomes exist.
  const scored = (m.validation ?? 0) > 0 && played > 0 && (m.positives ?? 0) > 0;
  const secs = Math.max(0, nowSecs() - (fit.fitted_at_unix || 0));
  const kind = KIND[fit.kind] ?? KIND.full;
  const rows = [
    [kind.charAt(0).toUpperCase() + kind.slice(1), m.auc, m.brier, m.ece, m.spread],
    ['Priors', m.priors_auc, m.priors_brier, m.priors_ece, m.priors_spread],
  ];
  return (
    <section>
      <SectionTitle hint={`fitted ${ago(secs)}`} term="model">Forecast model</SectionTitle>
      <p>
        {fit.adopted ? <span className="text-state-ok">Running the {kind}, fitted to your history</span> : 'Hand-set priors'}
        <span className="text-fg-muted">
          {' · '}{m.examples ?? 0} past questions, {played} played within {m.horizon_days ?? 30} days
          {' '}({m.negative_items ?? 0} {m.negative_items === 1 ? 'title' : 'titles'})
        </span>
      </p>
      {fit.shortfall && <p className="mt-1 text-fg-muted">Not adopted yet: {fit.shortfall}.</p>}
      {scored && (
        <table className="mt-2 w-full max-w-lg text-[13px]">
          <thead>
            <tr className="border-b border-line text-left text-xs text-fg-muted">
              <th className="py-1 pr-3 font-normal">Out-of-fold ({m.validation} rows)</th>
              <th className="py-1 pr-3 text-right font-normal">AUC ↑</th>
              <th className="py-1 pr-3 text-right font-normal">Brier ↓</th>
              <th className="py-1 text-right font-normal">ECE ↓</th>
            </tr>
          </thead>
          <tbody>
            {rows.map(([label, auc, brier, ece, spread]) => (
              <tr key={label} className="border-b border-line-soft last:border-0">
                <td className="py-1 pr-3">{label}</td>
                <td className="num py-1 pr-3 text-right"><Scored value={auc} interval={spread?.auc} digits={2} /></td>
                <td className="num py-1 pr-3 text-right"><Scored value={brier} interval={spread?.brier} digits={3} /></td>
                <td className="num py-1 text-right">{fixed(ece, 3)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {scored && m.spread && m.priors_spread && (
        <p className="mt-1 text-xs text-fg-muted">
          Confident forecasts (≥90% either way): {kind} {confidentText(m.spread)} · priors {confidentText(m.priors_spread)}
        </p>
      )}
      {scored && m.validation < TRUSTED_ROWS && (
        <p className="mt-1 text-xs text-fg-faint">Only {m.validation} out-of-fold rows: too few for these scores to mean much yet.</p>
      )}
      {!scored && (m.examples ?? 0) > 0 && (
        <p className="mt-1 text-xs text-fg-faint">Out-of-fold scores appear once the history holds both outcomes: titles played within the window and titles not.</p>
      )}
      {benchmark && <Benchmark benchmark={benchmark} />}
    </section>
  );
}
