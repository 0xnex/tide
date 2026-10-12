// 创建一个 devnet（或本地验证器）上的基金：价格用 Pyth，代币和交易所是模拟的（原型专用）。
//
// 步骤与 README 的 “Deploy to devnet” 一致：
//   模拟 USDC（6 位）和 BTC（8 位）代币 -> init_mock_market + 注入流动性
//   -> initialize -> init_vaults -> seed_prices（最近 49 天，都用 Pyth 的最新 BTC/USD 价格）
// 可选：DEMO_USDC=<数量> 给钱包铸造模拟 USDC 并申购这么多，让第一次 crank 有事可做。
//
// 用法（钱包同时是管理员、清算地址和收费地址，程序已经 anchor deploy）：
//   ANCHOR_PROVIDER_URL=https://api.devnet.solana.com ANCHOR_WALLET=~/.config/solana/id.json \
//   yarn ts-node scripts/setup.ts
//
// 环境变量（都可选）：
//   SEED_PRICE           种子价格（USD），默认取 Pyth 的最新 BTC/USD 价格（要 PYTH_API_KEY）
//   PYTH_API_KEY         Hermes 的 API key（从 Pyth Terminal 申请）
//   SETTLE_OFFSET_SECS   定价时刻在 UTC 0 点之后多少秒，默认 3600（01:00）；以后不能修改
//   SIGNAL_FEED          btc（默认）或 cbbtc
//   TRADE_CAP_USDC       单笔交易上限，默认 200000
//   DEMO_USDC            演示申购的 USDC 数量，默认不申购
import * as anchor from "@coral-xyz/anchor";
import { BN, Program } from "@coral-xyz/anchor";
import { Keypair, PublicKey, SystemProgram, Transaction } from "@solana/web3.js";
import {
  MINT_SIZE,
  TOKEN_PROGRAM_ID,
  createAssociatedTokenAccountIdempotentInstruction,
  createInitializeMint2Instruction,
  createMintToInstruction,
  getAssociatedTokenAddressSync,
} from "@solana/spl-token";
import { Tide } from "../target/types/tide";
import { initShareMetadata } from "./share-metadata";

const IDL = require("../target/idl/tide.json");

const USDC = 1_000_000;
const DAY = 86_400;

// Pyth 价格源 ID（主网和 devnet 相同）
const BTC_FEED = "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43";
const CBBTC_FEED = "2817d7bfe5c64b8ea956e9a26f573ef64e72e4d7891f2d6af9bcc93f7aff9a97";

/** 种子价格：SEED_PRICE，或者 Pyth 的最新 BTC/USD 价格 */
async function btcPriceUsd(): Promise<number> {
  if (process.env.SEED_PRICE) return Number(process.env.SEED_PRICE);
  const hermes = process.env.HERMES_URL ?? "https://hermes.pyth.network";
  const key = process.env.PYTH_API_KEY;
  if (!key) throw new Error("缺少 PYTH_API_KEY（或者用 SEED_PRICE 指定种子价格）");
  const r = await fetch(`${hermes}/v2/updates/price/latest?ids[]=${BTC_FEED}&parsed=true`, {
    headers: { Authorization: `Bearer ${key}` },
  });
  if (!r.ok) throw new Error(`Hermes 返回 HTTP ${r.status}`);
  const p = ((await r.json()) as any).parsed[0].price;
  return Number(p.price) * 10 ** p.expo;
}

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = new Program<Tide>(IDL, provider);
  const wallet = (provider.wallet as anchor.Wallet).payer;
  const admin = wallet.publicKey;
  const pda = (seeds: (Buffer | Uint8Array)[]) => PublicKey.findProgramAddressSync(seeds, program.programId)[0];
  const send = (tx: Transaction, signers: Keypair[] = []) => provider.sendAndConfirm(tx, signers);

  const fund = pda([Buffer.from("fund"), admin.toBuffer()]);
  if (await provider.connection.getAccountInfo(fund)) throw new Error(`基金已经存在：${fund.toBase58()}`);
  const market = pda([Buffer.from("market"), admin.toBuffer()]);
  const shareMint = pda([Buffer.from("share_mint"), fund.toBuffer()]);
  const marketUsdc = pda([Buffer.from("market_usdc"), market.toBuffer()]);
  const marketBtc = pda([Buffer.from("market_btc"), market.toBuffer()]);

  const price = await btcPriceUsd();
  if (!(price > 0)) throw new Error(`价格无效：${price}`);
  const usd = (x: number) => new BN(Math.round(x * USDC));
  const feed = process.env.SIGNAL_FEED ?? "btc";
  if (feed !== "btc" && feed !== "cbbtc") throw new Error(`SIGNAL_FEED 只能是 btc 或 cbbtc：${feed}`);
  const btcFeed = feed === "btc";

  // 1. 模拟代币，钱包是铸币权限
  const usdcMint = Keypair.generate();
  const btcMint = Keypair.generate();
  const rent = await provider.connection.getMinimumBalanceForRentExemption(MINT_SIZE);
  const tx = new Transaction();
  for (const [mint, decimals] of [
    [usdcMint, 6],
    [btcMint, 8],
  ] as const) {
    tx.add(
      SystemProgram.createAccount({
        fromPubkey: admin,
        newAccountPubkey: mint.publicKey,
        lamports: rent,
        space: MINT_SIZE,
        programId: TOKEN_PROGRAM_ID,
      }),
      createInitializeMint2Instruction(mint.publicKey, decimals, admin, null),
    );
  }
  await send(tx, [usdcMint, btcMint]);
  console.log(`模拟 USDC：${usdcMint.publicKey.toBase58()}，模拟 BTC：${btcMint.publicKey.toBase58()}`);

  // 2. 模拟交易所（按 Pyth 价格成交，0.18% 手续费），注入 100 万 USDC 和 20 BTC 的流动性
  await program.methods
    .initMockMarket(18)
    .accounts({ authority: admin, usdcMint: usdcMint.publicKey, btcMint: btcMint.publicKey })
    .rpc();
  await send(
    new Transaction().add(
      createMintToInstruction(usdcMint.publicKey, marketUsdc, admin, 1_000_000 * USDC),
      createMintToInstruction(btcMint.publicKey, marketBtc, admin, 20 * 1e8),
    ),
  );

  // 3. 基金：钱包同时是管理员、清算地址和收费地址
  const settleOffset = Number(process.env.SETTLE_OFFSET_SECS ?? 3_600);
  await program.methods
    .initialize({
      window: 50,
      settleOffsetSecs: new BN(settleOffset),
      maxSlippageBps: 30,
      subscriptionFeeBps: 0,
      minSubscription: new BN(USDC),
      depegThresholdBps: 200,
      tradeCapUsdc: usd(Number(process.env.TRADE_CAP_USDC ?? 200_000)),
      minTradeIntervalSecs: new BN(60),
      keeperWindowSecs: new BN(1_800),
      publicSlippageBps: 10,
      withdrawalFeeBps: 100,
      subscriptionCapUsdc: new BN(1_000_000 * USDC),
      signalFeed: btcFeed ? { btc: {} } : { cbBtc: {} },
      maxConfidenceBps: 100,
      maxStalenessSecs: new BN(120), // 交易用的 Pyth 价格最多 2 分钟
      keeper: admin,
      feeAddress: admin,
      btcFeedId: [...Buffer.from(BTC_FEED, "hex")],
      cbbtcFeedId: [...Buffer.from(CBBTC_FEED, "hex")],
    })
    .accountsPartial({
      admin,
      fund,
      usdcMint: usdcMint.publicKey,
      btcMint: btcMint.publicKey,
      shareMint,
      market,
    })
    .rpc();
  await program.methods
    .initVaults()
    .accountsPartial({ admin, fund, usdcMint: usdcMint.publicKey, btcMint: btcMint.publicKey, shareMint })
    .rpc();
  // 最近 49 天的历史价格（原型：都用当前价格），均线从第一次定价就能计算
  await program.methods
    .seedPrices(Array(49).fill(usd(price)))
    .accountsPartial({ admin, fund })
    .rpc();
  // 份额代币的名称、符号和图标（Metaplex 元数据），钱包里显示成基金份额
  console.log(`份额元数据：${await initShareMetadata(program, admin)}`);
  console.log(`基金：${fund.toBase58()}（定价时刻 UTC 0 点后 ${settleOffset} 秒，信号 ${btcFeed ? "BTC/USD" : "cbBTC/USD"}）`);

  // 4. 可选：演示申购
  const demo = Number(process.env.DEMO_USDC ?? 0);
  if (demo > 0) {
    const userUsdc = getAssociatedTokenAddressSync(usdcMint.publicKey, admin);
    await send(
      new Transaction().add(
        createAssociatedTokenAccountIdempotentInstruction(admin, userUsdc, admin, usdcMint.publicKey),
        createMintToInstruction(usdcMint.publicKey, userUsdc, admin, Math.round(demo * USDC)),
      ),
    );
    // 期号：下一个定价时刻那天的日期序号（按链上时钟）
    const now = (await provider.connection.getBlockTime(await provider.connection.getSlot()))!;
    const day = Math.floor(now / DAY);
    const epoch = now < day * DAY + settleOffset ? day : day + 1;
    await program.methods
      .requestSubscription(usd(demo), new BN(epoch))
      .accountsPartial({
        user: admin,
        fund,
        ticket: pda([Buffer.from("ticket"), fund.toBuffer(), admin.toBuffer(), new BN(epoch).toArrayLike(Buffer, "le", 8)]),
        userUsdc,
        subscriptionEscrow: pda([Buffer.from("subscription_escrow"), fund.toBuffer()]),
      })
      .rpc();
    console.log(`演示申购 ${demo} USDC，第 ${epoch} 期`);
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
