// 每日结算脚本（devnet 原型用）
//
// 做两件事：
//   1. 从 Binance 公开行情读取 BTC 价格，写入模拟预言机（主网版本应改用 Pyth，删除这一步）
//   2. 调用 settle 完成当天结算
//
// 用法（每天 UTC 01:00 用 cron 运行一次。模拟预言机的价格就是定价价格，
// 必须在 1 点后 60 秒内写入并结算，否则当天会被拒绝，第二天再结算）：
//   ANCHOR_PROVIDER_URL=https://api.devnet.solana.com \
//   ANCHOR_WALLET=~/.config/solana/id.json \
//   yarn ts-node scripts/crank.ts
//
// 注意：模拟预言机由管理员钱包签名，所以这个脚本必须用管理员钱包运行。
import * as anchor from "@coral-xyz/anchor";
import { BN, Program } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";
import { Tide } from "../target/types/tide";

const IDL = require("../target/idl/tide.json");

async function btcPriceMicroUsd(): Promise<BN> {
  const r = await fetch("https://api.binance.com/api/v3/ticker/price?symbol=BTCUSDT");
  if (!r.ok) throw new Error(`读取价格失败：HTTP ${r.status}`);
  const j: any = await r.json();
  const p = Number(j.price);
  if (!(p > 0)) throw new Error(`价格无效：${j.price}`);
  return new BN(Math.round(p * 1_000_000));
}

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

  const price = await btcPriceMicroUsd();
  await program.methods.setMockPrice(price).accounts({ authority: admin, oracle }).rpc();
  console.log(`已写入价格：${price.toNumber() / 1e6} USD`);

  const f = await program.account.fund.fetch(fund);
  const sig = await program.methods
    .settle()
    .accountsPartial({
      cranker: admin,
      fund,
      epochRecord: pda([Buffer.from("epoch"), fund.toBuffer(), f.state.epoch.toArrayLike(Buffer, "le", 8)]),
      oracle,
      market,
      marketUsdc: pda([Buffer.from("market_usdc"), market.toBuffer()]),
      marketBtc: pda([Buffer.from("market_btc"), market.toBuffer()]),
      shareMint,
      usdcVault: pda([Buffer.from("usdc_vault"), fund.toBuffer()]),
      btcVault: pda([Buffer.from("btc_vault"), fund.toBuffer()]),
      subscriptionEscrow: pda([Buffer.from("subscription_escrow"), fund.toBuffer()]),
      shareEscrow: pda([Buffer.from("share_escrow"), fund.toBuffer()]),
      payoutVault: pda([Buffer.from("payout_vault"), fund.toBuffer()]),
    })
    .rpc();
  const after = await program.account.fund.fetch(fund);
  console.log(`结算完成：${sig}`);
  console.log(`当前持有：${after.state.inBtc ? "BTC" : "USDC"}，第 ${after.state.epoch.toNumber()} 期开放申购赎回`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
