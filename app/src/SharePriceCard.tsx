// 每份价格：(USDC + cbBTC × BTC 价格) ÷ 份额，只用来显示（基金本身从不按美元估值）。
// 历史点用每次定价记录里定价时（当天申购赎回之前）的资产和份额，价格用那次定价的 Pyth 价格；
// 最后一个点用现在的资产和实时 BTC 价格。
import { BN } from "@coral-xyz/anchor";
import { LineChart } from "./LineChart";
import type { Snapshot } from "./chain";

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

export function SharePriceCard({ snap, spot }: { snap: Snapshot; spot: number | null }) {
  const s = snap.fund.state;
  const basis = pricingBasis(snap, spot);
  const history = [...snap.records]
    .reverse()
    .filter((r) => !r.fundShares.isZero())
    .map((r) => ({
      day: r.epoch.toNumber(),
      price: sharePrice(big(r.fundUsdc), big(r.fundBtc), big(r.fundShares), Number(big(r.signalPrice)) / 1e6)!,
      btcUsd: Number(big(r.signalPrice)) / 1e6,
      now: false,
    }));
  const current = basis ? sharePrice(big(s.usdc), big(s.btc), big(s.shareSupply), basis.btcUsd) : null;
  const points =
    current !== null && basis
      ? [...history, { day: Math.floor(Date.now() / 86_400_000), price: current, btcUsd: basis.btcUsd, now: true }]
      : history;
  const first = points[0];
  const change = first && current !== null ? current / first.price - 1 : null;

  return (
    <section className="card viz-root">
      <div className="card-head">
        <div>
          <h2>Share price</h2>
          <p className="muted small">
            (USDC + cbBTC × BTC price) ÷ shares. Indicative: the fund is not valued in USD, and the 1% withdrawal fee is
            not deducted.
          </p>
        </div>
        <div className="headline">
          <div className="headline-value">{current !== null ? usd(current) : "$1.0000"}</div>
          <div className="muted small">
            {current === null
              ? "No shares yet: the first subscription mints 1 share per USDC"
              : change !== null && points.length > 1
                ? `${change >= 0 ? "+" : ""}${(change * 100).toFixed(2)}% since ${new Date(first.day * 86_400_000).toISOString().slice(0, 10)}`
                : `at BTC ${usd(basis!.btcUsd, 0)} (${basis!.source})`}
          </div>
        </div>
      </div>
      {points.length >= 2 ? (
        <LineChart
          days={points.map((p) => p.day)}
          series={[{ label: "Share price", className: "s1", values: points.map((p) => p.price) }]}
          format={(v) => usd(v)}
          tick={(v, step) => usd(v, step < 0.001 ? 4 : step < 0.01 ? 3 : 2)}
          ariaLabel={`Share price at each fixing, now ${current !== null ? usd(current) : "not available"}`}
          extra={(i) =>
            points[i].now
              ? `Now, at BTC ${usd(points[i].btcUsd, 0)} (${basis!.source})`
              : `At the fixing price, BTC ${usd(points[i].btcUsd, 0)}`
          }
        />
      ) : (
        <p className="muted small">
          The curve starts once shares exist: each daily fixing adds a point, and the last point uses the live BTC price.
        </p>
      )}
    </section>
  );
}
