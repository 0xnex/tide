// 链上地址、读数据和发交易。PDA 种子与程序一致（见 AGENTS.md 的 PDA 表）。
import { AnchorProvider, BN, Program } from "@coral-xyz/anchor";
import type { AnchorWallet } from "@solana/wallet-adapter-react";
import { Connection, PublicKey, Transaction } from "@solana/web3.js";
import {
  createAssociatedTokenAccountIdempotentInstruction,
  createTransferCheckedInstruction,
  getAssociatedTokenAddressSync,
} from "@solana/spl-token";
import idl from "./idl/tide.json";
import type { Tide } from "./idl/tide";
import { ERROR_MESSAGES } from "./errors";

export const RPC_URL: string = import.meta.env.VITE_RPC_URL ?? "https://api.devnet.solana.com";
export const FUND_ADMIN = new PublicKey(
  import.meta.env.VITE_FUND_ADMIN ?? "DoFt9r97s51QkpohoCvsYRfcDMkuSof2WtrFqtrhjDKW",
);
export const PROGRAM_ID = new PublicKey(idl.address);
export const DAY = 86_400;

const pda = (seeds: (Buffer | Uint8Array)[]) => PublicKey.findProgramAddressSync(seeds, PROGRAM_ID)[0];
const le = (n: number) => new BN(n).toArrayLike(Buffer, "le", 8);
export const FUND = pda([Buffer.from("fund"), FUND_ADMIN.toBuffer()]);
export const SHARE_MINT = pda([Buffer.from("share_mint"), FUND.toBuffer()]);
export const fundVault = (name: string) => pda([Buffer.from(name), FUND.toBuffer()]);
export const ticketPda = (user: PublicKey, epoch: number) =>
  pda([Buffer.from("ticket"), FUND.toBuffer(), user.toBuffer(), le(epoch)]);
export const epochPda = (epoch: number) => pda([Buffer.from("epoch"), FUND.toBuffer(), le(epoch)]);

/** 没连钱包时只读用的 provider */
const readOnlyWallet = {
  publicKey: PublicKey.default,
  signTransaction: async () => {
    throw new Error("Connect a wallet first");
  },
  signAllTransactions: async () => {
    throw new Error("Connect a wallet first");
  },
} as unknown as AnchorWallet;

export function makeProgram(connection: Connection, wallet?: AnchorWallet): Program<Tide> {
  const provider = new AnchorProvider(connection, wallet ?? readOnlyWallet, { commitment: "confirmed" });
  return new Program<Tide>(idl as Tide, provider);
}

/** 这一刻提交的请求属于哪一期：定价时刻之前是当天，之后是第二天（与 fund-logic 的 request_epoch 一致） */
export function requestEpoch(nowSec: number, offset: number): number {
  const day = Math.floor(nowSec / DAY);
  return nowSec < day * DAY + offset ? day : day + 1;
}

/** 第 epoch 期最晚什么时刻之前可以撤销（与 fund-logic 的 cancel_deadline 一致） */
export function cancelDeadline(epoch: number, nowSec: number, offset: number): number {
  return Math.max(epoch, Math.floor(nowSec / DAY)) * DAY + offset;
}

export type Fund = Awaited<ReturnType<Program<Tide>["account"]["fund"]["fetch"]>>;
export type EpochRecord = Awaited<ReturnType<Program<Tide>["account"]["epochRecord"]["fetch"]>>;
export type Ticket = Awaited<ReturnType<Program<Tide>["account"]["ticket"]["fetch"]>> & { address: PublicKey };
export type MockPool = Awaited<ReturnType<Program<Tide>["account"]["mockPool"]["fetch"]>>;

export interface Snapshot {
  fund: Fund;
  records: EpochRecord[]; // 新的在前
  tickets: Ticket[]; // 当前钱包的排队记录
  usdcBalance: bigint;
  shareBalance: bigint;
  solBalance: number | null; // lamports，用来付交易费；没连钱包时为 null
  lentUsdc: bigint; // 借出的 USDC 现在值多少（ADR 0008），只用来显示
  lendingApyBps: number; // 借贷池的年化
}

/** 借贷池比率的单位（fund_logic::RATE_ONE） */
const RATE_ONE = 1_000_000_000_000n;
const YEAR = 31_536_000n;

/** 借贷凭证按借贷池此刻的比率值多少 USDC：和程序一样单利计息到 nowSec、向下取整 */
function lentValue(receipts: BN, pool: MockPool | null, nowSec: number): bigint {
  if (!pool) return 0n;
  const rate = BigInt(pool.rate.toString());
  const dt = BigInt(Math.max(0, nowSec - pool.lastUpdate.toNumber()));
  const accrued = rate + (rate * BigInt(pool.apyBps) * dt) / (10_000n * YEAR);
  return (BigInt(receipts.toString()) * accrued) / RATE_ONE;
}

/** 基金的 USDC：主账户里的加上借出的 */
export const fundUsdc = (snap: Snapshot): bigint => BigInt(snap.fund.state.usdc.toString()) + snap.lentUsdc;

async function tokenBalance(connection: Connection, addr: PublicKey): Promise<bigint> {
  const info = await connection.getTokenAccountBalance(addr).catch(() => null);
  return info ? BigInt(info.value.amount) : 0n;
}

/** 读基金、所有定价记录，以及钱包的排队记录和余额 */
export async function loadSnapshot(program: Program<Tide>, user: PublicKey | null): Promise<Snapshot> {
  const connection = program.provider.connection;
  const fund = await program.account.fund.fetch(FUND);
  // 按当前账户大小过滤：定价记录加字段之前建的旧记录读不出来，跳过
  const records = (
    await program.account.epochRecord.all([
      { dataSize: program.account.epochRecord.size },
      { memcmp: { offset: 8, bytes: FUND.toBase58() } },
    ])
  )
    .map((r) => r.account)
    .sort((a, b) => b.epoch.toNumber() - a.epoch.toNumber());
  const pool = await program.account.mockPool.fetch(fund.pool).catch(() => null);
  const lending = {
    lentUsdc: lentValue(fund.state.receipts, pool, Math.floor(Date.now() / 1000)),
    lendingApyBps: pool?.apyBps ?? 0,
  };
  if (!user) return { fund, records, tickets: [], usdcBalance: 0n, shareBalance: 0n, solBalance: null, ...lending };
  const tickets = (
    await program.account.ticket.all([
      { memcmp: { offset: 8, bytes: FUND.toBase58() } },
      { memcmp: { offset: 40, bytes: user.toBase58() } },
    ])
  )
    .map((t) => ({ ...t.account, address: t.publicKey }))
    .sort((a, b) => b.epoch.toNumber() - a.epoch.toNumber());
  const [usdcBalance, shareBalance, solBalance] = await Promise.all([
    tokenBalance(connection, getAssociatedTokenAddressSync(fund.usdcMint, user, true)),
    tokenBalance(connection, getAssociatedTokenAddressSync(SHARE_MINT, user, true)),
    connection.getBalance(user).catch(() => null),
  ]);
  return { fund, records, tickets, usdcBalance, shareBalance, solBalance, ...lending };
}

/** 关闭这张排队记录那一期的定价记录：first_epoch ≤ 期号 ≤ epoch */
export function recordFor(records: EpochRecord[], epoch: number): EpochRecord | undefined {
  return records.find((r) => r.firstEpoch.toNumber() <= epoch && epoch <= r.epoch.toNumber());
}

// ---------------------------------------------------------------------------
// 交易
// ---------------------------------------------------------------------------

export async function subscribe(program: Program<Tide>, fund: Fund, user: PublicKey, amount: bigint, epoch: number) {
  return program.methods
    .requestSubscription(new BN(amount.toString()), new BN(epoch))
    .accountsPartial({
      user,
      fund: FUND,
      ticket: ticketPda(user, epoch),
      userUsdc: getAssociatedTokenAddressSync(fund.usdcMint, user, true),
      subscriptionEscrow: fundVault("subscription_escrow"),
    })
    .rpc();
}

export async function redeem(program: Program<Tide>, user: PublicKey, shares: bigint, epoch: number) {
  return program.methods
    .requestRedemption(new BN(shares.toString()), new BN(epoch))
    .accountsPartial({
      user,
      fund: FUND,
      ticket: ticketPda(user, epoch),
      userShares: getAssociatedTokenAddressSync(SHARE_MINT, user, true),
      shareEscrow: fundVault("share_escrow"),
    })
    .rpc();
}

const userAccounts = (fund: Fund, user: PublicKey) => ({
  shareMint: SHARE_MINT,
  userShares: getAssociatedTokenAddressSync(SHARE_MINT, user, true),
  userUsdc: getAssociatedTokenAddressSync(fund.usdcMint, user, true),
  subscriptionEscrow: fundVault("subscription_escrow"),
  shareEscrow: fundVault("share_escrow"),
});

export async function cancel(program: Program<Tide>, fund: Fund, user: PublicKey, ticket: Ticket) {
  return program.methods
    .cancel()
    .accountsPartial({ user, fund: FUND, ticket: ticket.address, ...userAccounts(fund, user) })
    .rpc();
}

export async function cancelPending(program: Program<Tide>, fund: Fund, user: PublicKey, ticket: Ticket, record: EpochRecord) {
  return program.methods
    .cancelPending()
    .accountsPartial({
      user,
      fund: FUND,
      ticket: ticket.address,
      epochRecord: epochPda(record.epoch.toNumber()),
      ...userAccounts(fund, user),
    })
    .rpc();
}

export async function claim(program: Program<Tide>, fund: Fund, user: PublicKey, ticket: Ticket, record: EpochRecord) {
  return program.methods
    .claim()
    .accountsPartial({
      user,
      fund: FUND,
      ticket: ticket.address,
      epochRecord: epochPda(record.epoch.toNumber()),
      usdcMint: fund.usdcMint,
      payoutVault: fundVault("payout_vault"),
      ...userAccounts(fund, user),
    })
    .rpc();
}

/** 把份额转给另一个钱包（份额是普通的 SPL 代币；对方没有份额账户时由转出人创建） */
export async function transferShares(program: Program<Tide>, user: PublicKey, to: PublicKey, amount: bigint) {
  const from = getAssociatedTokenAddressSync(SHARE_MINT, user, true);
  const dest = getAssociatedTokenAddressSync(SHARE_MINT, to, true);
  const tx = new Transaction().add(
    createAssociatedTokenAccountIdempotentInstruction(user, dest, to, SHARE_MINT),
    createTransferCheckedInstruction(from, SHARE_MINT, dest, user, amount, 6),
  );
  return program.provider.sendAndConfirm!(tx);
}

/** 网站后端的测试水龙头：每个地址一次 10,000 测试 USDC，SOL 不够时再送一点 */
export async function requestFaucet(user: PublicKey): Promise<{ signature: string; usdc: number; sol: number }> {
  const r = await fetch("/api/faucet", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ address: user.toBase58() }),
  });
  const body = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(body.error ?? `Faucet returned HTTP ${r.status}`);
  return body;
}

/** 程序错误的英文说明（errors.ts）；不是程序错误时返回原始信息 */
export function describeError(e: unknown): string {
  const err = e as any;
  const logs: string = (err?.logs ?? []).join("\n");
  const fromLog = `${err?.message ?? ""}\n${logs}`.match(/Error Code: (\w+)/)?.[1];
  const code: string | undefined = err?.error?.errorCode?.code ?? fromLog;
  const name = code && idl.errors.find((x) => x.name.toLowerCase() === code.toLowerCase())?.name;
  if (name) return ERROR_MESSAGES[name] ?? name;
  return String(err?.message ?? err);
}
