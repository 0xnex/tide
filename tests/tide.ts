// 端到端测试：在 bankrun 里跑真实的程序，并通过修改时钟模拟多天结算。
// 单笔交易上限设为 1,000 USDC，每天的调仓要分成几笔、间隔 60 秒完成。
//
// 场景：
//   第 1 天：Alice 申购 1000、Bob 申购 3000；价格高于均线 -> 分 4 笔为申购买入 cbBTC，中途先领一部分份额
//   第 2 天：Alice 赎回全部份额；价格上涨 10% -> 分 2 笔卖出她那份 cbBTC，卖完才能领取，1% 取款费付给收费地址；
//            中途一笔成交低于 min_out，只回滚那一笔
//   第 3 天：价格跌破均线 -> 分几笔全部换成 USDC；清算窗口内 Alice 不能交易，窗口之后可以
//   第 4 天：Bob 赎回全部 -> 基金只有 USDC，赎回资产定价时就完成，份额清零
//   第 5 天：Alice 再申购；BTC 高于均线，但 cbBTC 比 BTC 低 3%（脱锚）-> 不买入，申购款留在托管里
//   第 6 天：恢复锚定 -> 为 Alice 买入 cbBTC，她领到份额
//   治理：配置修改等 48 小时时间锁；申购可以立即暂停，赎回不受影响；只有管理员能操作
//   申购上限：算上待成交的申购，超过就拒绝；撤销待成交申购退回 USDC、腾出额度
//   价格源：前面用 cbBTC/USD 作信号（脱锚时不买入）；治理之后换成 BTC/USD，没有 cbBTC/USD 也照常定价
//   回填：某天没人定价，第二天任何人用那天 01:00 发布的价格回填均线，只能回填一次
//   第 7 天：排队从不关闭。定价时刻之后的请求进入下一期，每个用户每期一张排队记录；
//            关闭这一期的定价时刻之前可以撤销、全额退回，之后被拒绝
//
// 每个请求属于关闭它的定价时刻那一期（期号是那天的日期序号）。第一天的申购在第 0 天提交，
// 那天没有定价，顺延到第 1 天的定价：领取时用之后第一次定价的记录。
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
const SUBSCRIPTION_CAP = 5_000; // 申购上限（USDC），比默认的 100 万小，方便测试
const MARKET_FEE_BPS = 18; // 模拟交易所的手续费：0.075% 手续费 + 约 0.1% 滑点
const usd = (x: number) => new BN(Math.round(x * USDC)); // 价格单位：微美元

describe("tide", () => {
  let context: ProgramTestContext;
  let provider: BankrunProvider;
  let program: Program<Tide>;
  let admin: Keypair;
  const alice = Keypair.generate();
  const bob = Keypair.generate();
  const manager = Keypair.generate(); // 收费地址：取款费付到它的 USDC 关联代币账户
  const usdcMint = Keypair.generate();
  const btcMint = Keypair.generate();

  let fund: PublicKey, oracle: PublicKey, market: PublicKey, shareMint: PublicKey;
  const pda = (seeds: (Buffer | Uint8Array)[]) =>
    PublicKey.findProgramAddressSync(seeds, program.programId)[0];
  const fundPda = (name: string) => pda([Buffer.from(name), fund.toBuffer()]);
  const le = (n: number | BN) => new BN(n).toArrayLike(Buffer, "le", 8);
  const epochPda = (epoch: number | BN) => pda([Buffer.from("epoch"), fund.toBuffer(), le(epoch)]);
  const ticketPda = (user: PublicKey, epoch: number) =>
    pda([Buffer.from("ticket"), fund.toBuffer(), user.toBuffer(), le(epoch)]);
  /** 这一刻提交的请求属于哪一期：定价时刻之前是当天，之后是第二天 */
  const requestEpoch = () => {
    const day = Math.floor(now / DAY);
    return now < day * DAY + FIX_OFFSET ? day : day + 1;
  };
  const lastTicket = new Map<string, number>(); // 每个用户最近一次请求的期号

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
    const epoch = Math.floor(now / DAY); // 定价记录的期号：当天的日期序号
    return program.methods
      .fix(new BN(epoch))
      .accountsPartial({
        cranker: signer.publicKey,
        fund,
        epochRecord: epochPda(epoch),
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

  /** 关闭第 epoch 期的那次定价的记录：通常期号相同，定价被跳过时是之后第一次定价的；还没定价时传最新的 */
  async function recordFor(epoch: number): Promise<PublicKey> {
    for (let e = epoch; e <= today; e++) {
      if (!(await context.banksClient.getAccount(epochPda(e)))) continue;
      const r = await program.account.epochRecord.fetch(epochPda(e));
      if (r.firstEpoch.toNumber() <= epoch) return epochPda(e);
    }
    return epochPda(today);
  }

  async function claim(user: Keypair, userUsdc: PublicKey, epoch = lastTicket.get(user.publicKey.toBase58())!) {
    await program.methods
      .claim()
      .accountsPartial({
        user: user.publicKey,
        fund,
        ticket: ticketPda(user.publicKey, epoch),
        epochRecord: await recordFor(epoch),
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

  const managerUsdc = () => getAssociatedTokenAddressSync(usdcMint.publicKey, manager.publicKey);

  /** 任何人都可以结清欠管理人的取款费 */
  async function collectFees(signer: Keypair) {
    await program.methods
      .collectFees()
      .accountsPartial({
        caller: signer.publicKey,
        fund,
        usdcMint: usdcMint.publicKey,
        feeAddress: manager.publicKey,
        feeUsdc: managerUsdc(),
        payoutVault: fundPda("payout_vault"),
      })
      .signers([signer])
      .rpc();
  }

  /** 申购，返回这张排队记录的期号 */
  async function subscribe(user: Keypair, userUsdc: PublicKey, amount: number): Promise<number> {
    const epoch = requestEpoch();
    await program.methods
      .requestSubscription(new BN(amount), new BN(epoch))
      .accountsPartial({
        user: user.publicKey,
        fund,
        ticket: ticketPda(user.publicKey, epoch),
        userUsdc,
        subscriptionEscrow: fundPda("subscription_escrow"),
      })
      .signers([user])
      .rpc();
    lastTicket.set(user.publicKey.toBase58(), epoch);
    return epoch;
  }

  /** 赎回，返回这张排队记录的期号 */
  async function redeem(user: Keypair, shares: bigint): Promise<number> {
    const epoch = requestEpoch();
    await program.methods
      .requestRedemption(new BN(shares.toString()), new BN(epoch))
      .accountsPartial({
        user: user.publicKey,
        fund,
        ticket: ticketPda(user.publicKey, epoch),
        userShares: getAssociatedTokenAddressSync(shareMint, user.publicKey),
        shareEscrow: fundPda("share_escrow"),
      })
      .signers([user])
      .rpc();
    lastTicket.set(user.publicKey.toBase58(), epoch);
    return epoch;
  }

  async function cancelPending(user: Keypair, userUsdc: PublicKey, epoch: number) {
    await program.methods
      .cancelPending()
      .accountsPartial({
        user: user.publicKey,
        fund,
        ticket: ticketPda(user.publicKey, epoch),
        epochRecord: await recordFor(epoch),
        shareMint,
        userShares: getAssociatedTokenAddressSync(shareMint, user.publicKey),
        userUsdc,
        subscriptionEscrow: fundPda("subscription_escrow"),
        shareEscrow: fundPda("share_escrow"),
      })
      .signers([user])
      .rpc();
  }

  async function cancel(user: Keypair, userUsdc: PublicKey, epoch: number) {
    await program.methods
      .cancel()
      .accountsPartial({
        user: user.publicKey,
        fund,
        ticket: ticketPda(user.publicKey, epoch),
        shareMint,
        userShares: getAssociatedTokenAddressSync(shareMint, user.publicKey),
        userUsdc,
        subscriptionEscrow: fundPda("subscription_escrow"),
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
        minSubscription: new BN(USDC),
        depegThresholdBps: 200,
        tradeCapUsdc: new BN(1_000 * USDC),
        minTradeIntervalSecs: new BN(TRADE_INTERVAL),
        keeperWindowSecs: new BN(KEEPER_WINDOW),
        publicSlippageBps: PUBLIC_SLIPPAGE_BPS,
        withdrawalFeeBps: 100,
        subscriptionCapUsdc: new BN(SUBSCRIPTION_CAP * USDC),
        signalFeed: { cbBtc: {} }, // 先用 cbBTC/USD 作信号，治理测试里换成 BTC/USD
        maxStalenessSecs: new BN(600),
        keeper: admin.publicKey, // 清算地址：这里就是管理员钱包
        feeAddress: manager.publicKey,
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
    const r = await program.account.epochRecord.fetch(epochPda(today));
    expect(r.sliceSoldOut).to.equal(true);

    // 领取时 1% 取款费欠给管理人，其余给 Alice；之后任何人（这里是 Bob）把取款费结清到收费地址，
    // 收费地址的 USDC 账户由他创建
    await claim(alice, aliceUsdc);
    const got = (await tokenBalance(aliceUsdc)) - before;
    const payout = BigInt(r.payoutUsdc.toString());
    const owed = BigInt((await program.account.fund.fetch(fund)).state.feesOwed.toString());
    expect(owed).to.equal(payout / 100n);
    expect(got + owed).to.equal(payout);
    expect(await tokenBalance(fundPda("payout_vault"))).to.equal(owed);
    await collectFees(bob);
    expect(await tokenBalance(managerUsdc())).to.equal(owed);
    expect((await program.account.fund.fetch(fund)).state.feesOwed.toNumber()).to.equal(0);
    // 1000 × 1.1，扣掉买入和卖出各 0.18% 的成交成本约 1096，再扣 1% 取款费
    expect(Number(got) / USDC).to.be.within(1_080, 1_090);
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
    // 待领取账户里只剩欠管理人的取款费
    expect(await tokenBalance(fundPda("payout_vault"))).to.equal(BigInt(f.state.feesOwed.toString()));
    expect(f.state.feesOwed.toNumber()).to.be.greaterThan(0);
    expect(await trade()).to.equal("NothingToTrade");
  });

  it("第5天：cbBTC 脱锚，不买入，申购款留在托管里，定价照常完成", async () => {
    await subscribe(alice, aliceUsdc, 1_000 * USDC);
    await fixDay(52_000, 50_440); // cbBTC 比 BTC 低 3%，超过 2% 阈值
    const r = await program.account.epochRecord.fetch(epochPda(today));
    // 信号用 cbBTC/USD，BTC/USD 是脱锚检查的参照
    expect(r.signalPrice.toString()).to.equal(usd(50_440).toString());
    expect(r.referencePrice.toString()).to.equal(usd(52_000).toString());
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

  it("第7天：排队从不关闭，定价时刻之前可以撤销，每期一张排队记录", async () => {
    const escrow = fundPda("subscription_escrow");
    const exists = async (addr: PublicKey) => (await context.banksClient.getAccount(addr)) !== null;

    // 第 6 天定价之后提交：属于第 7 期，排队记录的地址带期号
    const bobBefore = await tokenBalance(bobUsdc);
    const e7 = await subscribe(bob, bobUsdc, 500 * USDC);
    expect(e7).to.equal(today + 1);
    expect((await program.account.ticket.fetch(ticketPda(bob.publicKey, e7))).epoch.toNumber()).to.equal(e7);
    // 定价时刻之前撤销：全额退回，排队记录关闭
    await cancel(bob, bobUsdc, e7);
    expect(await tokenBalance(bobUsdc)).to.equal(bobBefore);
    expect(await exists(ticketPda(bob.publicKey, e7))).to.equal(false);
    await warpTo(now + 1);
    await subscribe(bob, bobUsdc, 300 * USDC);

    // 赎回同样可以撤销，份额全额退回
    const aliceShares = getAssociatedTokenAddressSync(shareMint, alice.publicKey);
    const sharesBefore = await tokenBalance(aliceShares);
    await redeem(alice, 1_000n);
    expect(await tokenBalance(aliceShares)).to.equal(sharesBefore - 1_000n);
    await cancel(alice, aliceUsdc, e7);
    expect(await tokenBalance(aliceShares)).to.equal(sharesBefore);

    // 第 7 天 01:00 之后、定价之前：第 7 期不能再撤销；新的申购进入第 8 期，Bob 同时有两期
    await warpTo((today + 1) * DAY + FIX_OFFSET + 10);
    expect(await errorName(cancel(bob, bobUsdc, e7))).to.equal("TooLateToCancel");
    const e8 = await subscribe(bob, bobUsdc, 200 * USDC);
    expect(e8).to.equal(e7 + 1);
    expect(await tokenBalance(escrow)).to.equal(BigInt(500 * USDC));

    // 第 7 天的定价只处理第 7 期
    await fixDay(54_000);
    const r = await program.account.epochRecord.fetch(epochPda(e7));
    expect([r.firstEpoch.toNumber(), r.subscriptionUsdc.toNumber()]).to.deep.equal([e7, 300 * USDC]);
    const [, why] = await tradeUntilDone();
    expect(why).to.equal("NothingToTrade");
    await claim(bob, bobUsdc, e7);
    expect(Number(await tokenBalance(getAssociatedTokenAddressSync(shareMint, bob.publicKey)))).to.be.greaterThan(0);
    expect(await exists(ticketPda(bob.publicKey, e7))).to.equal(false);
    expect(await errorName(claim(bob, bobUsdc, e8))).to.equal("EpochNotSettled");
    expect(await tokenBalance(escrow)).to.equal(BigInt(200 * USDC));
  });

  it("治理：配置修改等 48 小时，申购可以立即暂停，只有管理员能操作", async () => {
    const cfg0 = (await program.account.fund.fetch(fund)).config;
    // 改单笔上限，并把信号价格源换成 BTC/USD（比如 Pyth 没有 cbBTC/USD 的网络）
    const proposed = { ...cfg0, tradeCapUsdc: new BN(2_000 * USDC), signalFeed: { btc: {} } };
    const asAdmin = { admin: admin.publicKey, fund };

    // 其他人不能提议、生效、暂停、恢复：基金地址由管理员推出，换了签名人就对不上
    const byAlice = [
      program.methods.proposeConfig(proposed),
      program.methods.applyConfig(),
      program.methods.pauseSubscriptions(),
      program.methods.resumeSubscriptions(),
    ];
    for (const m of byAlice) {
      expect(await errorName(m.accountsPartial({ admin: alice.publicKey, fund }).signers([alice]).rpc())).to.equal(
        "ConstraintSeeds",
      );
    }

    // 提议时就校验：取款费率最多 1%；定价时刻不能改
    const bad = { ...cfg0, withdrawalFeeBps: 101 };
    expect(await errorName(program.methods.proposeConfig(bad).accountsPartial(asAdmin).rpc())).to.equal("InvalidConfig");
    const moved = { ...cfg0, settleOffsetSecs: new BN(2 * FIX_OFFSET) };
    expect(await errorName(program.methods.proposeConfig(moved).accountsPartial(asAdmin).rpc())).to.equal(
      "ConfigFieldFixed",
    );

    // 时间锁：提议之后 48 小时才能生效，时钟跳过去之后可以
    const proposedAt = now;
    await program.methods.proposeConfig(proposed).accountsPartial(asAdmin).rpc();
    expect(await errorName(program.methods.applyConfig().accountsPartial(asAdmin).rpc())).to.equal("TimelockNotElapsed");
    await warpTo(proposedAt + 2 * DAY - 1);
    expect(await errorName(program.methods.applyConfig().accountsPartial(asAdmin).rpc())).to.equal("TimelockNotElapsed");
    await warpTo(proposedAt + 2 * DAY);
    await program.methods.applyConfig().accountsPartial(asAdmin).rpc();
    const f = await program.account.fund.fetch(fund);
    expect(f.config.tradeCapUsdc.toNumber()).to.equal(2_000 * USDC);
    expect(f.config.signalFeed).to.deep.equal({ btc: {} });
    expect(f.pendingConfig).to.equal(null);
    await warpTo(now + 1);
    expect(await errorName(program.methods.applyConfig().accountsPartial(asAdmin).rpc())).to.equal("NoPendingConfig");

    // 暂停只挡新的申购，赎回照常；恢复之后又可以申购
    await program.methods.pauseSubscriptions().accountsPartial(asAdmin).rpc();
    expect(await errorName(subscribe(alice, aliceUsdc, 100 * USDC))).to.equal("SubscriptionsPaused");
    await redeem(alice, 1_000n);
    await program.methods.resumeSubscriptions().accountsPartial(asAdmin).rpc();
    await subscribe(alice, aliceUsdc, 100 * USDC);
  });

  it("回填：没人定价的一天，第二天用那天定价窗口内的价格回填", async () => {
    // 第二天 01:00:30 写入价格，但没有人定价（上一个测试把时钟往前跳过，按时钟算日期）
    today = Math.floor(now / DAY) + 1;
    const missed = today;
    await warpTo(missed * DAY + FIX_OFFSET + 30);
    await setPrice(58_000);
    const backfill = (day: number, signer: Keypair) =>
      program.methods
        .backfillPrice(new BN(day))
        .accountsPartial({ caller: signer.publicKey, fund, oracle })
        .signers([signer])
        .rpc();
    // 当天不能回填（应该由定价记录）
    expect(await errorName(backfill(missed, alice))).to.equal("BackfillOutOfRange");

    // 第二天 00:10，预言机还是那一天 01:00:30 的价格：任何人都可以回填
    await warpTo((missed + 1) * DAY + 600);
    await backfill(missed, alice);
    const f = await program.account.fund.fetch(fund);
    const slot = missed % 64;
    expect([f.state.priceDays[slot].toNumber(), f.state.prices[slot].toString()]).to.deep.equal([
      missed,
      usd(58_000).toString(),
    ]);
    await warpTo(now + 1);
    expect(await errorName(backfill(missed, bob))).to.equal("PriceAlreadyRecorded");

    // 前一天同样没人定价，但预言机现在的价格不是那天定价窗口内发布的，不能用来回填
    await setPrice(58_000);
    expect(await errorName(backfill(missed - 1, alice))).to.equal("PriceNotAtFixTime");
  });

  it("申购上限算上待成交的申购；撤销待成交申购退回没成交的 USDC、领走已成交的份额", async () => {
    const escrow = fundPda("subscription_escrow");
    const state = async () => (await program.account.fund.fetch(fund)).state;
    // 还没买完的申购：排队的（含下一期）加上待成交的
    const outstanding = async () => {
      const st = await state();
      const pending = st.pending.slice(0, st.pendingCount).reduce((x, p) => x + p.usdc.toNumber(), 0);
      return st.queuedSubscriptionUsdc.toNumber() + st.nextSubscriptionUsdc.toNumber() + pending;
    };

    // Bob 申购 3000（之前没定价的几期也顺延进这次定价）；价格高于均线，每笔最多 1000 USDC：
    // 交易到这一期买进第一笔为止
    const bobBefore = await tokenBalance(bobUsdc);
    const epoch = await subscribe(bob, bobUsdc, 3_000 * USDC);
    await fixDay(56_000);
    const recordAddr = await recordFor(epoch);
    const record = () => program.account.epochRecord.fetch(recordAddr);
    const unfilled = async () => {
      const st = await state();
      const r = await record();
      return st.pending.slice(0, st.pendingCount).find((p) => p.epoch.eq(r.epoch))!.usdc.toNumber();
    };
    const start = await unfilled();
    while ((await unfilled()) === start) {
      await warpTo(now + TRADE_INTERVAL);
      expect(await trade()).to.equal(null);
    }

    // 待成交的算进上限：Alice 多 1 USDC 就被拒绝
    const room = SUBSCRIPTION_CAP * USDC - (await outstanding());
    expect(await errorName(subscribe(alice, aliceUsdc, room + 1))).to.equal("SubscriptionCapExceeded");

    // Bob 随时可以撤销待成交申购：按他占这一期申购额的比例，领走已成交的份额、退回没成交的 USDC
    const r = await record();
    const part = 3_000n * BigInt(USDC);
    const basis = BigInt(r.subscriptionUsdc.toString());
    const left = BigInt(await unfilled());
    const shares = (BigInt(r.mintedShares.toString()) * part) / basis;
    const refund = (left * part) / basis;
    expect(Number(shares)).to.be.greaterThan(0);
    const bobShares = getAssociatedTokenAddressSync(shareMint, bob.publicKey);
    const sharesBefore = await tokenBalance(bobShares);
    const escrowBefore = await tokenBalance(escrow);
    await cancelPending(bob, bobUsdc, epoch);
    expect(await tokenBalance(bobUsdc)).to.equal(bobBefore - part + refund);
    expect(await tokenBalance(escrow)).to.equal(escrowBefore - refund);
    expect(await tokenBalance(bobShares)).to.equal(sharesBefore + shares);
    expect(await context.banksClient.getAccount(ticketPda(bob.publicKey, epoch))).to.equal(null);
    const after = await record();
    expect(BigInt(after.cancelledSubscriptionUsdc.toString())).to.equal(part);

    // 腾出额度之后 Alice 的申购可以进来
    await warpTo(now + 1);
    await subscribe(alice, aliceUsdc, room + 1);
    expect(await outstanding()).to.be.at.most(SUBSCRIPTION_CAP * USDC);
  });

  it("BTC/USD 作信号：用 BTC/USD 定价和交易，cbBTC/USD 即使有也不用，不检查脱锚", async () => {
    // cbBTC/USD 比 BTC/USD 低 12%：用 BTC/USD 时不比较，照常定价、交易
    await fixDay(57_000, 50_000);
    const r = await program.account.epochRecord.fetch(epochPda(today));
    expect(r.signalPrice.toString()).to.equal(usd(57_000).toString());
    expect(r.referencePrice.toNumber()).to.equal(0);
    const [, why] = await tradeUntilDone();
    expect(why).to.equal("NothingToTrade");
  });
});
