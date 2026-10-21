import { useCallback, useEffect, useMemo, useState } from "react";
import { useAnchorWallet, useConnection } from "@solana/wallet-adapter-react";
import { WalletMultiButton } from "@solana/wallet-adapter-react-ui";
import { BN } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";
import {
  FUND,
  PROGRAM_ID,
  cancel,
  cancelDeadline,
  cancelPending,
  claim,
  describeError,
  fundUsdc,
  loadSnapshot,
  makeProgram,
  recordFor,
  redeem,
  requestEpoch,
  requestFaucet,
  subscribe,
  transferShares,
  type EpochRecord,
  type Fund,
  type Snapshot,
  type Ticket,
  DAY,
} from "./chain";
import * as fmt from "./format";
import { Backtest } from "./BacktestCard";
import { SharePriceCard, pricingBasis, sharePrice } from "./SharePriceCard";
import { SignalCard } from "./SignalCard";
import { fetchDailyCloses, fetchSpot, type DailyClose, type Spot } from "./backtest";
import { nextFixTime } from "./signal";
import { Icon, type IconName } from "./icons";

type Toast = { kind: "ok" | "error" | "busy"; text: string; sig?: string };

const big = (x: BN) => BigInt(x.toString());

export function App() {
  const { connection } = useConnection();
  const wallet = useAnchorWallet();
  const program = useMemo(() => makeProgram(connection, wallet), [connection, wallet]);
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [toast, setToast] = useState<Toast | null>(null);
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));
  const [spotStats, setSpotStats] = useState<Spot | null>(null); // 实时 BTC 价格，只用来显示
  const spot = spotStats?.price ?? null;
  // 每日收盘价：回测和每份价格的模拟部分共用
  const [closes, setCloses] = useState<DailyClose[] | null>(null);
  const [closesError, setClosesError] = useState<string | null>(null);
  useEffect(() => {
    fetchDailyCloses()
      .then(setCloses)
      .catch((e) => setClosesError(String(e?.message ?? e)));
  }, []);

  const refresh = useCallback(async () => {
    try {
      setSnap(await loadSnapshot(program, wallet?.publicKey ?? null));
      setLoadError(null);
    } catch (e) {
      setLoadError(describeError(e));
    }
  }, [program, wallet]);

  useEffect(() => {
    refresh();
    const t = setInterval(refresh, 20_000);
    return () => clearInterval(t);
  }, [refresh]);
  useEffect(() => {
    const t = setInterval(() => setNow(Math.floor(Date.now() / 1000)), 1000);
    return () => clearInterval(t);
  }, []);
  useEffect(() => {
    const load = () => fetchSpot().then((p) => p && setSpotStats(p));
    load();
    const t = setInterval(load, 60_000);
    return () => clearInterval(t);
  }, []);

  /** Send one transaction, show its progress and result, then refresh */
  const run = useCallback(
    async (label: string, send: () => Promise<string>) => {
      setToast({ kind: "busy", text: `${label}: waiting for the wallet signature and confirmation…` });
      try {
        const sig = await send();
        setToast({ kind: "ok", text: `${label}: done`, sig });
      } catch (e) {
        setToast({ kind: "error", text: `${label} failed: ${describeError(e)}` });
      }
      await refresh();
    },
    [refresh],
  );

  return (
    <div className="page">
      <Header spot={spotStats} />
      <main className="container">
        {loadError && !snap && <div className="notice error">Could not load the fund: {loadError}</div>}
        {!snap && !loadError && <div className="notice">Loading on-chain data…</div>}
        {snap && (
          <>
            <VaultHero snap={snap} />
            <div className="grid">
              <div className="col-main">
                <Overview snap={snap} spot={spot} />
                <SignalCard snap={snap} spot={spot} now={now} />
                <SharePriceCard snap={snap} spot={spot} closes={closes} />
                <Backtest closes={closes} error={closesError} />
                <Strategy snap={snap} now={now} />
                <Records records={snap.records} window={snap.fund.config.window} />
              </div>
              <div className="col-side">
                <Faucet snap={snap} run={run} />
                <ActionPanel snap={snap} now={now} spot={spot} run={run} program={program} />
                <Position snap={snap} now={now} spot={spot} run={run} program={program} />
              </div>
            </div>
          </>
        )}
        {toast && <ToastView toast={toast} onClose={() => setToast(null)} />}
      </main>
      <footer className="footer container">
        <span>Devnet prototype: prices from Pyth, trades on a mock exchange. Test tokens only. Not investment advice, not audited, may contain bugs.</span>
        <a href={fmt.explorerAddress(PROGRAM_ID.toBase58())} target="_blank" rel="noreferrer">
          Program {fmt.short(PROGRAM_ID.toBase58())}
        </a>
      </footer>
    </div>
  );
}

function Header({ spot }: { spot: Spot | null }) {
  return (
    <header className="header">
      <div className="container header-inner">
        <div className="brand">
          <svg width="22" height="22" viewBox="0 0 24 24" aria-hidden="true">
            <path d="M2 14c3-4 6-4 9 0s6 4 9 0" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" />
            <path d="M2 8c3-4 6-4 9 0s6 4 9 0" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" opacity=".45" />
          </svg>
          <span>Tide</span>
        </div>
        <nav className="nav">
          <span className="nav-item active">Vault</span>
        </nav>
        <div className="header-right">
          {spot && (
            <span className="btc-price" title="BTC/USD now (Coinbase) and its 24-hour change">
              <span className="muted">BTC</span> ${spot.price.toLocaleString("en-US", { maximumFractionDigits: 0 })}
              {spot.change24h !== null && (
                <span className={`delta ${spot.change24h >= 0 ? "up" : "down"}`}>
                  {spot.change24h >= 0 ? "▲" : "▼"} {Math.abs(spot.change24h * 100).toFixed(2)}%
                </span>
              )}
            </span>
          )}
          <span className="pill">Devnet</span>
          <WalletMultiButton />
        </div>
      </div>
    </header>
  );
}

function VaultHero({ snap }: { snap: Snapshot }) {
  const cfg = snap.fund.config;
  return (
    <section className="hero">
      <div className="hero-title">
        <div className="token-icon">₿</div>
        <h1>Tide BTC Trend</h1>
      </div>
      <div className="tags">
        <span className="tag">Withdrawal fee {cfg.withdrawalFeeBps / 100}%</span>
        <a className="tag link" href={fmt.explorerAddress(FUND.toBase58())} target="_blank" rel="noreferrer">
          Vault {fmt.short(FUND.toBase58())} ↗
        </a>
      </div>
    </section>
  );
}

function Overview({ snap, spot }: { snap: Snapshot; spot: number | null }) {
  const s = snap.fund.state;
  const basis = pricingBasis(snap, spot);
  // display only: the fund itself is never valued in USD
  const usdc = fundUsdc(snap); // 含借出的
  const total = basis ? Number(usdc) / 1e6 + (Number(big(s.btc)) / 1e8) * basis.btcUsd : null;
  const price = basis ? sharePrice(usdc, big(s.btc), big(s.shareSupply), basis.btcUsd) : null;
  // queued and partly filled subscriptions: still the subscribers' USDC, not fund assets
  const waiting =
    big(s.queuedSubscriptionUsdc) +
    big(s.nextSubscriptionUsdc) +
    s.pending.slice(0, s.pendingCount).reduce((x: bigint, p: { usdc: BN }) => x + big(p.usdc), 0n);
  return (
    <section className="card">
      <div className="stats five">
        <Stat icon="tag" label="Share price" value={price !== null ? `$${price.toFixed(4)}` : "$1.0000"} sub="estimate" />
        <Stat
          icon="wallet"
          label="Total value"
          value={total !== null ? `$${total.toLocaleString("en-US", { maximumFractionDigits: 2 })}` : "—"}
          sub={`${fmt.shares(s.shareSupply)} shares`}
        />
        <Stat icon="btc" label="cbBTC" value={fmt.btc(s.btc)} />
        <Stat
          icon="usd"
          label="USDC"
          value={fmt.usdc(usdc)}
          sub={
            [
              snap.lentUsdc > 0n && `${fmt.usdc(snap.lentUsdc)} lent · ${(snap.lendingApyBps / 100).toFixed(1)}% APY`,
              waiting > 0n && `+${fmt.usdc(waiting)} queued`,
            ]
              .filter(Boolean)
              .join(" · ") || undefined
          }
        />
        <Stat
          icon="target"
          label="Holds"
          value={<span className={`pill ${s.targetBtc ? "pill-blue" : "pill-gray"}`}>{s.targetBtc ? "cbBTC" : "USDC"}</span>}
        />
      </div>
    </section>
  );
}

function Strategy({ snap, now }: { snap: Snapshot; now: number }) {
  const { fund, records } = snap;
  const cfg = fund.config;
  const s = fund.state;
  const nextFix = nextFixTime(fund, now);
  const last = records[0];
  const pending = s.pending.slice(0, s.pendingCount).reduce((x: bigint, p: { usdc: BN }) => x + big(p.usdc), 0n);
  const today = Math.floor(now / DAY);
  const todayFix = today * DAY + cfg.settleOffsetSecs.toNumber();
  // 结算每天定价后几分钟内由定时任务运行；过了一小时还没定价就是出了问题
  const overdue = now > todayFix + 3600 && s.lastFixDay.toNumber() < today;
  // 还要交易的量：目标 cbBTC 时 USDC 还没买完，目标 USDC 时 cbBTC 还没卖完（不到 1 USDC / 1 聪的尘埃不算）
  const toBuy = s.targetBtc ? fundUsdc(snap) + pending : 0n;
  const toSell = s.targetBtc ? 0n : big(s.btc);
  const rebalancing = toBuy >= 1_000_000n || toSell > 0n || s.sliceCount > 0;
  const outstanding = big(s.queuedSubscriptionUsdc) + big(s.nextSubscriptionUsdc) + pending;
  const cap = big(cfg.subscriptionCapUsdc);
  const capPct = cap > 0n ? Number((outstanding * 10_000n) / cap) / 100 : 0;
  const signal = last ? Number(big(last.signalPrice)) : 0;
  const ma = last ? Number(big(last.movingAverage)) : 0;
  const settlement = overdue ? (
    <span className="pill pill-amber">Not fixed yet</span>
  ) : rebalancing ? (
    <span className="pill pill-amber">Trading</span>
  ) : (
    <span className="pill pill-green">Done</span>
  );
  const left = rebalancing
    ? [
        s.sliceCount > 0 && `${s.sliceCount} slice${s.sliceCount > 1 ? "s" : ""} selling`,
        toBuy > 0n ? `${fmt.usdc(toBuy)} USDC to buy` : toSell > 0n ? `${fmt.btc(toSell)} cbBTC to sell` : "",
      ]
        .filter(Boolean)
        .join(", ")
    : undefined;
  return (
    <section className="card">
      <h2>Fund status</h2>
      <div className="stats three">
        <Stat
          icon="chart"
          label={last ? `Fixing ${fmt.dayLabel(last.epoch.toNumber()).slice(5)}` : "Last fixing"}
          value={last ? fmt.price(last.signalPrice) : "—"}
          sub={
            ma > 0 && (
              <span className={`delta ${signal > ma ? "up" : "down"}`}>
                {signal > ma ? "▲" : "▼"} {Math.abs(((signal - ma) / ma) * 100).toFixed(2)}% vs average
              </span>
            )
          }
        />
        <Stat icon="average" label={`${cfg.window}-day average`} value={ma > 0 ? fmt.price(last!.movingAverage) : "—"} />
        <Stat icon="clock" label="Next fixing" value={fmt.countdown(nextFix - now)} sub={fmt.timeLabel(nextFix)} />
        <Stat icon="sync" label="Settlement" value={settlement} sub={left} />
        <Stat
          icon="gauge"
          label="Subscription cap"
          value={`${capPct.toFixed(1)}%`}
          sub={
            <>
              {fmt.usdc(outstanding)} / {fmt.usdc(cap)} USDC
              <div className="bar">
                <div className="bar-fill" style={{ width: `${Math.min(capPct, 100)}%` }} />
              </div>
            </>
          }
        />
      </div>
      <div className="mini-stats">
        {s.subscriptionsPaused && <span className="pill pill-amber">Subscriptions paused</span>}
        <span>Unsold slices {s.sliceCount}</span>
        <span>Pending subscriptions {s.pendingCount}</span>
        <span>Fees owed {fmt.usdc(s.feesOwed)} USDC</span>
      </div>
    </section>
  );
}

function Records({ records, window }: { records: EpochRecord[]; window: number }) {
  return (
    <section className="card">
      <h2>Fixings</h2>
      {records.length === 0 ? (
        <p className="muted">No fixings yet.</p>
      ) : (
        <table className="fit-table">
          <thead>
            <tr>
              <th>Date</th>
              <th className="num">Price</th>
              <th className="num hide-sm">{window}d avg</th>
              <th>Holds</th>
              <th className="num">Flows</th>
              <th>Status</th>
            </tr>
          </thead>
          <tbody>
            {records.slice(0, 20).map((r, i) => {
              const prev = records[i + 1]; // 新的在前，下一行是前一次定价
              const gap = r.movingAverage.isZero() ? null : r.signalPrice.toNumber() / r.movingAverage.toNumber() - 1;
              const idle = r.subscriptionUsdc.isZero() && r.redemptionShares.isZero();
              const date = fmt.dayLabel(r.epoch.toNumber());
              return (
                <tr key={r.epoch.toString()}>
                  <td title={date}>{date.slice(5)}</td>
                  <td className="num">
                    {fmt.price(r.signalPrice)}
                    {gap !== null && (
                      <span className={`delta ${gap > 0 ? "up" : "down"}`}>
                        {gap > 0 ? "+" : "−"}
                        {(Math.abs(gap) * 100).toFixed(1)}%
                      </span>
                    )}
                  </td>
                  <td className="num hide-sm">{r.movingAverage.isZero() ? "—" : fmt.price(r.movingAverage)}</td>
                  <td>
                    <span className={`pill ${r.targetBtc ? "pill-blue" : "pill-gray"}`}>{r.targetBtc ? "cbBTC" : "USDC"}</span>
                    {prev && prev.targetBtc !== r.targetBtc && (
                      <span className="switch" title="Switched at this fixing">
                        ⇄
                      </span>
                    )}
                  </td>
                  <td className="num">
                    {idle ? (
                      <span className="muted">—</span>
                    ) : (
                      <>
                        {!r.subscriptionUsdc.isZero() && <div className="up">+{fmt.usdc(r.subscriptionUsdc)} USDC</div>}
                        {!r.redemptionShares.isZero() && <div className="down">−{fmt.shares(r.redemptionShares)} sh</div>}
                      </>
                    )}
                  </td>
                  <td>
                    {idle ? (
                      <span className="muted">—</span>
                    ) : !r.subscriptionFilled ? (
                      <span className="pill pill-amber">Buying</span>
                    ) : !r.sliceSoldOut ? (
                      <span className="pill pill-amber">Selling</span>
                    ) : (
                      <span className="pill pill-green">Filled</span>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
    </section>
  );
}

type RunFn = (label: string, send: () => Promise<string>) => Promise<void>;
type Prog = ReturnType<typeof makeProgram>;

function ActionPanel({
  snap,
  now,
  spot,
  run,
  program,
}: {
  snap: Snapshot;
  now: number;
  spot: number | null;
  run: RunFn;
  program: Prog;
}) {
  const wallet = useAnchorWallet();
  const [tab, setTab] = useState<"sub" | "red">("sub");
  const [text, setText] = useState("");
  const cfg = snap.fund.config;
  const offset = cfg.settleOffsetSecs.toNumber();
  const epoch = requestEpoch(now, offset);
  const decimals = 6;
  const amount = fmt.parseAmount(text, decimals);
  const balance = tab === "sub" ? snap.usdcBalance : snap.shareBalance;
  const min = big(cfg.minSubscription);
  // 赎回要先有份额：申购定价成交、领取之后份额才到钱包
  const lastFix = snap.fund.state.lastFixDay.toNumber();
  const waiting = snap.tickets.filter((t) => t.epoch.toNumber() > lastFix && !t.subscriptionUsdc.isZero());
  const waitingUsdc = waiting.reduce((x, t) => x + big(t.subscriptionUsdc), 0n);
  const toClaim = snap.tickets.some((t) => !t.subscriptionUsdc.isZero() && viewTicket(t, snap.fund, snap.records, now).canClaim);
  const noShares = tab === "red" && !!wallet && snap.shareBalance === 0n;
  const fixAt = epoch * DAY + offset;
  // 只是参考：真正的份额和赎回款按实际成交计算
  const basis = pricingBasis(snap, spot);
  const s = snap.fund.state;
  const price = basis ? sharePrice(fundUsdc(snap), big(s.btc), big(s.shareSupply), basis.btcUsd) ?? 1 : null;
  const estimate =
    amount && amount > 0n && price && !noShares
      ? tab === "sub"
        ? `≈ ${fmt.shares(BigInt(Math.floor((Number(amount) * (1 - cfg.subscriptionFeeBps / 10_000)) / price)))} shares @ $${price.toFixed(4)}`
        : `≈ ${fmt.usdc(BigInt(Math.floor(Number(amount) * price * (1 - cfg.withdrawalFeeBps / 10_000))))} USDC after fee`
      : null;
  const problem = !wallet
    ? "Connect a wallet"
    : noShares
      ? "No shares yet"
      : amount === null || amount === 0n
      ? "Enter an amount"
      : amount > balance
        ? "Insufficient balance"
        : tab === "sub" && amount < min
          ? `Minimum ${fmt.usdc(min)} USDC`
          : tab === "sub" && snap.fund.state.subscriptionsPaused
            ? "Subscriptions are paused"
            : null;

  const submit = () => {
    if (!wallet || amount === null) return;
    const label = tab === "sub" ? `Subscribe ${fmt.usdc(amount)} USDC` : `Redeem ${fmt.shares(amount)} shares`;
    run(label, () =>
      tab === "sub"
        ? subscribe(program, snap.fund, wallet.publicKey, amount, epoch)
        : redeem(program, wallet.publicKey, amount, epoch),
    ).then(() => setText(""));
  };

  return (
    <section className="card action">
      <div className="tabs">
        <button className={tab === "sub" ? "tab active" : "tab"} onClick={() => setTab("sub")}>
          Subscribe
        </button>
        <button className={tab === "red" ? "tab active" : "tab"} onClick={() => setTab("red")}>
          Redeem
        </button>
      </div>
      <div className="input-box">
        <div className="input-top">
          <span className="muted small">{tab === "sub" ? "You pay" : "Shares to redeem"}</span>
          <span className="muted small">
            Balance {tab === "sub" ? `${fmt.usdc(balance)} USDC` : `${fmt.shares(balance)} shares`}
            {balance > 0n && (
              <button className="link-btn" onClick={() => setText((Number(balance) / 1e6).toString())}>
                Max
              </button>
            )}
          </span>
        </div>
        <div className="input-row">
          <input inputMode="decimal" placeholder="0.00" value={text} onChange={(e) => setText(e.target.value)} />
          <span className="unit">{tab === "sub" ? "USDC" : "shares"}</span>
        </div>
      </div>
      <div className="rows compact">
        {estimate && <Row label="Estimate">{estimate}</Row>}
        <Row label="Fixing">
          {fmt.timeLabel(fixAt)} · in {fmt.countdown(fixAt - now)}
        </Row>
        {tab === "sub" ? (
          <>
            <Row label="Minimum">{fmt.usdc(min)} USDC</Row>
            {cfg.subscriptionFeeBps > 0 && <Row label="Subscription fee">{cfg.subscriptionFeeBps / 100}%</Row>}
          </>
        ) : (
          <Row label="Withdrawal fee">{cfg.withdrawalFeeBps / 100}%</Row>
        )}
      </div>
      {noShares && (
        <p className="hint">
          {toClaim
            ? 'Your subscription has filled. Claim the shares under "Your position" first, then redeem them here.'
            : waiting.length > 0
              ? `Your ${fmt.usdc(waitingUsdc)} USDC subscription fills after the ${fmt.timeLabel(
                  Math.max(...waiting.map((t) => t.epoch.toNumber())) * DAY + offset,
                )} fixing. Then claim the shares under "Your position" and redeem them here.`
              : "You have no shares yet. Subscribe first; after the next fixing, claim your shares and redeem them here."}
        </p>
      )}
      <button className="btn primary" disabled={problem !== null} onClick={submit}>
        {problem ?? (tab === "sub" ? "Subscribe" : "Redeem")}
      </button>
    </section>
  );
}

interface TicketView {
  epoch: number;
  record?: EpochRecord;
  status: string;
  tone: "blue" | "amber" | "green" | "gray";
  canCancel: boolean;
  canCancelPending: boolean;
  canClaim: boolean;
  detail: string[];
  sharesToClaim: bigint; // 现在领取能拿到的份额（估算）
  usdcToClaim: bigint; // 现在领取能拿到的 USDC：赎回款扣掉取款费，加上退款（估算）
  queuedUsdc: bigint; // 还没变成份额的申购款
  queuedShares: bigint; // 还没卖出的赎回份额
}

const perShare = (usdc: bigint, shares: bigint) => (shares > 0n ? `$${(Number(usdc) / Number(shares)).toFixed(4)}` : "—");

/** What a ticket is waiting for (display only; the program computes the real amounts with the same rounding) */
function viewTicket(t: Ticket, fund: Fund, records: EpochRecord[], now: number): TicketView {
  const epoch = t.epoch.toNumber();
  const sub = big(t.subscriptionUsdc);
  const red = big(t.redemptionShares);
  const detail: string[] = [];
  const offset = fund.config.settleOffsetSecs.toNumber();
  const none = { sharesToClaim: 0n, usdcToClaim: 0n, queuedUsdc: sub, queuedShares: red };
  if (epoch > fund.state.lastFixDay.toNumber()) {
    const deadline = cancelDeadline(epoch, now, offset);
    const open = now < deadline;
    if (sub > 0n) detail.push(`Subscribing ${fmt.usdc(sub)} USDC`);
    if (red > 0n) detail.push(`Redeeming ${fmt.shares(red)} shares`);
    detail.push(open ? `Fixing ${fmt.timeLabel(deadline)}; cancel before then` : "Waiting for the settlement to fix it");
    return {
      epoch,
      status: open ? `Queued, fixing in ${fmt.countdown(deadline - now)}` : "Waiting for the fixing",
      tone: "blue",
      canCancel: open,
      canCancelPending: false,
      canClaim: false,
      detail,
      ...none,
    };
  }
  const record = recordFor(records, epoch);
  if (!record)
    return { epoch, status: "Fixing record not found", tone: "gray", canCancel: false, canCancelPending: false, canClaim: false, detail, ...none };
  let sharesToClaim = 0n;
  let usdcToClaim = 0n;
  let queuedUsdc = 0n;
  let queuedShares = 0n;
  let pending = false;
  if (sub > 0n) {
    const basis = big(record.subscriptionUsdc) - big(record.cancelledSubscriptionUsdc);
    const minted = big(record.mintedShares) - big(record.cancelledShares);
    const entitled = basis > 0n ? (minted * sub) / basis : 0n;
    const claimed = big(t.sharesClaimed);
    if (record.subscriptionFilled) {
      // 最后一位领取人拿走剩下的全部
      const unclaimedSub = big(record.unclaimedSubscriptionUsdc);
      const last = sub === unclaimedSub;
      sharesToClaim = last ? big(record.unclaimedShares) : entitled - claimed;
      const refund = unclaimedSub > 0n ? (big(record.unclaimedRefundUsdc) * sub) / unclaimedSub : 0n;
      usdcToClaim += refund;
      detail.push(`${fmt.usdc(sub)} USDC became ${fmt.shares(claimed + sharesToClaim)} shares (${perShare(sub, claimed + sharesToClaim)} per share)`);
      if (claimed > 0n) detail.push(`${fmt.shares(claimed)} already claimed`);
      if (refund > 0n) detail.push(`${fmt.usdc(refund)} USDC refund`);
    } else {
      pending = true;
      sharesToClaim = entitled - claimed;
      const unfilled = fund.state.pending.slice(0, fund.state.pendingCount).find((p: { epoch: BN }) => p.epoch.eq(record.epoch));
      queuedUsdc = unfilled && basis > 0n ? (big(unfilled.usdc) * sub) / basis : 0n;
      detail.push(`Subscribed ${fmt.usdc(sub)} USDC: ${fmt.shares(entitled)} shares so far, ${fmt.usdc(queuedUsdc)} USDC still to buy`);
    }
  }
  if (red > 0n) {
    if (record.sliceSoldOut) {
      const unclaimedRed = big(record.unclaimedRedemptionShares);
      const gross = unclaimedRed > 0n ? (big(record.unclaimedPayoutUsdc) * red) / unclaimedRed : 0n;
      const fee = (gross * BigInt(fund.config.withdrawalFeeBps)) / 10_000n;
      usdcToClaim += gross - fee;
      detail.push(
        `${fmt.shares(red)} shares sold for ${fmt.usdc(gross)} USDC (${perShare(gross, red)} per share); you get ${fmt.usdc(gross - fee)} after the ${fund.config.withdrawalFeeBps / 100}% fee`,
      );
    } else {
      queuedShares = red;
      detail.push(`Redeeming ${fmt.shares(red)} shares: the slice is still selling`);
    }
  }
  const canClaim = sharesToClaim > 0n || usdcToClaim > 0n || (sub > 0n && record.subscriptionFilled) || (red > 0n && record.sliceSoldOut);
  return {
    epoch,
    record,
    status: canClaim ? "Ready to claim" : pending ? "Filling" : "Selling",
    tone: canClaim ? "green" : "amber",
    canCancel: false,
    canCancelPending: pending,
    canClaim,
    detail,
    sharesToClaim,
    usdcToClaim,
    queuedUsdc: pending ? queuedUsdc : 0n,
    queuedShares,
  };
}

function Position({
  snap,
  now,
  spot,
  run,
  program,
}: {
  snap: Snapshot;
  now: number;
  spot: number | null;
  run: RunFn;
  program: Prog;
}) {
  const wallet = useAnchorWallet();
  if (!wallet) {
    return (
      <section className="card">
        <h2>Your position</h2>
        <p className="muted">Connect a wallet to see your shares and tickets.</p>
      </section>
    );
  }
  const user = wallet.publicKey;
  const basis = pricingBasis(snap, spot);
  const s = snap.fund.state;
  const price = basis ? sharePrice(fundUsdc(snap), big(s.btc), big(s.shareSupply), basis.btcUsd) : null;
  const views = snap.tickets.map((t) => viewTicket(t, snap.fund, snap.records, now));
  const sum = (f: (v: TicketView) => bigint) => views.reduce((x, v) => x + f(v), 0n);
  const toClaimShares = sum((v) => v.sharesToClaim);
  const toClaimUsdc = sum((v) => v.usdcToClaim);
  const queuedUsdc = sum((v) => v.queuedUsdc);
  const queuedShares = sum((v) => v.queuedShares);
  const owned = snap.shareBalance + toClaimShares + queuedShares; // 还没卖出的赎回份额也还是你的
  const value = price !== null ? (Number(owned) / 1e6) * price : null;
  const lowSol = snap.solBalance !== null && snap.solBalance < 5_000_000; // 少于 0.005 SOL 可能付不起交易费
  return (
    <section className="card">
      <h2>Your position</h2>
      <div className="stats two">
        <Stat icon="shares" label="Shares in wallet" value={fmt.shares(snap.shareBalance)} />
        <Stat icon="usd" label="USDC balance" value={fmt.usdc(snap.usdcBalance)} />
        {value !== null && owned > 0n && (
          <Stat
            icon="wallet"
            label="Your value"
            value={`≈ $${value.toLocaleString("en-US", { maximumFractionDigits: 2 })}`}
            sub={`${fmt.shares(owned)} shares`}
          />
        )}
        {(toClaimShares > 0n || toClaimUsdc > 0n) && (
          <Stat
            label="Ready to claim"
            value={
              <span className="claimable">
                {toClaimShares > 0n && <span>{fmt.shares(toClaimShares)} shares</span>}
                {toClaimUsdc > 0n && <span>{fmt.usdc(toClaimUsdc)} USDC</span>}
              </span>
            }
          />
        )}
        {(queuedUsdc > 0n || queuedShares > 0n) && (
          <Stat
            label="In the queue"
            value={
              <span className="claimable">
                {queuedUsdc > 0n && <span>{fmt.usdc(queuedUsdc)} USDC</span>}
                {queuedShares > 0n && <span>{fmt.shares(queuedShares)} shares</span>}
              </span>
            }
          />
        )}
      </div>
      {toClaimShares > 0n && snap.shareBalance === 0n && (
        <p className="hint">Your shares are ready. Press Claim below to move them to your wallet.</p>
      )}
      {snap.solBalance !== null && (
        <p className={`small ${lowSol ? "hint" : "muted"}`}>
          {(snap.solBalance / 1e9).toLocaleString("en-US", { maximumFractionDigits: 4 })} SOL
          {lowSol && " · low, get devnet SOL from faucet.solana.com"}
        </p>
      )}
      <TransferShares balance={snap.shareBalance} run={run} program={program} />
      {snap.tickets.length === 0 ? (
        <p className="muted small">No tickets.</p>
      ) : (
        <ul className="tickets">
          {snap.tickets.map((t, i) => {
            const v = views[i];
            return (
              <li key={t.address.toBase58()} className="ticket">
                <div className="ticket-top">
                  <span className="ticket-epoch">Epoch {v.epoch} · {fmt.dayLabel(v.epoch)}</span>
                  <span className={`pill pill-${v.tone}`}>{v.status}</span>
                </div>
                {v.detail.map((d) => (
                  <div key={d} className="muted small">
                    {d}
                  </div>
                ))}
                <div className="ticket-actions">
                  {v.canClaim && v.record && (
                    <button className="btn small primary" onClick={() => run("Claim", () => claim(program, snap.fund, user, t, v.record!))}>
                      Claim
                    </button>
                  )}
                  {v.canCancel && (
                    <button className="btn small" onClick={() => run("Cancel", () => cancel(program, snap.fund, user, t))}>
                      Cancel
                    </button>
                  )}
                  {v.canCancelPending && v.record && (
                    <button
                      className="btn small"
                      onClick={() => run("Cancel the unfilled part", () => cancelPending(program, snap.fund, user, t, v.record!))}
                    >
                      Cancel unfilled part
                    </button>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

function Faucet({ snap, run }: { snap: Snapshot; run: RunFn }) {
  const wallet = useAnchorWallet();
  if (!wallet) return null;
  // hidden once the wallet holds USDC or shares (one claim per address)
  if (snap.usdcBalance > 0n || snap.shareBalance > 0n || snap.tickets.length > 0) return null;
  return (
    <section className="card highlight">
      <h3>Test USDC</h3>
      <button
        className="btn primary"
        onClick={() =>
          run("Claim 10,000 test USDC", async () => (await requestFaucet(wallet.publicKey)).signature)
        }
      >
        Claim 10,000 test USDC
      </button>
    </section>
  );
}

function TransferShares({ balance, run, program }: { balance: bigint; run: RunFn; program: Prog }) {
  const wallet = useAnchorWallet();
  const [to, setTo] = useState("");
  const [text, setText] = useState("");
  if (!wallet || balance === 0n) return null;
  const amount = fmt.parseAmount(text, 6);
  let dest: PublicKey | null = null;
  try {
    dest = to.trim() ? new PublicKey(to.trim()) : null;
  } catch {
    dest = null;
  }
  const problem = !dest
    ? "Enter a recipient address"
    : dest.equals(wallet.publicKey)
      ? "Cannot send to yourself"
      : amount === null || amount === 0n
        ? "Enter an amount"
        : amount > balance
          ? "Not enough shares"
          : null;
  return (
    <div className="transfer">
      <h3>Send shares</h3>
      <input className="field" placeholder="Recipient wallet address" value={to} onChange={(e) => setTo(e.target.value)} />
      <div className="field-row">
        <input className="field" inputMode="decimal" placeholder="Shares" value={text} onChange={(e) => setText(e.target.value)} />
        <button className="link-btn" onClick={() => setText((Number(balance) / 1e6).toString())}>
          Max
        </button>
      </div>
      <button
        className="btn"
        disabled={problem !== null}
        onClick={() =>
          dest &&
          amount !== null &&
          run(`Send ${fmt.shares(amount)} shares`, () => transferShares(program, wallet.publicKey, dest!, amount)).then(() => {
            setText("");
            setTo("");
          })
        }
      >
        {problem ?? "Send"}
      </button>
    </div>
  );
}

function ToastView({ toast, onClose }: { toast: Toast; onClose: () => void }) {
  return (
    <div className={`toast ${toast.kind}`}>
      <span>{toast.text}</span>
      {toast.sig && (
        <a href={fmt.explorer(toast.sig)} target="_blank" rel="noreferrer">
          View transaction ↗
        </a>
      )}
      {toast.kind !== "busy" && (
        <button className="link-btn" onClick={onClose}>
          Close
        </button>
      )}
    </div>
  );
}

function Stat({ icon, label, value, sub }: { icon?: IconName; label: string; value: React.ReactNode; sub?: React.ReactNode }) {
  return (
    <div className="stat">
      <div className="stat-label">
        {icon && <Icon name={icon} />}
        {label}
      </div>
      <div className="stat-value">{value}</div>
      {sub && <div className="stat-sub">{sub}</div>}
    </div>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="row">
      <span className="muted">{label}</span>
      <span className="row-value">{children}</span>
    </div>
  );
}
