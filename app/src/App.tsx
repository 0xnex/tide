import { useCallback, useEffect, useMemo, useState } from "react";
import { useAnchorWallet, useConnection } from "@solana/wallet-adapter-react";
import { WalletMultiButton } from "@solana/wallet-adapter-react-ui";
import { BN } from "@coral-xyz/anchor";
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
  subscribe,
  type EpochRecord,
  type Fund,
  type Snapshot,
  type Ticket,
  DAY,
} from "./chain";
import * as fmt from "./format";

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

  /** 发一笔交易，显示进度和结果，然后刷新 */
  const run = useCallback(
    async (label: string, send: () => Promise<string>) => {
      setToast({ kind: "busy", text: `${label}：等待钱包签名和链上确认…` });
      try {
        const sig = await send();
        setToast({ kind: "ok", text: `${label}成功`, sig });
      } catch (e) {
        setToast({ kind: "error", text: `${label}失败：${describeError(e)}` });
      }
      await refresh();
    },
    [refresh],
  );

  return (
    <div className="page">
      <Header />
      <main className="container">
        {loadError && !snap && <div className="notice error">读不到基金：{loadError}</div>}
        {!snap && !loadError && <div className="notice">正在读取链上数据…</div>}
        {snap && (
          <>
            <VaultHero snap={snap} />
            <div className="grid">
              <div className="col-main">
                <Overview snap={snap} now={now} />
                <Strategy snap={snap} now={now} />
                <Records records={snap.records} />
              </div>
              <div className="col-side">
                <ActionPanel snap={snap} now={now} run={run} program={program} />
                <Position snap={snap} now={now} run={run} program={program} />
                <Faucet />
              </div>
            </div>
          </>
        )}
        {toast && <ToastView toast={toast} onClose={() => setToast(null)} />}
      </main>
      <footer className="footer container">
        <span>devnet 原型：价格来自 Pyth，兑换是模拟交易所。不是投资建议，未经审计，可能有漏洞。</span>
        <a href={fmt.explorerAddress(PROGRAM_ID.toBase58())} target="_blank" rel="noreferrer">
          程序 {fmt.short(PROGRAM_ID.toBase58())}
        </a>
      </footer>
    </div>
  );
}

function Header() {
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
          <span className="nav-item active">基金</span>
        </nav>
        <div className="header-right">
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
          <h1>Tide BTC 趋势基金</h1>
          <p className="muted">价格高于 {cfg.window} 日均线时持有 cbBTC，低于时持有 USDC。每天定价一次，按实际成交发份额。</p>
        </div>
      </div>
      <div className="tags">
        <span className="tag">信号 {feed}（Pyth）</span>
        <span className="tag">{cfg.window} 日均线</span>
        <span className="tag">取款费 {cfg.withdrawalFeeBps / 100}%</span>
        <a className="tag link" href={fmt.explorerAddress(FUND.toBase58())} target="_blank" rel="noreferrer">
          基金 {fmt.short(FUND.toBase58())} ↗
        </a>
      </div>
    </section>
  );
}

function Overview({ snap, now }: { snap: Snapshot; now: number }) {
  const s = snap.fund.state;
  const last = snap.records[0];
  // 只用来显示：按最近一次定价价格折算成美元（基金本身从不按美元估值）
  const indicative = last ? Number(big(s.usdc)) / 1e6 + (Number(big(s.btc)) / 1e8) * (Number(big(last.signalPrice)) / 1e6) : null;
  const perShare = indicative !== null && !s.shareSupply.isZero() ? indicative / (Number(big(s.shareSupply)) / 1e6) : null;
  void now;
  return (
    <section className="card">
      <div className="stats">
        <Stat label="持有 cbBTC" value={fmt.btc(s.btc)} />
        <Stat label="持有 USDC" value={fmt.usdc(s.usdc)} />
        <Stat label="份额总数" value={fmt.shares(s.shareSupply)} />
        <Stat
          label="当前目标"
          value={<span className={`pill ${s.targetBtc ? "pill-blue" : "pill-gray"}`}>{s.targetBtc ? "cbBTC" : "USDC"}</span>}
        />
      </div>
      {indicative !== null && (
        <p className="muted small">
          按最近定价价格折算约 ${indicative.toLocaleString("en-US", { maximumFractionDigits: 2 })}
          {perShare !== null && `，每份约 $${perShare.toFixed(4)}`}（仅供参考，基金不按美元估值）
        </p>
      )}
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
      <h2>策略</h2>
      <div className="rows">
        <Row label="最近定价">{last ? `${fmt.dayLabel(last.epoch.toNumber())}（第 ${last.epoch.toNumber()} 期）` : "还没有定价"}</Row>
        <Row label="信号价格 / 均线">
          {last ? (
            <span>
              {fmt.price(last.signalPrice)} / {ma > 0 ? fmt.price(last.movingAverage) : "价格不足"}
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
        <Row label="下一个定价时刻">
          {fmt.timeLabel(nextFix)}
          <span className="muted">（{fmt.countdown(nextFix - now)}后）</span>
        </Row>
        <Row label="申购">
          {s.subscriptionsPaused ? <span className="pill pill-amber">已暂停</span> : <span className="pill pill-green">开放</span>}
        </Row>
        <Row label="申购额度">
          <span>
            {fmt.usdc(outstanding)} / {fmt.usdc(cap)} USDC
          </span>
        </Row>
      </div>
      <div className="bar">
        <div className="bar-fill" style={{ width: `${Math.min(capPct, 100)}%` }} />
      </div>
      <div className="mini-stats">
        <span>未卖完的赎回资产 {s.sliceCount} 期</span>
        <span>待成交申购 {s.pendingCount} 期</span>
        <span>欠管理人取款费 {fmt.usdc(s.feesOwed)} USDC</span>
      </div>
    </section>
  );
}

function Records({ records }: { records: EpochRecord[] }) {
  return (
    <section className="card">
      <h2>每次定价</h2>
      {records.length === 0 ? (
        <p className="muted">还没有定价记录。</p>
      ) : (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th>日期</th>
                <th className="num">信号</th>
                <th className="num">均线</th>
                <th>目标</th>
                <th className="num">申购 USDC</th>
                <th className="num">赎回份额</th>
                <th>状态</th>
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
                      <span className="pill pill-green">完成</span>
                    ) : (
                      <span className="pill pill-amber">成交中</span>
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
  const problem = !wallet
    ? "请先连接钱包"
    : amount === null || amount === 0n
      ? "输入数量"
      : amount > balance
        ? "余额不足"
        : tab === "sub" && amount < min
          ? `最低申购 ${fmt.usdc(min)} USDC`
          : tab === "sub" && snap.fund.state.subscriptionsPaused
            ? "申购已暂停"
            : null;

  const submit = () => {
    if (!wallet || amount === null) return;
    const label = tab === "sub" ? `申购 ${fmt.usdc(amount)} USDC` : `赎回 ${fmt.shares(amount)} 份`;
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
          申购
        </button>
        <button className={tab === "red" ? "tab active" : "tab"} onClick={() => setTab("red")}>
          赎回
        </button>
      </div>
      <div className="input-box">
        <div className="input-top">
          <span className="muted small">{tab === "sub" ? "付出" : "赎回份额"}</span>
          <span className="muted small">
            余额 {tab === "sub" ? `${fmt.usdc(balance)} USDC` : `${fmt.shares(balance)} 份`}
            {balance > 0n && (
              <button className="link-btn" onClick={() => setText((Number(balance) / 1e6).toString())}>
                最大
              </button>
            )}
          </span>
        </div>
        <div className="input-row">
          <input inputMode="decimal" placeholder="0.00" value={text} onChange={(e) => setText(e.target.value)} />
          <span className="unit">{tab === "sub" ? "USDC" : "份"}</span>
        </div>
      </div>
      <div className="rows compact">
        <Row label="加入的期">
          第 {epoch} 期（{fmt.timeLabel(epoch * DAY + offset)} 定价）
        </Row>
        <Row label="撤销">定价时刻之前可以全额撤销</Row>
        {tab === "sub" ? (
          <Row label="成交">按实际买到的 cbBTC（或 USDC）发份额，可能分几次</Row>
        ) : (
          <Row label="到账">赎回资产卖完后领取，扣 {cfg.withdrawalFeeBps / 100}% 取款费</Row>
        )}
      </div>
      <button className="btn primary" disabled={problem !== null} onClick={submit}>
        {problem ?? (tab === "sub" ? "申购" : "赎回")}
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

/** 一张排队记录现在的状态（只用来显示；实际数额由程序算） */
function viewTicket(t: Ticket, fund: Fund, records: EpochRecord[], now: number): TicketView {
  const epoch = t.epoch.toNumber();
  const sub = big(t.subscriptionUsdc);
  const red = big(t.redemptionShares);
  const detail: string[] = [];
  if (sub > 0n) detail.push(`申购 ${fmt.usdc(sub)} USDC`);
  if (red > 0n) detail.push(`赎回 ${fmt.shares(red)} 份`);
  const offset = fund.config.settleOffsetSecs.toNumber();
  if (epoch > fund.state.lastFixDay.toNumber()) {
    const deadline = cancelDeadline(epoch, now, offset);
    const open = now < deadline;
    return {
      epoch,
      status: open ? `排队中，${fmt.countdown(deadline - now)}后定价` : "等待定价",
      tone: "blue",
      canCancel: open,
      canCancelPending: false,
      canClaim: false,
      detail,
    };
  }
  const record = recordFor(records, epoch);
  if (!record) return { epoch, status: "找不到定价记录", tone: "gray", canCancel: false, canCancelPending: false, canClaim: false, detail };
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
    detail.push(record.subscriptionFilled ? "申购已全部成交" : `已成交约 ${fmt.shares(owed)} 份可领`);
  }
  if (red > 0n) {
    if (record.sliceSoldOut) {
      claimable = true;
      const gross = big(record.redemptionShares) > 0n ? (big(record.payoutUsdc) * red) / big(record.redemptionShares) : 0n;
      detail.push(`赎回约 ${fmt.usdc(gross)} USDC（扣取款费前）`);
    } else detail.push("赎回资产卖出中");
  }
  return {
    epoch,
    record,
    status: claimable ? "可以领取" : pending ? "成交中" : "卖出中",
    tone: claimable ? "green" : "amber",
    canCancel: false,
    canCancelPending: pending,
    canClaim: claimable,
    detail,
  };
}

function Position({ snap, now, run, program }: { snap: Snapshot; now: number; run: RunFn; program: Prog }) {
  const wallet = useAnchorWallet();
  if (!wallet) {
    return (
      <section className="card">
        <h2>我的仓位</h2>
        <p className="muted">连接钱包后显示你的份额和排队记录。</p>
      </section>
    );
  }
  const user = wallet.publicKey;
  return (
    <section className="card">
      <h2>我的仓位</h2>
      <div className="stats two">
        <Stat label="持有份额" value={fmt.shares(snap.shareBalance)} />
        <Stat label="USDC 余额" value={fmt.usdc(snap.usdcBalance)} />
      </div>
      {snap.tickets.length === 0 ? (
        <p className="muted small">没有排队记录。</p>
      ) : (
        <ul className="tickets">
          {snap.tickets.map((t) => {
            const v = viewTicket(t, snap.fund, snap.records, now);
            return (
              <li key={t.address.toBase58()} className="ticket">
                <div className="ticket-top">
                  <span className="ticket-epoch">第 {v.epoch} 期 · {fmt.dayLabel(v.epoch)}</span>
                  <span className={`pill pill-${v.tone}`}>{v.status}</span>
                </div>
                <div className="muted small">{v.detail.join("；")}</div>
                <div className="ticket-actions">
                  {v.canClaim && v.record && (
                    <button className="btn small primary" onClick={() => run("领取", () => claim(program, snap.fund, user, t, v.record!))}>
                      领取
                    </button>
                  )}
                  {v.canCancel && (
                    <button className="btn small" onClick={() => run("撤销", () => cancel(program, snap.fund, user, t))}>
                      撤销
                    </button>
                  )}
                  {v.canCancelPending && v.record && (
                    <button
                      className="btn small"
                      onClick={() => run("撤销待成交申购", () => cancelPending(program, snap.fund, user, t, v.record!))}
                    >
                      撤销未成交部分
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

function Faucet() {
  const wallet = useAnchorWallet();
  const [copied, setCopied] = useState(false);
  const cmd = `yarn ts-node scripts/faucet.ts ${wallet?.publicKey.toBase58() ?? "<你的地址>"}`;
  return (
    <section className="card subtle">
      <h3>需要测试 USDC？</h3>
      <p className="muted small">模拟 USDC 由管理员钱包铸造。在仓库里用管理员钱包运行：</p>
      <code className="code">{cmd}</code>
      <button
        className="link-btn"
        onClick={() => {
          navigator.clipboard.writeText(cmd);
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        }}
      >
        {copied ? "已复制" : "复制命令"}
      </button>
    </section>
  );
}

function ToastView({ toast, onClose }: { toast: Toast; onClose: () => void }) {
  return (
    <div className={`toast ${toast.kind}`}>
      <span>{toast.text}</span>
      {toast.sig && (
        <a href={fmt.explorer(toast.sig)} target="_blank" rel="noreferrer">
          查看交易 ↗
        </a>
      )}
      {toast.kind !== "busy" && (
        <button className="link-btn" onClick={onClose}>
          关闭
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
