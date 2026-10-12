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
import { fetchSpot, type Spot } from "./backtest";

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
                <SharePriceCard snap={snap} spot={spot} />
                <Backtest />
                <Strategy snap={snap} now={now} />
                <Records records={snap.records} />
              </div>
              <div className="col-side">
                <Faucet snap={snap} run={run} />
                <ActionPanel snap={snap} now={now} run={run} program={program} />
                <Position snap={snap} now={now} spot={spot} run={run} program={program} />
                <Guide snap={snap} />
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
  const feed = "cbBtc" in cfg.signalFeed ? "cbBTC/USD" : "BTC/USD";
  return (
    <section className="hero">
      <div className="hero-title">
        <div className="token-icon">₿</div>
        <div>
          <h1>Tide BTC Trend</h1>
          <p className="muted">Holds cbBTC when BTC is above its {cfg.window}-day moving average and USDC when it is below. One fixing a day; shares follow actual fills.</p>
        </div>
      </div>
      <div className="tags">
        <span className="tag">Signal {feed} (Pyth)</span>
        <span className="tag">{cfg.window}-day average</span>
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
  const total = basis ? Number(big(s.usdc)) / 1e6 + (Number(big(s.btc)) / 1e8) * basis.btcUsd : null;
  const price = basis ? sharePrice(big(s.usdc), big(s.btc), big(s.shareSupply), basis.btcUsd) : null;
  return (
    <section className="card">
      <div className="stats five">
        <Stat label="Share price" value={price !== null ? `$${price.toFixed(4)}` : "$1.0000"} />
        <Stat label="Total value" value={total !== null ? `$${total.toLocaleString("en-US", { maximumFractionDigits: 2 })}` : "—"} />
        <Stat label="cbBTC held" value={fmt.btc(s.btc)} />
        <Stat label="USDC held" value={fmt.usdc(s.usdc)} />
        <Stat
          label="Target"
          value={<span className={`pill ${s.targetBtc ? "pill-blue" : "pill-gray"}`}>{s.targetBtc ? "cbBTC" : "USDC"}</span>}
        />
      </div>
      <p className="muted small">
        {fmt.shares(s.shareSupply)} shares outstanding.
        {basis && ` Valued at BTC $${basis.btcUsd.toLocaleString("en-US", { maximumFractionDigits: 0 })} (${basis.source}).`}{" "}
        Indicative only: the fund is not valued in USD.
      </p>
    </section>
  );
}

function Strategy({ snap, now }: { snap: Snapshot; now: number }) {
  const { fund, records } = snap;
  const cfg = fund.config;
  const s = fund.state;
  const offset = cfg.settleOffsetSecs.toNumber();
  const today = Math.floor(now / DAY);
  const nextFix = now < today * DAY + offset ? today * DAY + offset : (today + 1) * DAY + offset;
  const last = records[0];
  const pending = s.pending.slice(0, s.pendingCount).reduce((x: bigint, p: { usdc: BN }) => x + big(p.usdc), 0n);
  const outstanding = big(s.queuedSubscriptionUsdc) + big(s.nextSubscriptionUsdc) + pending;
  const cap = big(cfg.subscriptionCapUsdc);
  const capPct = cap > 0n ? Number((outstanding * 10_000n) / cap) / 100 : 0;
  const signal = last ? Number(big(last.signalPrice)) : 0;
  const ma = last ? Number(big(last.movingAverage)) : 0;
  return (
    <section className="card">
      <h2>Fund status</h2>
      <div className="rows">
        <Row label="Last fixing">{last ? `${fmt.dayLabel(last.epoch.toNumber())} (epoch ${last.epoch.toNumber()})` : "None yet"}</Row>
        <Row label="Signal / moving average">
          {last ? (
            <span>
              {fmt.price(last.signalPrice)} / {ma > 0 ? fmt.price(last.movingAverage) : "not enough prices"}
              {ma > 0 && (
                <span className={`delta ${signal > ma ? "up" : "down"}`}>
                  {signal > ma ? "▲" : "▼"} {(((signal - ma) / ma) * 100).toFixed(2)}%
                </span>
              )}
            </span>
          ) : (
            "—"
          )}
        </Row>
        <Row label="Next fixing">
          {fmt.timeLabel(nextFix)}
          <span className="muted"> (in {fmt.countdown(nextFix - now)})</span>
        </Row>
        <Row label="Subscriptions">
          {s.subscriptionsPaused ? <span className="pill pill-amber">Paused</span> : <span className="pill pill-green">Open</span>}
        </Row>
        <Row label="Subscription cap used">
          <span>
            {fmt.usdc(outstanding)} / {fmt.usdc(cap)} USDC
          </span>
        </Row>
      </div>
      <div className="bar">
        <div className="bar-fill" style={{ width: `${Math.min(capPct, 100)}%` }} />
      </div>
      <div className="mini-stats">
        <span>Unsold redemption slices: {s.sliceCount}</span>
        <span>Pending subscriptions: {s.pendingCount}</span>
        <span>Fees owed to the manager: {fmt.usdc(s.feesOwed)} USDC</span>
      </div>
    </section>
  );
}

function Records({ records }: { records: EpochRecord[] }) {
  return (
    <section className="card">
      <h2>Fixings</h2>
      {records.length === 0 ? (
        <p className="muted">No fixings yet.</p>
      ) : (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Date</th>
                <th className="num">Signal</th>
                <th className="num">Average</th>
                <th>Target</th>
                <th className="num">Subscribed USDC</th>
                <th className="num">Redeemed shares</th>
                <th>Status</th>
              </tr>
            </thead>
            <tbody>
              {records.slice(0, 20).map((r) => (
                <tr key={r.epoch.toString()}>
                  <td>{fmt.dayLabel(r.epoch.toNumber())}</td>
                  <td className="num">{fmt.price(r.signalPrice)}</td>
                  <td className="num">{r.movingAverage.isZero() ? "—" : fmt.price(r.movingAverage)}</td>
                  <td>
                    <span className={`pill ${r.targetBtc ? "pill-blue" : "pill-gray"}`}>{r.targetBtc ? "cbBTC" : "USDC"}</span>
                  </td>
                  <td className="num">{fmt.usdc(r.subscriptionUsdc)}</td>
                  <td className="num">{fmt.shares(r.redemptionShares)}</td>
                  <td>
                    {r.subscriptionFilled && r.sliceSoldOut ? (
                      <span className="pill pill-green">Done</span>
                    ) : (
                      <span className="pill pill-amber">Filling</span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

type RunFn = (label: string, send: () => Promise<string>) => Promise<void>;
type Prog = ReturnType<typeof makeProgram>;

function ActionPanel({ snap, now, run, program }: { snap: Snapshot; now: number; run: RunFn; program: Prog }) {
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
        <Row label="Joins epoch">
          {epoch} (fixing {fmt.timeLabel(epoch * DAY + offset)})
        </Row>
        <Row label="Cancel">In full, any time before the fixing</Row>
        {tab === "sub" ? (
          <Row label="Fill">Shares by the cbBTC (or USDC) actually added, possibly in parts</Row>
        ) : (
          <Row label="Payout">Claim once your slice is sold; {cfg.withdrawalFeeBps / 100}% withdrawal fee</Row>
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
}

/** What a ticket is waiting for (display only; the program computes the real amounts) */
function viewTicket(t: Ticket, fund: Fund, records: EpochRecord[], now: number): TicketView {
  const epoch = t.epoch.toNumber();
  const sub = big(t.subscriptionUsdc);
  const red = big(t.redemptionShares);
  const detail: string[] = [];
  if (sub > 0n) detail.push(`Subscribed ${fmt.usdc(sub)} USDC`);
  if (red > 0n) detail.push(`Redeeming ${fmt.shares(red)} shares`);
  const offset = fund.config.settleOffsetSecs.toNumber();
  if (epoch > fund.state.lastFixDay.toNumber()) {
    const deadline = cancelDeadline(epoch, now, offset);
    const open = now < deadline;
    return {
      epoch,
      status: open ? `Queued, fixing in ${fmt.countdown(deadline - now)}` : "Waiting for the fixing",
      tone: "blue",
      canCancel: open,
      canCancelPending: false,
      canClaim: false,
      detail,
    };
  }
  const record = recordFor(records, epoch);
  if (!record) return { epoch, status: "Fixing record not found", tone: "gray", canCancel: false, canCancelPending: false, canClaim: false, detail };
  let claimable = false;
  let pending = false;
  if (sub > 0n) {
    const basis = big(record.subscriptionUsdc) - big(record.cancelledSubscriptionUsdc);
    const minted = big(record.mintedShares) - big(record.cancelledShares);
    const owed = basis > 0n ? (minted * sub) / basis - big(t.sharesClaimed) : 0n;
    if (record.subscriptionFilled) claimable = true;
    else {
      pending = true;
      if (owed > 0n) claimable = true;
    }
    detail.push(record.subscriptionFilled ? "Subscription fully filled" : `About ${fmt.shares(owed)} shares filled so far`);
  }
  if (red > 0n) {
    if (record.sliceSoldOut) {
      claimable = true;
      const gross = big(record.redemptionShares) > 0n ? (big(record.payoutUsdc) * red) / big(record.redemptionShares) : 0n;
      detail.push(`About ${fmt.usdc(gross)} USDC before the withdrawal fee`);
    } else detail.push("Redemption slice still selling");
  }
  return {
    epoch,
    record,
    status: claimable ? "Ready to claim" : pending ? "Filling" : "Selling",
    tone: claimable ? "green" : "amber",
    canCancel: false,
    canCancelPending: pending,
    canClaim: claimable,
    detail,
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
  const price = basis ? sharePrice(big(s.usdc), big(s.btc), big(s.shareSupply), basis.btcUsd) : null;
  const value = price !== null ? (Number(snap.shareBalance) / 1e6) * price : null;
  return (
    <section className="card">
      <h2>Your position</h2>
      <div className="stats two">
        <Stat label="Shares" value={fmt.shares(snap.shareBalance)} />
        <Stat label="USDC balance" value={fmt.usdc(snap.usdcBalance)} />
      </div>
      {value !== null && snap.shareBalance > 0n && (
        <p className="muted small">
          About ${value.toLocaleString("en-US", { maximumFractionDigits: 2 })} at ${price!.toFixed(4)} per share
          (before the 1% withdrawal fee).
        </p>
      )}
      <TransferShares balance={snap.shareBalance} run={run} program={program} />
      {snap.tickets.length === 0 ? (
        <p className="muted small">No tickets.</p>
      ) : (
        <ul className="tickets">
          {snap.tickets.map((t) => {
            const v = viewTicket(t, snap.fund, snap.records, now);
            return (
              <li key={t.address.toBase58()} className="ticket">
                <div className="ticket-top">
                  <span className="ticket-epoch">Epoch {v.epoch} · {fmt.dayLabel(v.epoch)}</span>
                  <span className={`pill pill-${v.tone}`}>{v.status}</span>
                </div>
                <div className="muted small">{v.detail.join(" · ")}</div>
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
      <h3>Get test USDC</h3>
      <p className="muted small">Each address can claim 10,000 test USDC once. If the wallet has too little SOL for fees, it also gets 0.02 devnet SOL.</p>
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

function Guide({ snap }: { snap: Snapshot }) {
  const offset = snap.fund.config.settleOffsetSecs.toNumber();
  const at = new Date(offset * 1000).toISOString().slice(11, 16);
  return (
    <section className="card subtle">
      <h3>How to test the full flow</h3>
      <ol className="steps">
        <li>Connect a wallet (Phantom on devnet) and claim 10,000 test USDC.</li>
        <li>Subscribe. The request joins the epoch of the next fixing (daily at {at} UTC) and can be cancelled until then.</li>
        <li>A few minutes after {at} UTC, the daily settlement fixes the epoch and fills it at the real Pyth price.</li>
        <li>Claim your shares under "Your position". Shares are ordinary tokens you can send to another wallet.</li>
        <li>Redeem shares. After the next fixing sells your slice, claim the USDC (1% withdrawal fee).</li>
      </ol>
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

function Stat({ label, value }: { label: string; value: React.ReactNode }) {
  return (
    <div className="stat">
      <div className="stat-label">{label}</div>
      <div className="stat-value">{value}</div>
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
