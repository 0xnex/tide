// 信号：最近一个均线窗口里的定价价格（链上存的价格，基金就是用它们做决定的）、每次定价记下的均线、
// 下一次定价的买入门槛，以及现在的 BTC 价格离门槛多远。只用来显示，真正的判断在 fund-logic 的 fix 里。
import { BN } from "@coral-xyz/anchor";
import { LineChart } from "./LineChart";
import { DAY, type Snapshot } from "./chain";
import * as fmt from "./format";
import { buyThreshold, nextFixTime } from "./signal";

/** 刻度条两端离门槛多远（±5%），更远的点贴在两端 */
const METER_RANGE = 0.05;

const usd = (v: number) => `$${v.toLocaleString("en-US", { maximumFractionDigits: 0 })}`;
const signedPct = (x: number) => `${x >= 0 ? "+" : "−"}${Math.abs(x * 100).toFixed(1)}%`;

export function SignalCard({ snap, spot, now }: { snap: Snapshot; spot: number | null; now: number }) {
  const { fund, records } = snap;
  const w = fund.config.window;
  const nextDay = Math.floor(nextFixTime(fund, now) / DAY);
  const raw = buyThreshold(fund, nextDay);
  const threshold = raw === null ? null : Number(raw) / 1e6;

  // 链上价格：最近 w 天（一个均线窗口）
  const prices = (fund.state.prices as BN[])
    .map((p, i) => ({ day: (fund.state.priceDays as BN[])[i].toNumber(), price: p.toNumber() / 1e6 }))
    .filter((p) => p.price > 0)
    .sort((a, b) => a.day - b.day)
    .slice(-w);
  // 每次定价时实际用的均线
  const ma = new Map(records.filter((r) => !r.movingAverage.isZero()).map((r) => [r.epoch.toNumber(), r.movingAverage.toNumber() / 1e6]));
  const withNow = spot !== null;
  const days = [...prices.map((p) => p.day), ...(withNow ? [Math.floor(now / DAY)] : [])];

  return (
    <section className="card viz-root">
      <div className="card-head">
        <div>
          <h2>Signal</h2>
          <p className="muted small">
            Fixing price vs its {w}-day average · {prices.length} days on chain
          </p>
        </div>
      </div>

      {threshold !== null && spot !== null && <SwitchMeter spot={spot} threshold={threshold} />}

      {prices.length >= 2 && (
        <>
          <div className="legend">
            <span className="legend-item">
              <span className="swatch line s1" /> Fixing price
            </span>
            <span className="legend-item">
              <span className="swatch line s2" /> {w}-day average
            </span>
            {threshold !== null && (
              <span className="legend-item">
                <span className="swatch line s3 dashed" /> Buy line, next fixing
              </span>
            )}
          </div>
          <LineChart
            days={days}
            series={[
              { label: "Price", className: "s1", values: [...prices.map((p) => p.price), ...(withNow ? [spot] : [])] },
              { label: `${w}-day avg`, className: "s2", values: [...prices.map((p) => ma.get(p.day) ?? null), ...(withNow ? [null] : [])] },
              ...(threshold !== null
                ? [{ label: "Buy line", className: "s3" as const, values: days.map(() => threshold), dashed: true }]
                : []),
            ]}
            marker={withNow ? { index: days.length - 1, label: "Now" } : undefined}
            format={usd}
            tick={(v) => usd(v)}
            ariaLabel={`Fixing prices of the last ${prices.length} days against the ${w}-day average${
              threshold !== null ? ` and the next buy line at ${usd(threshold)}` : ""
            }`}
            extra={(i) => (withNow && i === days.length - 1 ? "BTC now (Coinbase), not a fixing" : `Pyth fixing price, ${fmt.dayLabel(days[i])}`)}
          />
        </>
      )}
    </section>
  );
}

/** 现在的 BTC 价格在门槛哪一边、离多远：左边是 USDC，右边是 cbBTC */
function SwitchMeter({ spot, threshold }: { spot: number; threshold: number }) {
  const gap = spot / threshold - 1;
  const pos = 50 + (Math.max(-METER_RANGE, Math.min(METER_RANGE, gap)) / METER_RANGE) * 50;
  const above = gap > 0;
  return (
    <div className="meter">
      <div className="meter-head">
        <span>
          <span className="muted">Next fixing buys cbBTC above </span>
          <strong>{usd(threshold)}</strong>
        </span>
        <span className={above ? "up" : "down"}>
          BTC {usd(spot)} · <strong>{Math.abs(gap * 100).toFixed(1)}%</strong> {above ? "above" : "below"} the buy line
        </span>
      </div>
      <div className="meter-track" role="img" aria-label={`BTC now is ${signedPct(gap)} from the buy line`}>
        <span className="meter-side usdc">USDC</span>
        <span className="meter-side btc">cbBTC</span>
        <span className="meter-line" />
        <span className={`meter-dot ${above ? "up" : "down"}`} style={{ left: `${pos}%` }} />
      </div>
      <div className="meter-scale muted">
        <span>−{METER_RANGE * 100}%</span>
        <span>buy line</span>
        <span>+{METER_RANGE * 100}%</span>
      </div>
    </div>
  );
}
