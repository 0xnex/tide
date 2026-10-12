// 每日结算脚本（devnet 原型用）
//
// 价格来自 Pyth：每一步之前从 Hermes 取签名的价格更新，提交给 Pyth 接收程序，
// 把得到的价格更新账户传给程序，用完关闭、退回租金。用哪些价格源看 config.signal_feed：
// btc 只用 BTC/USD；cbBtc 用 cbBTC/USD，BTC/USD 作脱锚检查的参照。
//
// 做这几件事：
//   0. 昨天没有定价时，用昨天定价时刻的 Pyth 历史价格 backfill_price 回填均线。
//   1. 用今天定价时刻（UTC 0 点 + settle_offset_secs）的 Pyth 历史价格调用 fix。
//      定价价格必须在定价时刻后 60 秒内发布，Hermes 给的是那一刻之后的第一个价格，所以几点运行都可以。
//   2. 每隔最小交易间隔用最新的 Pyth 价格调用一次 trade，直到程序回答没有可交易的：
//      NothingToTrade（都做完了）、DepegBlocksBuy（脱锚，等下一次）或 FixPending（定价没成功，
//      未卖完的赎回资产已经卖完，基金调仓等下一次定价）。
//      成交低于 min_out 的那一笔会回滚，等下一个间隔重试。
//   3. 有欠管理人的取款费时调用 collect_fees，付到收费地址。
//
// 用法（每天 UTC 01:00 之后用 cron 运行一次）：
//   ANCHOR_PROVIDER_URL=https://api.devnet.solana.com \
//   ANCHOR_WALLET=~/.config/solana/id.json \
//   yarn ts-node scripts/crank.ts
// Hermes 要 API key（从 Pyth Terminal 申请）：PYTH_API_KEY=<key>，按 Bearer token 发送。
// HERMES_URL 可以换成别的 Hermes 服务地址。
//
// 注意：脚本以清算地址（config.keeper）签名定价和交易；定价时刻后 keeper_window_secs 内只有它能驱动结算。
// 兑换仍然走模拟交易所（按 Pyth 价格成交），主网要换成 Jupiter。
import * as anchor from "@coral-xyz/anchor";
import { BN, Program } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";
import { getAssociatedTokenAddressSync } from "@solana/spl-token";
import { PythSolanaReceiver } from "@pythnetwork/pyth-solana-receiver";
import { Tide } from "../target/types/tide";

const IDL = require("../target/idl/tide.json");

// 停止交易的回答（都做完了 / 脱锚 / 今天的定价没成功，基金调仓等下一次定价）；
// 其余的可重试错误等下一个间隔再试
const DONE = ["NothingToTrade", "DepegBlocksBuy", "FixPending"];
const RETRY = ["TradeTooSoon", "SlippageExceeded", "StalePrice", "PriceTooUncertain"];
// 定价步骤可以跳过、直接去交易的回答
const FIX_SKIPPABLE = ["AlreadySettled", "TooManySlices", "PriceNotAtFixTime", "TooEarly", "PriceTooUncertain"];
// 不用回填或者回填不了的回答
const BACKFILL_SKIPPABLE = ["PriceAlreadyRecorded", "PriceNotAtFixTime", "BackfillOutOfRange", "PriceTooUncertain"];
const DAY = 86_400;
const PRIORITY_FEE = { computeUnitPriceMicroLamports: 50_000 };
const HERMES = process.env.HERMES_URL ?? "https://hermes.pyth.network";

/** Hermes 要 API key：放在 Authorization 头里（不写进代码和仓库，从环境变量读） */
function hermesHeaders(): Record<string, string> {
  const key = process.env.PYTH_API_KEY;
  if (!key) throw new Error("缺少 PYTH_API_KEY：Hermes 要 API key，从 Pyth Terminal 申请");
  return { Authorization: `Bearer ${key}` };
}

/** Hermes 签名的价格更新（base64）：publishTime 为 null 时取最新，否则取那一刻之后的第一个 */
async function hermesUpdates(feeds: string[], publishTime: number | null): Promise<string[]> {
  const path = publishTime === null ? "latest" : String(publishTime);
  const query = feeds.map((id) => `ids[]=${id}`).join("&");
  const r = await fetch(`${HERMES}/v2/updates/price/${path}?${query}&encoding=base64`, {
    headers: hermesHeaders(),
  });
  if (!r.ok) throw new Error(`Hermes 返回 HTTP ${r.status}：${await r.text()}`);
  return ((await r.json()) as any).binary.data;
}

/** 程序返回的错误名（没有就是 undefined） */
function errorName(e: any): string | undefined {
  return e?.error?.errorCode?.code;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = new Program<Tide>(IDL, provider);
  const admin = provider.wallet.publicKey;
  const pda = (seeds: (Buffer | Uint8Array)[]) => PublicKey.findProgramAddressSync(seeds, program.programId)[0];

  const fund = pda([Buffer.from("fund"), admin.toBuffer()]);
  const market = pda([Buffer.from("market"), admin.toBuffer()]);
  const shareMint = pda([Buffer.from("share_mint"), fund.toBuffer()]);
  const fundPda = (name: string) => pda([Buffer.from(name), fund.toBuffer()]);
  const epochPda = (epoch: BN | number) =>
    pda([Buffer.from("epoch"), fund.toBuffer(), new BN(epoch).toArrayLike(Buffer, "le", 8)]);

  const f = await program.account.fund.fetch(fund);
  if (!f.config.keeper.equals(admin)) {
    throw new Error(`钱包 ${admin.toBase58()} 不是清算地址 ${f.config.keeper.toBase58()}，清算窗口内会被拒绝`);
  }

  // Pyth：signal_feed 为 btc 时只要 BTC/USD；为 cbBtc 时要 cbBTC/USD 和作参照的 BTC/USD
  const receiver = new PythSolanaReceiver({ connection: provider.connection, wallet: provider.wallet as any });
  const btcFeed = Buffer.from(f.config.btcFeedId).toString("hex");
  const cbbtcFeed = Buffer.from(f.config.cbbtcFeedId).toString("hex");
  const useCbbtc = "cbBtc" in f.config.signalFeed;
  const feeds = useCbbtc ? [cbbtcFeed, btcFeed] : [btcFeed];

  /**
   * 取 Hermes 签名的价格更新（publishTime 为 null 时取最新，否则取那一刻之后的第一个），
   * 提交给 Pyth 接收程序，把价格更新账户交给 run；不管 run 成功与否，最后都关闭这些账户、退回租金。
   */
  async function withPyth<T>(
    publishTime: number | null,
    run: (accounts: { btcPriceUpdate: PublicKey; cbbtcPriceUpdate: PublicKey | null }) => Promise<T>,
  ): Promise<T> {
    const { postInstructions, priceFeedIdToPriceUpdateAccount: accounts, closeInstructions } =
      await receiver.buildPostPriceUpdateInstructions(await hermesUpdates(feeds, publishTime));
    await receiver.provider.sendAll(await receiver.batchIntoVersionedTransactions(postInstructions, PRIORITY_FEE));
    try {
      return await run({
        btcPriceUpdate: accounts["0x" + btcFeed],
        cbbtcPriceUpdate: useCbbtc ? accounts["0x" + cbbtcFeed] : null,
      });
    } finally {
      await receiver.provider.sendAll(await receiver.batchIntoVersionedTransactions(closeInstructions, PRIORITY_FEE));
    }
  }

  /** 执行一步，返回程序的错误名；skippable 里的错误只记一笔，其他错误抛出 */
  async function attempt(label: string, skippable: string[], send: () => Promise<string>): Promise<string | null> {
    try {
      console.log(`${label}：${await send()}`);
      return null;
    } catch (e) {
      const name = errorName(e);
      if (!name || !skippable.includes(name)) throw e;
      console.log(`${label}没有完成（${name}）`);
      return name;
    }
  }

  // 链上时钟：最近的 finalized 区块时间，取不到时用本机时钟
  async function chainNow(): Promise<number> {
    try {
      const t = await provider.connection.getBlockTime(await provider.connection.getSlot("finalized"));
      if (t) return t;
    } catch (e) {
      console.log(`读取链上时间失败，改用本机时钟：${e}`);
    }
    return Math.floor(Date.now() / 1000);
  }
  const offset = f.config.settleOffsetSecs.toNumber();
  const today = Math.floor((await chainNow()) / DAY);

  // 0. 昨天没有价格时，用昨天定价时刻的历史价格回填
  const yesterday = today - 1;
  const slot = ((yesterday % 64) + 64) % 64;
  if (f.state.priceDays[slot].toNumber() !== yesterday || f.state.prices[slot].isZero()) {
    await withPyth(yesterday * DAY + offset, (prices) =>
      attempt(`回填昨天（${yesterday}）的价格`, BACKFILL_SKIPPABLE, () =>
        program.methods.backfillPrice(new BN(yesterday)).accountsPartial({ caller: admin, fund, ...prices }).rpc(),
      ),
    );
  }

  // 1. 定价：用今天定价时刻的历史价格
  if ((await chainNow()) < today * DAY + offset) {
    console.log("还没到今天的定价时刻，只做交易");
  } else {
    await withPyth(today * DAY + offset, (prices) =>
      attempt("定价", FIX_SKIPPABLE, () =>
        program.methods
          .fix(new BN(today))
          .accountsPartial({
            cranker: admin,
            fund,
            epochRecord: epochPda(today),
            ...prices,
            shareMint,
            usdcVault: fundPda("usdc_vault"),
            subscriptionEscrow: fundPda("subscription_escrow"),
            shareEscrow: fundPda("share_escrow"),
            payoutVault: fundPda("payout_vault"),
          })
          .rpc(),
      ),
    );
  }

  // 2. 交易：每次用最新价格
  const intervalMs = f.config.minTradeIntervalSecs.toNumber() * 1000;
  for (;;) {
    const s = (await program.account.fund.fetch(fund)).state;
    // 最早一期未卖完的赎回资产的记录；没有时用最早一期待成交申购的记录
    const recordEpoch = s.sliceCount > 0 ? s.slices[0].epoch : s.pendingCount > 0 ? s.pending[0].epoch : null;
    const why = await withPyth(null, (prices) =>
      attempt("交易", [...DONE, ...RETRY], () =>
        program.methods
          .trade()
          .accountsPartial({
            cranker: admin,
            fund,
            ...prices,
            market,
            marketUsdc: pda([Buffer.from("market_usdc"), market.toBuffer()]),
            marketBtc: pda([Buffer.from("market_btc"), market.toBuffer()]),
            usdcVault: fundPda("usdc_vault"),
            btcVault: fundPda("btc_vault"),
            payoutVault: fundPda("payout_vault"),
            subscriptionEscrow: fundPda("subscription_escrow"),
            shareEscrow: fundPda("share_escrow"),
            shareMint,
            epochRecord: recordEpoch === null ? null : epochPda(recordEpoch),
          })
          .rpc(),
      ),
    );
    if (why && DONE.includes(why)) break;
    await sleep(intervalMs);
  }

  // 3. 结清欠管理人的取款费（收费地址的 USDC 账户不存在时由清算地址创建）
  const owed = (await program.account.fund.fetch(fund)).state.feesOwed;
  if (owed.gtn(0)) {
    const sig = await program.methods
      .collectFees()
      .accountsPartial({
        caller: admin,
        fund,
        usdcMint: f.usdcMint,
        feeAddress: f.config.feeAddress,
        feeUsdc: getAssociatedTokenAddressSync(f.usdcMint, f.config.feeAddress, true),
        payoutVault: fundPda("payout_vault"),
      })
      .rpc();
    console.log(`结清取款费 ${owed.toNumber() / 1e6} USDC：${sig}`);
  }

  const after = await program.account.fund.fetch(fund);
  console.log(
    `目标持仓：${after.state.targetBtc ? "cbBTC" : "USDC"}；未卖完的赎回资产 ${after.state.sliceCount} 期；` +
      `下一次定价处理的申购 ${after.state.queuedSubscriptionUsdc.toNumber() / 1e6} USDC`,
  );
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
