import React, { useMemo } from 'react';
import { GiB, SectionTitle } from './ui.jsx';
import { Explain } from './Explain.jsx';

const LIMIT = 10;
const TIME = { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' };
/** The default storage ceiling; history rows do not record the one in force. */
const CEILING_WARN = 0.8;

/** The last runs from `/api/history`, newest first. */
export default function RecentRuns({ history }) {
  const rows = useMemo(() => history.slice(-LIMIT).reverse(), [history]);
  // history.json only carries a mode once the daemon writes one per run; until
  // then the column would be a stack of dashes, so it is left out.
  const hasMode = rows.some((h) => typeof h.dry_run === 'boolean');
  // Same for disk use: older rows predate the capacity measurement.
  const hasUsed = rows.some((h) => typeof h.utilization === 'number');

  return (
    <section>
      <SectionTitle hint={history.length > LIMIT ? `last ${LIMIT} of ${history.length}` : undefined}>Recent runs</SectionTitle>
      {rows.length === 0 ? (
        <p className="text-[13px] text-fg-muted">No runs recorded yet.</p>
      ) : (
        // The padding pair keeps the Mode explainer's touch area inside the scroller's clip.
        <div className="-mt-1.5 overflow-x-auto pt-1.5">
          <table className="w-full text-[13px]">
            <thead>
              <tr className="border-b border-line text-left text-xs text-fg-muted">
                <th className="py-1.5 pr-3 font-normal">Time</th>
                <th className="py-1.5 pr-3 text-right font-normal">Candidates</th>
                <th className={`py-1.5 text-right font-normal ${hasUsed || hasMode ? 'pr-3' : ''}`}>Reclaimable</th>
                {hasUsed && <th className={`py-1.5 text-right font-normal ${hasMode ? 'pr-3' : ''}`} title="Disk utilization measured at the run">Used</th>}
                {hasMode && (
                  <th className="py-1.5 pr-3 font-normal">
                    <span className="inline-flex items-center gap-1">Mode <Explain term="dry_run" /></span>
                  </th>
                )}
              </tr>
            </thead>
            <tbody>
              {rows.map((h) => (
                <tr key={h.ran_at_unix} className="border-b border-line-soft last:border-0">
                  <td className="num whitespace-nowrap py-1.5 pr-3 text-fg-muted">
                    {h.ran_at_unix ? new Date(h.ran_at_unix * 1000).toLocaleString([], TIME) : '—'}
                  </td>
                  <td className={`num py-1.5 pr-3 text-right ${h.delete_candidates > 0 ? 'text-state-warn' : ''}`}>{h.delete_candidates ?? '—'}</td>
                  <td className={`num whitespace-nowrap py-1.5 text-right ${hasUsed || hasMode ? 'pr-3' : ''}`}>{GiB(h.reclaimed_bytes)} GiB</td>
                  {hasUsed && (
                    <td className={`num py-1.5 text-right ${hasMode ? 'pr-3' : ''} ${h.utilization >= CEILING_WARN ? 'text-state-warn' : ''}`}>
                      {typeof h.utilization === 'number' ? `${Math.round(h.utilization * 100)}%` : '—'}
                    </td>
                  )}
                  {hasMode && (
                    <td className="py-1.5 text-fg-muted">{typeof h.dry_run === 'boolean' ? (h.dry_run ? 'Dry run' : 'Enforced') : '—'}</td>
                  )}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}
