// 每份价格：(USDC + cbBTC × BTC 价格) ÷ 份额，只用来显示（基金本身从不按美元估值）。
// 历史点用每次定价记录里定价时（当天申购赎回之前）的资产和份额，价格用那次定价的 Pyth 价格；
// 最后一个点用现在的资产和实时 BTC 价格。
// 开始之前的部分用回测模拟（虚线），缩放到开始那天正好 1 美元，和实际部分接上。
import { useState } from "react";
import { BN } from "@coral-xyz/anchor";
import { LineChart } from "./LineChart";
import type { Snapshot } from "./chain";
import { simulatedSharePrice, type DailyClose } from "./backtest";

const big = (x: BN) => BigInt(x.toString());

/** 每份价格（美元）；没有份额时为 null */
export function sharePrice(usdc: bigint, btc: bigint, shares: bigint, btcUsd: number): number | null {
  if (shares === 0n) return null;
  return (Number(usdc) / 1e6 + (Number(btc) / 1e8) * btcUsd) / (Number(shares) / 1e6);
}

/** 现在用来折算的 BTC 价格：实时价格，没有时用最近一次定价的价格 */
export function pricingBasis(snap: Snapshot, spot: number | null): { btcUsd: number; source: string } | null {
  if (spot) return { btcUsd: spot, source: "Coinbase spot" };
  const last = snap.records[0];
  return last ? { btcUsd: Number(big(last.signalPrice)) / 1e6, source: "last fixing" } : null;
}

const usd = (v: number, digits = 4) => `$${v.toLocaleString("en-US", { minimumFractionDigits: digits, maximumFractionDigits: digits })}`;

const pctChange = (v: number | null) => (v === null ? "—" : `${v >= 0 ? "+" : ""}${(v * 100).toFixed(2)}%`);
const dateOf = (day: number) => new Date(day * 86_400_000).toISOString().slice(0, 10);

interface PricePoint {
  day: number;
  price: number;
  btcUsd: number;
  now: boolean;
}

/** 模拟部分覆盖开始前多少天 */
const SIMULATED_DAYS = 365;

export function SharePriceCard({ snap, spot, closes }: { snap: Snapshot; spot: number | null; closes: DailyClose[] | null }) {
  const s = snap.fund.state;
  const basis = pricingBasis(snap, spot);
  const history: PricePoint[] = [...snap.records]
    .reverse()
    .filter((r) => !r.fundShares.isZero())
    .map((r) => ({
      day: r.epoch.toNumber(),
      price: sharePrice(big(r.fundUsdc), big(r.fundBtc), big(r.fundShares), Number(big(r.signalPrice)) / 1e6)!,
      btcUsd: Number(big(r.signalPrice)) / 1e6,
      now: false,
    }));
  const current = basis ? sharePrice(big(s.usdc), big(s.btc), big(s.shareSupply), basis.btcUsd) : null;
  const points: PricePoint[] =
    current !== null && basis
      ? [...history, { day: Math.floor(Date.now() / 86_400_000), price: current, btcUsd: basis.btcUsd, now: true }]
      : history;
  const chart = joinedChart(snap, points, closes);
  const first = points[0];
  const change = first && current !== null && points.length > 1 ? current / first.price - 1 : null;
  // 预估的组成：每份对应的 cbBTC 和 USDC
  const supply = Number(big(s.shareSupply)) / 1e6;
  const btcPerShare = supply > 0 ? Number(big(s.btc)) / 1e8 / supply : 0;
  const usdcPerShare = supply > 0 ? Number(big(s.usdc)) / 1e6 / supply : 0;

  return (
    <section className="card viz-root">
      <div className="card-head">
        <div>
          <h2>Share price</h2>
          <p className="muted small">Estimate: (USDC + cbBTC × BTC) ÷ shares</p>
        </div>
        <div className="headline">
          <div className="muted small">Estimated now</div>
          <div className="headline-value">{current !== null ? usd(current) : "$1.0000"}</div>
          <div className="muted small">
            {current === null
              ? "No shares yet: 1 share per USDC at first"
              : change !== null
                ? `${pctChange(change)} since ${dateOf(first.day)}`
                : `at BTC ${usd(basis!.btcUsd, 0)}`}
          </div>
        </div>
      </div>

      {current !== null && basis && (
        <div className="breakdown">
          <span>Each share holds</span>
          <strong>{btcPerShare.toFixed(8)} cbBTC</strong>
          <span>× BTC {usd(basis.btcUsd, 0)} ({basis.source})</span>
          <span>+</span>
          <strong>{usdcPerShare.toFixed(4)} USDC</strong>
          <span>=</span>
          <strong>{usd(current)}</strong>
        </div>
      )}

      {chart ? (
        <>
          <div className="legend">
            {chart.launched && (
              <span className="legend-item">
                <span className="swatch line s1" /> Actual
              </span>
            )}
            <span className="legend-item">
              <span className="swatch line s1 dashed" /> Simulated (backtest)
            </span>
            <span className="legend-item">
              <span className="swatch band" /> Holding BTC (simulated)
            </span>
          </div>
          <LineChart
            days={chart.days}
            series={[
              { label: "Actual", className: "s1", values: chart.actual },
              { label: "Simulated", className: "s1", values: chart.simulated, dashed: true },
            ]}
            bands={chart.bands}
            marker={{ index: chart.launchIndex, label: chart.launched ? "Launch" : "Today" }}
            format={(v) => usd(v)}
            tick={(v, step) => usd(v, step < 0.001 ? 4 : step < 0.01 ? 3 : 2)}
            ariaLabel={`Simulated share price before the fund started, then the actual share price, now ${
              current !== null ? usd(current) : "not available"
            }`}
            extra={(i) => {
              const p = chart.rest[i - chart.launchIndex - 1];
              if (p) return p.now ? `Estimated now, at BTC ${usd(p.btcUsd, 0)} (${basis!.source})` : `Actual at the fixing price, BTC ${usd(p.btcUsd, 0)}`;
              if (i === chart.launchIndex && chart.launched) return "Launch: 1 share per USDC";
              return `Simulated, holding ${chart.bands[i] ? "BTC" : "USDC"}`;
            }}
          />
          {chart.tracking && <Tracking {...chart.tracking} />}
        </>
      ) : points.length >= 2 ? (
        <LineChart
          days={points.map((p) => p.day)}
          series={[{ label: "Share price", className: "s1", values: points.map((p) => p.price) }]}
          format={(v) => usd(v)}
          tick={(v, step) => usd(v, step < 0.001 ? 4 : step < 0.01 ? 3 : 2)}
          ariaLabel={`Share price at each fixing, now ${current !== null ? usd(current) : "not available"}`}
          extra={(i) =>
            points[i].now
              ? `Estimated now, at BTC ${usd(points[i].btcUsd, 0)} (${basis!.source})`
              : `At the fixing price, BTC ${usd(points[i].btcUsd, 0)}`
          }
        />
      ) : null}

      {points.length > 0 && <History points={points} />}
    </section>
  );
}

/**
 * 模拟部分接实际部分的数据。开始那天是第一次按 1 美元一份发出份额的定价（还没有份额时就是今天）：
 * 回测里第 D 天的收盘价对应第 D+1 天的定价，所以模拟到开始前一天的收盘为止，正好落在开始那天、值为 1。
 * 收盘价还没取到时返回 null。
 */
function joinedChart(snap: Snapshot, points: PricePoint[], closes: DailyClose[] | null) {
  if (!closes) return null;
  const minted = snap.records.filter((r) => !r.mintedShares.isZero()).map((r) => r.epoch.toNumber());
  const launched = minted.length > 0;
  const launch = launched ? Math.min(...minted) : Math.floor(Date.now() / 86_400_000);
  const sim = simulatedSharePrice(closes, launch - 1, SIMULATED_DAYS);
  if (!sim) return null;
  const before = sim.slice(0, SIMULATED_DAYS + 1);
  // 开始之后模拟接着算，按定价日对上实际的点（现在的预估点不是定价，不比）
  const after = new Map(sim.slice(SIMULATED_DAYS + 1).map((p) => [p.day + 1, p]));
  const rest = points.filter((p) => p.day > launch || p.now);
  const simAt = (p: PricePoint) => (p.now ? undefined : after.get(p.day));
  const launchIndex = before.length - 1;
  // 实际和模拟的差：最近一次两者都有的定价
  const last = [...rest].reverse().find((p) => simAt(p));
  const tracking = last && { day: last.day, actual: last.price - 1, simulated: simAt(last)!.strategy - 1 };
  return {
    days: [...before.map((p) => p.day + 1), ...rest.map((p) => p.day)],
    simulated: [...before.map((p) => p.strategy), ...rest.map((p) => simAt(p)?.strategy ?? null)],
    actual: [...before.map((_, i) => (launched && i === launchIndex ? 1 : null)), ...rest.map((p) => p.price)],
    bands: [...before.map((p) => p.inBtc), ...rest.map((p) => simAt(p)?.inBtc ?? false)],
    launchIndex,
    launched,
    rest,
    tracking,
  };
}

/** 开始以来实际和模拟的收益对比：差值大致是真实交易成本（手续费、滑点、分批交易）和回测假设的差别 */
function Tracking({ day, actual, simulated }: { day: number; actual: number; simulated: number }) {
  const gap = actual - simulated;
  const flat = Math.abs(gap) < 0.0005;
  return (
    <div className="tracking">
      <span className="stat-label">Since launch, to the {dateOf(day)} fixing</span>
      <span>
        Actual <strong>{pctChange(actual)}</strong>
      </span>
      <span>
        Simulated <strong>{pctChange(simulated)}</strong>
      </span>
      <span className={`pill ${flat ? "pill-gray" : gap > 0 ? "pill-green" : "pill-red"}`}>
        {flat ? "On track" : `${gap > 0 ? "+" : "−"}${Math.abs(gap * 100).toFixed(2)} pts vs simulated`}
      </span>
    </div>
  );
}

/** 每份价格的历史变动：最上面是现在的预估，往下是每次定价 */
function History({ points }: { points: PricePoint[] }) {
  const [all, setAll] = useState(false);
  const first = points[0];
  const rows = points.map((p, i) => ({
    ...p,
    vsPrevious: i > 0 ? p.price / points[i - 1].price - 1 : null,
    vsFirst: i > 0 ? p.price / first.price - 1 : null,
  }));
  return (
    <>
      <h3 className="history-title">History</h3>
      <div className="table-wrap">
        <table>
          <thead>
            <tr>
              <th>Date</th>
              <th className="num">BTC price</th>
              <th className="num">Share price</th>
              <th className="num">vs previous</th>
              <th className="num">Since {dateOf(first.day)}</th>
            </tr>
          </thead>
          <tbody>
            {[...rows].reverse().slice(0, all ? undefined : 10).map((r) => (
              <tr key={`${r.day}-${r.now}`}>
                <td>
                  {r.now ? (
                    <>
                      Now <span className="pill pill-blue">estimate</span>
                    </>
                  ) : (
                    `${dateOf(r.day)} fixing`
                  )}
                </td>
                <td className="num">{usd(r.btcUsd, 0)}</td>
                <td className="num">{usd(r.price)}</td>
                <td className="num">{pctChange(r.vsPrevious)}</td>
                <td className="num">{pctChange(r.vsFirst)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {rows.length > 10 && (
        <button className="link-btn show-all" onClick={() => setAll(!all)}>
          {all ? "Show the latest 10" : `Show all ${rows.length}`}
        </button>
      )}
    </>
  );
}
