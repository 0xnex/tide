import { useMemo, useState } from "react";
import { LineChart } from "./LineChart";
import { SWITCH_COST_BPS, WINDOWS, backtest, type DailyClose, type Result, type WindowKey } from "./backtest";

const pct = (x: number, digits = 1) => `${x >= 0 ? "+" : ""}${(x * 100).toFixed(digits)}%`;
/** 两个比例之差，按百分点；四舍五入后是 0 的不带正负号 */
const pts = (x: number) => (Math.abs(x) < 0.0005 ? "0.0 pts" : `${x > 0 ? "+" : "−"}${Math.abs(x * 100).toFixed(1)} pts`);
/** 差值的颜色：策略更好是绿色，更差是红色，差不多是灰色 */
const tone = (x: number) => (Math.abs(x) < 0.0005 ? "muted" : x > 0 ? "up" : "down");

export function Backtest({ closes, error }: { closes: DailyClose[] | null; error: string | null }) {
  const [key, setKey] = useState<WindowKey>("1Y");

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
          <div className="compare">
            <Compare
              label="Return"
              strategy={result.strategyReturn}
              hold={result.holdReturn}
              diff={result.strategyReturn - result.holdReturn}
              badge={`${pts(result.strategyReturn - result.holdReturn)} vs hold`}
            />
            <Compare
              label="Max drawdown"
              strategy={result.strategyDrawdown}
              hold={result.holdDrawdown}
              // 回撤是负数：策略的数更大就是回撤更浅
              diff={result.strategyDrawdown - result.holdDrawdown}
              badge={
                tone(result.strategyDrawdown - result.holdDrawdown) === "muted"
                  ? "Same as hold"
                  : `${Math.abs((result.strategyDrawdown - result.holdDrawdown) * 100).toFixed(1)} pts ${
                      result.strategyDrawdown > result.holdDrawdown ? "shallower" : "deeper"
                    }`
              }
            />
          </div>
          <div className="mini-stats compare-extra">
            <span>Switches {result.switches}</span>
            <span>Time in BTC {Math.round(result.timeInBtc * 100)}%</span>
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

/** 策略和持有 BTC 的一项指标：两根横条，长度按绝对值，标签写出差多少 */
function Compare({ label, strategy, hold, diff, badge }: { label: string; strategy: number; hold: number; diff: number; badge: string }) {
  const scale = Math.max(Math.abs(strategy), Math.abs(hold)) || 1;
  const bar = (name: string, cls: "s1" | "s2", v: number) => (
    <div className="cbar">
      <span className="muted">{name}</span>
      <span className="cbar-track">
        <span className={`cbar-fill ${cls}`} style={{ width: `${(Math.abs(v) / scale) * 100}%` }} />
      </span>
      <strong className={v >= 0 ? "up" : "down"}>{pct(v)}</strong>
    </div>
  );
  return (
    <div>
      <div className="compare-head">
        <span className="stat-label">{label}</span>
        <span className={`pill ${{ up: "pill-green", down: "pill-red", muted: "pill-gray" }[tone(diff)]}`}>{badge}</span>
      </div>
      {bar("Strategy", "s1", strategy)}
      {bar("Hold BTC", "s2", hold)}
    </div>
  );
}

function SummaryTable({ results }: { results: Record<WindowKey, Result | null> }) {
  return (
    <table className="fit-table summary">
      <thead>
        <tr>
          <th>Period</th>
          <th className="num">Return vs hold</th>
          <th className="num">Drawdown vs hold</th>
          <th className="num hide-sm">Switches</th>
          <th className="num hide-sm">In BTC</th>
        </tr>
      </thead>
      <tbody>
        {WINDOWS.map((w) => {
          const r = results[w.key];
          if (!r) return null;
          const ret = r.strategyReturn - r.holdReturn;
          const dd = r.strategyDrawdown - r.holdDrawdown;
          return (
            <tr key={w.key}>
              <td>{w.key}</td>
              <td className="num">
                <strong className={tone(ret)}>{pts(ret)}</strong>
                <div className="muted small">
                  {pct(r.strategyReturn)} vs {pct(r.holdReturn)}
                </div>
              </td>
              <td className="num">
                <strong className={tone(dd)}>{pts(dd)}</strong>
                <div className="muted small">
                  {pct(r.strategyDrawdown)} vs {pct(r.holdDrawdown)}
                </div>
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
