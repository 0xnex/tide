// 每日结算脚本（devnet 原型用）
//
// 做三件事：
//   1. 从 Binance 公开行情读取 BTC 价格，写入模拟预言机的两个价格源（主网版本应改用 Pyth，删除这一步）。
//      Binance 没有 cbBTC 行情，devnet 上把 cbBTC/USD 当作与 BTC/USD 同价；
//      Pyth 上线后分别读取 BTC/USD 和 cbBTC/USD。
//   2. 调用 fix 完成当天的定价步骤
//   3. 每隔最小交易间隔调用一次 trade（每次之前刷新价格），直到程序回答没有可交易的：
//      NothingToTrade（都做完了）、DepegBlocksBuy（脱锚，等下一次）或 FixPending（定价没成功，
//      未卖完的赎回资产已经卖完，基金调仓等下一次定价）。
//      成交低于 min_out 的那一笔会回滚，等下一个间隔重试。
//
// 用法（每天 UTC 01:00 用 cron 运行一次。模拟预言机的价格就是定价价格，
// 必须在 1 点后 60 秒内写入并定价，否则当天的定价会被拒绝，但交易照常进行，
// 当天的申购赎回顺延到第二天定价）：
//   ANCHOR_PROVIDER_URL=https://api.devnet.solana.com \
//   ANCHOR_WALLET=~/.config/solana/id.json \
//   yarn ts-node scripts/crank.ts
//
// 注意：脚本以清算地址（config.keeper）签名定价和交易；01:00–01:30 只有它能驱动结算。
// devnet 上模拟预言机也由管理员钱包签名，所以清算地址要设成管理员钱包，并用这个钱包运行。
import * as anchor from "@coral-xyz/anchor";
import { BN, Program } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";
import { Tide } from "../target/types/tide";

const IDL = require("../target/idl/tide.json");

// 停止交易的回答（都做完了 / 脱锚 / 今天的定价没成功，基金调仓等下一次定价）；
// 其余的可重试错误等下一个间隔再试
const DONE = ["NothingToTrade", "DepegBlocksBuy", "FixPending"];
const RETRY = ["TradeTooSoon", "SlippageExceeded", "StalePrice"];
// 定价步骤可以跳过、直接去交易的回答
const FIX_SKIPPABLE = ["AlreadySettled", "TooManySlices", "PriceNotAtFixTime", "TooEarly"];

async function btcPriceMicroUsd(): Promise<BN> {
  const r = await fetch("https://api.binance.com/api/v3/ticker/price?symbol=BTCUSDT");
  if (!r.ok) throw new Error(`读取价格失败：HTTP ${r.status}`);
  const j: any = await r.json();
  const p = Number(j.price);
  if (!(p > 0)) throw new Error(`价格无效：${j.price}`);
  return new BN(Math.round(p * 1_000_000));
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
  const oracle = pda([Buffer.from("oracle"), admin.toBuffer()]);
  const market = pda([Buffer.from("market"), admin.toBuffer()]);
  const shareMint = pda([Buffer.from("share_mint"), fund.toBuffer()]);
  const fundPda = (name: string) => pda([Buffer.from(name), fund.toBuffer()]);
  const epochPda = (epoch: BN) => pda([Buffer.from("epoch"), fund.toBuffer(), epoch.toArrayLike(Buffer, "le", 8)]);

  // 信号价格（BTC/USD）和估值价格（cbBTC/USD）
  async function postPrice() {
    const price = await btcPriceMicroUsd();
    await program.methods.setMockPrice(price, price).accountsPartial({ authority: admin, oracle }).rpc();
    return price.toNumber() / 1e6;
  }

  const f = await program.account.fund.fetch(fund);
  if (!f.config.keeper.equals(admin)) {
    throw new Error(`钱包 ${admin.toBase58()} 不是清算地址 ${f.config.keeper.toBase58()}，01:00–01:30 会被拒绝`);
  }
  console.log(`已写入价格：${await postPrice()} USD`);
  try {
    const sig = await program.methods
      .fix()
      .accountsPartial({
        cranker: admin,
        fund,
        epochRecord: epochPda(f.state.epoch),
        oracle,
        shareMint,
        usdcVault: fundPda("usdc_vault"),
        subscriptionEscrow: fundPda("subscription_escrow"),
        shareEscrow: fundPda("share_escrow"),
        payoutVault: fundPda("payout_vault"),
      })
      .rpc();
    console.log(`定价完成：${sig}`);
  } catch (e) {
    const name = errorName(e);
    if (!name || !FIX_SKIPPABLE.includes(name)) throw e;
    console.log(`跳过定价（${name}），继续交易`);
  }

  const intervalMs = f.config.minTradeIntervalSecs.toNumber() * 1000;
  for (;;) {
    const s = (await program.account.fund.fetch(fund)).state;
    // 最早一期未卖完的赎回资产的记录；没有时用最早一期待成交申购的记录
    const recordEpoch = s.sliceCount > 0 ? s.slices[0].epoch : s.pendingCount > 0 ? s.pending[0].epoch : null;
    await postPrice();
    try {
      const sig = await program.methods
        .trade()
        .accountsPartial({
          cranker: admin,
          fund,
          oracle,
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
        .rpc();
      console.log(`交易完成：${sig}`);
    } catch (e) {
      const name = errorName(e);
      if (name && DONE.includes(name)) {
        console.log(`停止交易：${name}`);
        break;
      }
      if (!name || !RETRY.includes(name)) throw e;
      console.log(`本次未成交（${name}），${intervalMs / 1000} 秒后重试`);
    }
    await sleep(intervalMs);
  }

  const after = await program.account.fund.fetch(fund);
  console.log(
    `目标持仓：${after.state.targetBtc ? "cbBTC" : "USDC"}；未卖完的赎回资产 ${after.state.sliceCount} 期；` +
      `第 ${after.state.epoch.toNumber()} 期开放申购赎回`,
  );
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
