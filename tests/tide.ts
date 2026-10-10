// 端到端测试：在 bankrun 里跑真实的程序，并通过修改时钟模拟多天结算。
//
// 场景：
//   第 1 天：Alice 申购 1000、Bob 申购 3000；价格高于均线 -> 基金买入 BTC
//   第 2 天：Alice 赎回全部份额；价格上涨 10%（仍高于均线）-> 卖出部分 BTC 付款
//   第 3 天：价格跌破均线 -> 基金全部换成 USDC
//   第 4 天：Bob 赎回全部 -> 份额清零
import * as anchor from "@coral-xyz/anchor";
import { BN, Program } from "@coral-xyz/anchor";
import {
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
} from "@solana/web3.js";
import {
  TOKEN_PROGRAM_ID,
  ASSOCIATED_TOKEN_PROGRAM_ID,
  MINT_SIZE,
  createInitializeMint2Instruction,
  createAssociatedTokenAccountInstruction,
  createMintToInstruction,
  getAssociatedTokenAddressSync,
  unpackAccount,
} from "@solana/spl-token";
import { startAnchor, Clock, ProgramTestContext } from "solana-bankrun";
import { BankrunProvider } from "anchor-bankrun";
import { expect } from "chai";
import { Tide } from "../target/types/tide";

const IDL = require("../target/idl/tide.json");

const USDC = 1_000_000;
const DAY = 86_400;
const usd = (x: number) => new BN(Math.round(x * USDC)); // 价格单位：微美元

describe("tide", () => {
  let context: ProgramTestContext;
  let provider: BankrunProvider;
  let program: Program<Tide>;
  let admin: Keypair;
  const alice = Keypair.generate();
  const bob = Keypair.generate();
  const usdcMint = Keypair.generate();
  const btcMint = Keypair.generate();

  let fund: PublicKey, oracle: PublicKey, market: PublicKey, shareMint: PublicKey;
  const pda = (seeds: (Buffer | Uint8Array)[]) =>
    PublicKey.findProgramAddressSync(seeds, program.programId)[0];

  let today = 20_000; // 起始日期序号（unix / 86400）

  async function send(ixs: anchor.web3.TransactionInstruction[], signers: Keypair[] = []) {
    const tx = new Transaction().add(...ixs);
    await provider.sendAndConfirm!(tx, signers);
  }

  async function tokenBalance(addr: PublicKey): Promise<bigint> {
    const info = await context.banksClient.getAccount(addr);
    if (!info) return 0n;
    const acc = unpackAccount(addr, { ...info, data: Buffer.from(info.data) } as any);
    return acc.amount;
  }

  async function warpTo(unix: number) {
    const c = await context.banksClient.getClock();
    context.warpToSlot(c.slot + 10n); // 先推进区块，避免重复交易被判为已处理
    const c2 = await context.banksClient.getClock();
    context.setClock(new Clock(c2.slot, c2.epochStartTimestamp, c2.epoch, c2.leaderScheduleEpoch, BigInt(unix)));
  }

  async function createMint(mint: Keypair, decimals: number) {
    const rent = await context.banksClient.getRent();
    await send(
      [
        SystemProgram.createAccount({
          fromPubkey: admin.publicKey,
          newAccountPubkey: mint.publicKey,
          lamports: Number(rent.minimumBalance(BigInt(MINT_SIZE))),
          space: MINT_SIZE,
          programId: TOKEN_PROGRAM_ID,
        }),
        createInitializeMint2Instruction(mint.publicKey, decimals, admin.publicKey, null),
      ],
      [mint],
    );
  }

  async function ataWith(owner: PublicKey, mint: PublicKey, amount: number) {
    const ata = getAssociatedTokenAddressSync(mint, owner, true);
    await send([
      createAssociatedTokenAccountInstruction(admin.publicKey, ata, owner, mint),
      ...(amount > 0 ? [createMintToInstruction(mint, ata, admin.publicKey, amount)] : []),
    ]);
    return ata;
  }

  async function settleDay(price: number) {
    today += 1;
    await warpTo(today * DAY + 3_600 + 30); // UTC 01:00:30，模拟预言机的价格必须在定价窗口（1 点后 60 秒）内发布
    await program.methods.setMockPrice(usd(price)).accounts({ authority: admin.publicKey, oracle }).rpc();
    const f = await program.account.fund.fetch(fund);
    await program.methods
      .settle()
      .accountsPartial({
        cranker: admin.publicKey,
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
  }

  async function claim(user: Keypair, userUsdc: PublicKey) {
    const ticket = pda([Buffer.from("ticket"), fund.toBuffer(), user.publicKey.toBuffer()]);
    const t = await program.account.ticket.fetch(ticket);
    await program.methods
      .claim()
      .accountsPartial({
        user: user.publicKey,
        fund,
        ticket,
        epochRecord: pda([Buffer.from("epoch"), fund.toBuffer(), t.epoch.toArrayLike(Buffer, "le", 8)]),
        shareMint,
        usdcMint: usdcMint.publicKey,
        userShares: getAssociatedTokenAddressSync(shareMint, user.publicKey),
        userUsdc,
        shareEscrow: pda([Buffer.from("share_escrow"), fund.toBuffer()]),
        payoutVault: pda([Buffer.from("payout_vault"), fund.toBuffer()]),
      })
      .signers([user])
      .rpc();
  }

  async function subscribe(user: Keypair, userUsdc: PublicKey, amount: number) {
    await program.methods
      .requestSubscription(new BN(amount))
      .accountsPartial({
        user: user.publicKey,
        fund,
        ticket: pda([Buffer.from("ticket"), fund.toBuffer(), user.publicKey.toBuffer()]),
        userUsdc,
        subscriptionEscrow: pda([Buffer.from("subscription_escrow"), fund.toBuffer()]),
      })
      .signers([user])
      .rpc();
  }

  async function redeem(user: Keypair, shares: bigint) {
    await program.methods
      .requestRedemption(new BN(shares.toString()))
      .accountsPartial({
        user: user.publicKey,
        fund,
        ticket: pda([Buffer.from("ticket"), fund.toBuffer(), user.publicKey.toBuffer()]),
        userShares: getAssociatedTokenAddressSync(shareMint, user.publicKey),
        shareEscrow: pda([Buffer.from("share_escrow"), fund.toBuffer()]),
      })
      .signers([user])
      .rpc();
  }

  let aliceUsdc: PublicKey, bobUsdc: PublicKey;

  before(async () => {
    context = await startAnchor(".", [], []);
    provider = new BankrunProvider(context);
    anchor.setProvider(provider);
    program = new Program<Tide>(IDL, provider);
    admin = (provider.wallet as anchor.Wallet).payer;
    await warpTo(today * DAY);

    // 给用户转一些 SOL 付手续费和租金
    await send([
      SystemProgram.transfer({ fromPubkey: admin.publicKey, toPubkey: alice.publicKey, lamports: 1e9 }),
      SystemProgram.transfer({ fromPubkey: admin.publicKey, toPubkey: bob.publicKey, lamports: 1e9 }),
    ]);
    await createMint(usdcMint, 6);
    await createMint(btcMint, 8);
    aliceUsdc = await ataWith(alice.publicKey, usdcMint.publicKey, 10_000 * USDC);
    bobUsdc = await ataWith(bob.publicKey, usdcMint.publicKey, 10_000 * USDC);

    oracle = pda([Buffer.from("oracle"), admin.publicKey.toBuffer()]);
    market = pda([Buffer.from("market"), admin.publicKey.toBuffer()]);
    fund = pda([Buffer.from("fund"), admin.publicKey.toBuffer()]);
    shareMint = pda([Buffer.from("share_mint"), fund.toBuffer()]);

    await program.methods.initMockOracle(usd(50_000)).accounts({ authority: admin.publicKey }).rpc();
    await program.methods
      .initMockMarket(18) // 模拟 Binance：0.075% 手续费 + 约 0.1% 滑点
      .accounts({ authority: admin.publicKey, usdcMint: usdcMint.publicKey, btcMint: btcMint.publicKey })
      .rpc();
    // 给模拟交易所注入流动性
    await send([
      createMintToInstruction(usdcMint.publicKey, pda([Buffer.from("market_usdc"), market.toBuffer()]), admin.publicKey, 1_000_000 * USDC),
      createMintToInstruction(btcMint.publicKey, pda([Buffer.from("market_btc"), market.toBuffer()]), admin.publicKey, 100 * 1e8),
    ]);

    await program.methods
      .initialize({
        window: 50,
        settleOffsetSecs: new BN(3_600),
        maxSlippageBps: 30,
        subscriptionFeeBps: 0,
        redemptionFeeBps: 0,
        minSubscription: new BN(USDC),
        maxStalenessSecs: new BN(600),
      })
      .accountsPartial({ admin: admin.publicKey, fund, usdcMint: usdcMint.publicKey, btcMint: btcMint.publicKey, shareMint, oracle, market })
      .rpc();
    await program.methods
      .initVaults()
      .accountsPartial({ admin: admin.publicKey, fund, usdcMint: usdcMint.publicKey, btcMint: btcMint.publicKey, shareMint })
      .rpc();
    await program.methods
      .seedPrices(Array(49).fill(usd(50_000)))
      .accountsPartial({ admin: admin.publicKey, fund })
      .rpc();
  });

  it("第1天：申购后价格高于均线，买入 BTC", async () => {
    await subscribe(alice, aliceUsdc, 1_000 * USDC);
    await subscribe(bob, bobUsdc, 3_000 * USDC);
    await settleDay(55_000);

    const f = await program.account.fund.fetch(fund);
    expect(f.state.inBtc).to.equal(true);
    expect(await tokenBalance(pda([Buffer.from("usdc_vault"), fund.toBuffer()]))).to.equal(0n);
    expect(Number(await tokenBalance(pda([Buffer.from("btc_vault"), fund.toBuffer()])))).to.be.greaterThan(0);

    await claim(alice, aliceUsdc);
    await claim(bob, bobUsdc);
    const aShares = await tokenBalance(getAssociatedTokenAddressSync(shareMint, alice.publicKey));
    const bShares = await tokenBalance(getAssociatedTokenAddressSync(shareMint, bob.publicKey));
    expect(aShares).to.equal(BigInt(1_000 * USDC));
    expect(bShares).to.equal(BigInt(3_000 * USDC));
  });

  it("同一天不能结算两次", async () => {
    const f = await program.account.fund.fetch(fund);
    let failed = false;
    try {
      await program.methods
        .settle()
        .accountsPartial({
          cranker: admin.publicKey,
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
    } catch (e) {
      failed = true;
    }
    expect(failed).to.equal(true);
  });

  it("第2天：Alice 赎回，价格涨 10%，按新净值付款", async () => {
    const aShares = await tokenBalance(getAssociatedTokenAddressSync(shareMint, alice.publicKey));
    const before = await tokenBalance(aliceUsdc);
    await redeem(alice, aShares);
    await settleDay(60_500);
    await claim(alice, aliceUsdc);
    const got = Number((await tokenBalance(aliceUsdc)) - before) / USDC;
    // 1000 × 1.1，扣掉买入时 0.18% 的成交成本，约 1098
    expect(got).to.be.within(1_090, 1_100);
    const f = await program.account.fund.fetch(fund);
    expect(f.state.inBtc).to.equal(true);
    expect(f.state.shareSupply.toNumber()).to.equal(3_000 * USDC);
  });

  it("第3天：跌破均线，全部换成 USDC", async () => {
    await settleDay(40_000);
    const f = await program.account.fund.fetch(fund);
    expect(f.state.inBtc).to.equal(false);
    expect(await tokenBalance(pda([Buffer.from("btc_vault"), fund.toBuffer()]))).to.equal(0n);
  });

  it("第4天：Bob 全部赎回，份额清零", async () => {
    const bShares = await tokenBalance(getAssociatedTokenAddressSync(shareMint, bob.publicKey));
    const before = await tokenBalance(bobUsdc);
    await redeem(bob, bShares);
    await settleDay(41_000);
    await claim(bob, bobUsdc);
    const got = Number((await tokenBalance(bobUsdc)) - before) / USDC;
    expect(got).to.be.greaterThan(2_000);
    const f = await program.account.fund.fetch(fund);
    expect(f.state.shareSupply.toNumber()).to.equal(0);
    expect(await tokenBalance(pda([Buffer.from("share_escrow"), fund.toBuffer()]))).to.equal(0n);
  });
});
