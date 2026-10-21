import { useEffect, useMemo, useState } from "react";
import { LineChart } from "./LineChart";
import {
  SWITCH_COST_BPS,
  WINDOWS,
  backtest,
  fetchDailyCloses,
  type DailyClose,
  type Result,
  type WindowKey,
} from "./backtest";

const pct = (x: number, digits = 1) => `${x >= 0 ? "+" : ""}${(x * 100).toFixed(digits)}%`;

export function Backtest() {
  const [closes, setCloses] = useState<DailyClose[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [key, setKey] = useState<WindowKey>("1Y");

  useEffect(() => {
    fetchDailyCloses()
      .then(setCloses)
      .catch((e) => setError(String(e?.message ?? e)));
  }, []);

  const results = useMemo(() => {
    if (!closes) return null;
    return Object.fromEntries(WINDOWS.map((w) => [w.key, backtest(closes, w.days)])) as Record<WindowKey, Result | null>;
  }, [closes]);
  const result = results?.[key] ?? null;

  return (
    <section className="card viz-root">
      <div className="card-head">
        <div>
          <h2>Backtest</h2>
        </div>
        <div className="segmented" role="tablist" aria-label="Time range">
          {WINDOWS.map((w) => (
            <button
              key={w.key}
              role="tab"
              aria-selected={w.key === key}
              className={w.key === key ? "seg active" : "seg"}
              onClick={() => setKey(w.key)}
            >
              {w.key}
            </button>
          ))}
        </div>
      </div>

      {error && <p className="muted">Could not load price history: {error}</p>}
      {!error && !result && <p className="muted">Loading price history…</p>}
      {result && (
        <>
          <div className="stats five">
            <Stat label="Strategy" value={pct(result.strategyReturn)} tone={result.strategyReturn >= 0 ? "up" : "down"} />
            <Stat label="Buy & hold BTC" value={pct(result.holdReturn)} tone={result.holdReturn >= 0 ? "up" : "down"} />
            <Stat label="Max drawdown" value={`${pct(result.strategyDrawdown)} vs ${pct(result.holdDrawdown)}`} small />
            <Stat label="Switches" value={String(result.switches)} />
            <Stat label="Time in BTC" value={`${Math.round(result.timeInBtc * 100)}%`} />
          </div>
          <div className="legend">
            <span className="legend-item">
              <span className="swatch line s1" /> Strategy
            </span>
            <span className="legend-item">
              <span className="swatch line s2" /> Buy & hold BTC
            </span>
            <span className="legend-item">
              <span className="swatch band" /> Holding BTC
            </span>
          </div>
          <LineChart
            days={result.points.map((p) => p.day)}
            series={[
              { label: "Strategy", className: "s1", values: result.points.map((p) => p.strategy - 1) },
              { label: "Buy & hold", className: "s2", values: result.points.map((p) => p.hold - 1) },
            ]}
            bands={result.points.map((p) => p.inBtc)}
            includeZero
            format={(v) => pct(v)}
            tick={(v, step) => (Math.abs(v) < 1e-9 ? "0%" : pct(v, step < 0.01 ? 1 : 0))}
            directLabel={(s, v) => `${s.className === "s1" ? "Strategy" : "Hold"} ${pct(v)}`}
            ariaLabel={`Return of the strategy (${pct(result.strategyReturn)}) and of holding BTC (${pct(result.holdReturn)}) over the period`}
            extra={(i) => `Holding ${result.points[i].inBtc ? "BTC" : "USDC"}`}
          />
          <SummaryTable results={results!} />
          <p className="muted small note">
            Coinbase daily closes · {SWITCH_COST_BPS / 100}% per switch · excludes the 1% withdrawal fee · Past results do
            not predict future returns.
          </p>
        </>
      )}
    </section>
  );
}

function Stat({ label, value, tone, small }: { label: string; value: string; tone?: "up" | "down"; small?: boolean }) {
  return (
    <div className="stat">
      <div className="stat-label">{label}</div>
      <div className={`stat-value ${small ? "stat-small" : ""} ${tone ?? ""}`}>{value}</div>
    </div>
  );
}

function SummaryTable({ results }: { results: Record<WindowKey, Result | null> }) {
  return (
    <table className="fit-table summary">
        <thead>
          <tr>
            <th>Period</th>
            <th className="num">Strategy</th>
            <th className="num">Buy & hold</th>
            <th className="num">Drawdown</th>
            <th className="num hide-sm">Switches</th>
            <th className="num hide-sm">In BTC</th>
          </tr>
        </thead>
        <tbody>
          {WINDOWS.map((w) => {
            const r = results[w.key];
            if (!r) return null;
            return (
              <tr key={w.key}>
                <td>{w.key}</td>
                <td className="num">{pct(r.strategyReturn)}</td>
                <td className="num">{pct(r.holdReturn)}</td>
                <td className="num">
                  {pct(r.strategyDrawdown)} / {pct(r.holdDrawdown)}
                </td>
                <td className="num hide-sm">{r.switches}</td>
                <td className="num hide-sm">{Math.round(r.timeInBtc * 100)}%</td>
              </tr>
            );
          })}
        </tbody>
    </table>
  );
}
