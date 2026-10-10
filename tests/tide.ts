// 端到端测试：在 bankrun 里跑真实的程序，并通过修改时钟模拟多天结算。
// 单笔交易上限设为 1,000 USDC，每天的调仓要分成几笔、间隔 60 秒完成。
//
// 场景：
//   第 1 天：Alice 申购 1000、Bob 申购 3000；价格高于均线 -> 分 4 笔为申购买入 cbBTC，中途先领一部分份额
//   第 2 天：Alice 赎回全部份额；价格上涨 10% -> 分 2 笔卖出她那份 cbBTC，卖完才能领取；
//            中途一笔成交低于 min_out，只回滚那一笔
//   第 3 天：价格跌破均线 -> 分几笔全部换成 USDC；清算窗口内 Alice 不能交易，窗口之后可以
//   第 4 天：Bob 赎回全部 -> 基金只有 USDC，赎回资产定价时就完成，份额清零
//   第 5 天：Alice 再申购；BTC 高于均线，但 cbBTC 比 BTC 低 3%（脱锚）-> 不买入，申购款留在托管里
//   第 6 天：恢复锚定 -> 为 Alice 买入 cbBTC，她领到份额
import * as anchor from "@coral-xyz/anchor";
import { BN, Program } from "@coral-xyz/anchor";
import { Keypair, PublicKey, SystemProgram, Transaction } from "@solana/web3.js";
import {
  TOKEN_PROGRAM_ID,
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
const FIX_OFFSET = 3_600; // UTC 01:00
const TRADE_INTERVAL = 60;
const KEEPER_WINDOW = 1_800; // 01:00–01:30 只有清算地址能驱动结算
const PUBLIC_SLIPPAGE_BPS = 10; // 窗口之后其他人的滑点上限
const MARKET_FEE_BPS = 18; // 模拟交易所的手续费：0.075% 手续费 + 约 0.1% 滑点
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
  const fundPda = (name: string) => pda([Buffer.from(name), fund.toBuffer()]);
  const epochPda = (epoch: BN) =>
    pda([Buffer.from("epoch"), fund.toBuffer(), epoch.toArrayLike(Buffer, "le", 8)]);

  let today = 20_000; // 起始日期序号（unix / 86400）
  let now = 0; // 当前时钟（unix 秒）
  let price = 50_000; // 模拟预言机当前的 BTC/USD
  let valuation = 50_000; // 模拟预言机当前的 cbBTC/USD
  let pricedAt = 0; // 最近一次写入价格时的时钟

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
    now = unix;
  }

  /** 调用失败时返回程序的错误名，成功时返回 null */
  async function errorName(p: Promise<unknown>): Promise<string | null> {
    try {
      await p;
      return null;
    } catch (e: any) {
      const code = e?.error?.errorCode?.code;
      if (code) return code;
      const text = `${e}\n${(e?.logs ?? []).join("\n")}`;
      const m = text.match(/Error Code: (\w+)/);
      if (m) return m[1];
      const hex = text.match(/custom program error: (0x[0-9a-f]+)/i);
      if (hex) {
        const n = parseInt(hex[1], 16);
        const err = IDL.errors.find((x: any) => x.code === n);
        if (err) return err.name.charAt(0).toUpperCase() + err.name.slice(1);
      }
      throw e;
    }
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

  async function setPrice(p: number, v: number = p) {
    price = p;
    valuation = v;
    pricedAt = now;
    await program.methods
      .setMockPrice(usd(p), usd(v))
      .accountsPartial({ authority: admin.publicKey, oracle })
      .rpc();
  }

  async function fixIx(signer: Keypair = admin) {
    const f = await program.account.fund.fetch(fund);
    return program.methods
      .fix()
      .accountsPartial({
        cranker: signer.publicKey,
        fund,
        epochRecord: epochPda(f.state.epoch),
        oracle,
        shareMint,
        usdcVault: fundPda("usdc_vault"),
        subscriptionEscrow: fundPda("subscription_escrow"),
        shareEscrow: fundPda("share_escrow"),
        payoutVault: fundPda("payout_vault"),
      })
      .signers(signer === admin ? [] : [signer]);
  }

  // 第二天 01:00:30 写入两个价格并定价（模拟预言机的价格必须在定价窗口内发布）。
  // price：BTC/USD 信号价格；valuation：cbBTC/USD 估值价格，默认与 BTC 同价。
  // 先确认清算窗口内其他人不能定价
  async function fixDay(p: number, v: number = p) {
    today += 1;
    await warpTo(today * DAY + FIX_OFFSET + 30);
    await setPrice(p, v);
    expect(await errorName((await fixIx(alice)).rpc())).to.equal("KeeperOnly");
    await (await fixIx()).rpc();
  }

  /** 在当前时钟交易一笔（默认由清算地址签名）；卖出赎回资产时带上最早一期未卖完的记录。返回错误名或 null */
  async function trade(signer: Keypair = admin): Promise<string | null> {
    // 保持预言机价格新鲜（同一时刻已经写入过就不再写，避免重复交易）
    if (pricedAt !== now) await setPrice(price, valuation);
    const f = await program.account.fund.fetch(fund);
    // 最早一期未卖完的赎回资产的记录；没有时用最早一期待成交申购的记录
    const recordEpoch =
      f.state.sliceCount > 0 ? f.state.slices[0].epoch : f.state.pendingCount > 0 ? f.state.pending[0].epoch : null;
    return errorName(
      program.methods
        .trade()
        .accountsPartial({
          cranker: signer.publicKey,
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
        .signers(signer === admin ? [] : [signer])
        .rpc(),
    );
  }

  /** 每隔最小间隔交易一笔，直到程序回答没有可交易的。返回成交笔数和停下的原因 */
  async function tradeUntilDone(): Promise<[number, string]> {
    let n = 0;
    for (;;) {
      await warpTo(now + TRADE_INTERVAL);
      const err = await trade();
      if (err) return [n, err];
      n += 1;
    }
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
        epochRecord: epochPda(t.epoch),
        shareMint,
        usdcMint: usdcMint.publicKey,
        userShares: getAssociatedTokenAddressSync(shareMint, user.publicKey),
        userUsdc,
        shareEscrow: fundPda("share_escrow"),
        payoutVault: fundPda("payout_vault"),
        subscriptionEscrow: fundPda("subscription_escrow"),
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
        subscriptionEscrow: fundPda("subscription_escrow"),
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
        shareEscrow: fundPda("share_escrow"),
      })
      .signers([user])
      .rpc();
  }

  async function setMarketFee(bps: number) {
    await program.methods.setMockMarketFee(bps).accountsPartial({ authority: admin.publicKey, market }).rpc();
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

    await program.methods.initMockOracle(usd(50_000), usd(50_000)).accounts({ authority: admin.publicKey }).rpc();
    await program.methods
      .initMockMarket(MARKET_FEE_BPS)
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
        settleOffsetSecs: new BN(FIX_OFFSET),
        maxSlippageBps: 30,
        subscriptionFeeBps: 0,
        redemptionFeeBps: 0,
        minSubscription: new BN(USDC),
        depegThresholdBps: 200,
        tradeCapUsdc: new BN(1_000 * USDC),
        minTradeIntervalSecs: new BN(TRADE_INTERVAL),
        keeperWindowSecs: new BN(KEEPER_WINDOW),
        publicSlippageBps: PUBLIC_SLIPPAGE_BPS,
        maxStalenessSecs: new BN(600),
        keeper: admin.publicKey, // 清算地址：这里就是管理员钱包
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

  it("第1天：价格高于均线，分 4 笔为申购买入 cbBTC，份额分次领取，间隔不足时拒绝", async () => {
    await subscribe(alice, aliceUsdc, 1_000 * USDC);
    await subscribe(bob, bobUsdc, 3_000 * USDC);
    await fixDay(55_000);

    const f = await program.account.fund.fetch(fund);
    expect(f.state.targetBtc).to.equal(true);
    const escrow = fundPda("subscription_escrow");
    expect(await tokenBalance(escrow)).to.equal(BigInt(4_000 * USDC)); // 待成交申购留在托管里

    // 第一笔 1000 USDC：基金还没有份额，1 USDC 换 1 份；Alice 先领她那 1/4
    expect(await trade()).to.equal(null);
    expect(await tokenBalance(escrow)).to.equal(BigInt(3_000 * USDC));
    await claim(alice, aliceUsdc);
    const aliceShares = getAssociatedTokenAddressSync(shareMint, alice.publicKey);
    expect(await tokenBalance(aliceShares)).to.equal(BigInt(250 * USDC));
    await warpTo(now); // 推进区块：同样的领取交易换一个区块哈希，否则会被当成重复交易
    expect(await errorName(claim(alice, aliceUsdc))).to.equal("NothingToClaim");

    await warpTo(now + TRADE_INTERVAL - 1);
    expect(await trade()).to.equal("TradeTooSoon");
    const [n, why] = await tradeUntilDone();
    expect([n, why]).to.deep.equal([3, "NothingToTrade"]);
    expect(await tokenBalance(escrow)).to.equal(0n);
    expect(await tokenBalance(fundPda("usdc_vault"))).to.equal(0n);
    expect(Number(await tokenBalance(fundPda("btc_vault")))).to.be.greaterThan(0);

    // 全部成交：两人领完，总数精确等于发行的份额，比例约 1:3
    await claim(alice, aliceUsdc);
    await claim(bob, bobUsdc);
    const aShares = await tokenBalance(aliceShares);
    const bShares = await tokenBalance(getAssociatedTokenAddressSync(shareMint, bob.publicKey));
    const f1 = await program.account.fund.fetch(fund);
    expect(aShares + bShares).to.equal(BigInt(f1.state.shareSupply.toString()));
    expect(Number(bShares) / Number(aShares)).to.be.closeTo(3, 1e-6);
    expect(await tokenBalance(fundPda("share_escrow"))).to.equal(0n);
  });

  it("同一天不能定价两次", async () => {
    expect(await errorName((await fixIx()).rpc())).to.equal("AlreadySettled");
  });

  it("第2天：Alice 赎回，卖完她那份 cbBTC 才能领取，拿到实际卖出所得", async () => {
    const aShares = await tokenBalance(getAssociatedTokenAddressSync(shareMint, alice.publicKey));
    const before = await tokenBalance(aliceUsdc);
    await redeem(alice, aShares);
    await fixDay(60_500);
    const f0 = await program.account.fund.fetch(fund);
    expect(f0.state.sliceCount).to.equal(1);
    const bShares = await tokenBalance(getAssociatedTokenAddressSync(shareMint, bob.publicKey));
    expect(BigInt(f0.state.shareSupply.toString())).to.equal(bShares);

    // 约 1,100 USDC 的 cbBTC，第一笔卖 1,000
    expect(await trade()).to.equal(null);
    expect(await errorName(claim(alice, aliceUsdc))).to.equal("SliceNotSold");

    // 交易所只给 0.4% 以下的价格：这一笔回滚，余额不变
    await setMarketFee(40);
    await warpTo(now + TRADE_INTERVAL);
    const btcBefore = await tokenBalance(fundPda("btc_vault"));
    expect(await trade()).to.equal("SlippageExceeded");
    expect(await tokenBalance(fundPda("btc_vault"))).to.equal(btcBefore);
    await setMarketFee(MARKET_FEE_BPS);

    // 下一笔照常卖完；之后基金已经是目标持仓，没有可交易的
    const [n, why] = await tradeUntilDone();
    expect([n, why]).to.deep.equal([1, "NothingToTrade"]);
    const f = await program.account.fund.fetch(fund);
    expect(f.state.sliceCount).to.equal(0);
    const r = await program.account.epochRecord.fetch(epochPda(f0.state.epoch.subn(1)));
    expect(r.sliceSoldOut).to.equal(true);

    await claim(alice, aliceUsdc);
    const got = Number((await tokenBalance(aliceUsdc)) - before) / USDC;
    expect(got).to.equal(r.payoutUsdc.toNumber() / USDC);
    // 1000 × 1.1，扣掉买入和卖出各 0.18% 的成交成本，约 1096
    expect(got).to.be.within(1_090, 1_100);
    expect(await tokenBalance(fundPda("payout_vault"))).to.equal(0n);
  });

  it("第3天：跌破均线，分几笔全部换成 USDC；窗口内只有清算地址能交易", async () => {
    await fixDay(40_000);
    const f0 = await program.account.fund.fetch(fund);
    expect(f0.state.targetBtc).to.equal(false);

    // 01:01：Alice 不是清算地址，被拒绝
    await warpTo(now + TRADE_INTERVAL);
    expect(await trade(alice)).to.equal("KeeperOnly");
    // 01:30 之后谁都可以，但滑点上限是 0.1%：交易所收 0.18% 时这一笔回滚，收 0.05% 时成交
    await warpTo(today * DAY + FIX_OFFSET + KEEPER_WINDOW);
    expect(await trade(alice)).to.equal("SlippageExceeded");
    await setMarketFee(5);
    await warpTo(now + 1);
    expect(await trade(alice)).to.equal(null);
    await setMarketFee(MARKET_FEE_BPS);

    const [n, why] = await tradeUntilDone();
    expect(n).to.be.greaterThan(1);
    expect(why).to.equal("NothingToTrade");
    expect(await tokenBalance(fundPda("btc_vault"))).to.equal(0n);
  });

  it("第4天：Bob 全部赎回，基金只有 USDC，定价后直接领取，份额清零", async () => {
    const bShares = await tokenBalance(getAssociatedTokenAddressSync(shareMint, bob.publicKey));
    const before = await tokenBalance(bobUsdc);
    await redeem(bob, bShares);
    await fixDay(41_000);
    await claim(bob, bobUsdc);
    const got = Number((await tokenBalance(bobUsdc)) - before) / USDC;
    expect(got).to.be.greaterThan(2_000);
    const f = await program.account.fund.fetch(fund);
    expect(f.state.shareSupply.toNumber()).to.equal(0);
    expect(await tokenBalance(fundPda("share_escrow"))).to.equal(0n);
    expect(await tokenBalance(fundPda("payout_vault"))).to.equal(0n);
    expect(await trade()).to.equal("NothingToTrade");
  });

  it("第5天：cbBTC 脱锚，不买入，申购款留在托管里，定价照常完成", async () => {
    await subscribe(alice, aliceUsdc, 1_000 * USDC);
    const f0 = await program.account.fund.fetch(fund);
    await fixDay(52_000, 50_440); // cbBTC 比 BTC 低 3%，超过 2% 阈值
    const r = await program.account.epochRecord.fetch(epochPda(f0.state.epoch));
    expect(r.signalPrice.toString()).to.equal(usd(52_000).toString());
    expect(r.valuationPrice.toString()).to.equal(usd(50_440).toString());
    expect(r.targetBtc).to.equal(true); // 目标仍是 cbBTC
    expect(await trade()).to.equal("DepegBlocksBuy");
    expect(await tokenBalance(fundPda("btc_vault"))).to.equal(0n);
    expect(await tokenBalance(fundPda("subscription_escrow"))).to.equal(BigInt(1_000 * USDC));
    expect(await errorName(claim(alice, aliceUsdc))).to.equal("NothingToClaim");
  });

  it("第6天：恢复锚定，为 Alice 买入 cbBTC，她领到份额", async () => {
    await fixDay(53_000);
    const [n, why] = await tradeUntilDone();
    expect(n).to.be.greaterThan(0);
    expect(why).to.equal("NothingToTrade");
    const f = await program.account.fund.fetch(fund);
    expect(f.state.btc.toNumber()).to.be.greaterThan(0);
    expect(f.state.pendingCount).to.equal(0);
    expect(await tokenBalance(fundPda("btc_vault"))).to.equal(BigInt(f.state.btc.toString()));
    expect(await tokenBalance(fundPda("subscription_escrow"))).to.equal(0n);
    const before = await tokenBalance(getAssociatedTokenAddressSync(shareMint, alice.publicKey));
    await claim(alice, aliceUsdc);
    const after = await tokenBalance(getAssociatedTokenAddressSync(shareMint, alice.publicKey));
    expect(after - before).to.equal(BigInt(f.state.shareSupply.toString()));
  });
});
