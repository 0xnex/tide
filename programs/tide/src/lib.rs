//! BTC 50 日均线基金（Solana / Anchor，devnet 原型）
//!
//! - 任何人随时用 USDC 申购、用份额赎回，排队从不关闭。每个请求属于关闭它的定价时刻那一期
//!   （期号是那天的日期序号：01:00 之前提交的属于当天，之后的属于第二天）；
//!   每个用户每期一张排队记录。关闭这一期的定价时刻之前可以 cancel，全额退回
//! - 结算分多步（ADR 0001）。定价时刻后 keeper_window_secs 秒内只有清算地址（config.keeper）
//!   能驱动，之后任何人都可以，但其他人的交易用更严的滑点上限：
//!     fix：每天 UTC 1 点之后调用一次，记录 1 点的定价价格 -> 算 50 日均线和目标 ->
//!          销毁赎回份额并按比例划出实物赎回资产 -> 轧差，其余申购款成为待成交申购
//!     trade：之后反复调用，每次一笔不超过 trade_cap_usdc 的交易，间隔至少
//!          min_trade_interval_secs；先卖赎回资产，再把基金调向目标，最后为申购买入
//!          （目标为 USDC 时申购款不交易直接成交），按带来的资产发份额（ADR 0006）；
//!          没有可交易的时候返回 NothingToTrade / TradeTooSoon / DepegBlocksBuy
//!   信号和轧差只取决于 1 点发布的价格，和调用 fix 的时间无关；某天没有 fix 时，
//!   这一期的请求顺延到下一次 fix，一次 fix 可以关闭好几期
//! - 用户调用 claim 领取份额（可以分几次领取已成交的部分）；赎回人要等这一期的赎回资产卖完才能领取 USDC
//!
//! - 均线按日历日计算：某天没有定价时，任何人可以用那天定价窗口内发布的价格 backfill_price
//! - 治理：管理员（应为外部多签，比如 Squads）提议一份完整的新配置，48 小时之后才能生效；
//!   唯一立即生效的操作是暂停、恢复申购。赎回永远不能暂停，管理员也不能动基金资产
//!
//! 原型限制（上主网前必须替换）：
//! - 价格来自 MockOracle（管理员手动设置两个价格：BTC/USD 信号价格、cbBTC/USD 估值价格），
//!   主网应改为 Pyth 的两个价格源
//! - 兑换走 MockMarket（按预言机价格成交、收固定手续费），主网应改为 Jupiter
//!
//! 所有记账逻辑在 fund-logic crate 中（纯 Rust，有完整单元测试），本文件只负责
//! 账户校验和代币转账。

use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{self, Burn, Mint, MintTo, Token, TokenAccount, Transfer};
use fund_logic as logic;

declare_id!("3E8fFGuHJnrKDyPH7ZXdkffq4CKryaHovizg4yUH1jNs"); // anchor keys sync 会替换成你的程序地址

const _: () = assert!(logic::MAX_WINDOW == 64);
const _: () = assert!(logic::MAX_SLICES == 8);
const _: () = assert!(logic::MAX_PENDING == 8);

#[program]
pub mod tide {
    use super::*;

    // ------------------------------------------------------------------
    // 初始化
    // ------------------------------------------------------------------

    /// 第 1 步：创建基金账户和份额代币
    pub fn initialize(ctx: Context<Initialize>, config: ConfigData) -> Result<()> {
        config.to_logic().validate().map_err(map_err)?;
        check_program_config(&config)?;
        let fund = &mut ctx.accounts.fund;
        fund.admin = ctx.accounts.admin.key();
        fund.usdc_mint = ctx.accounts.usdc_mint.key();
        fund.btc_mint = ctx.accounts.btc_mint.key();
        fund.share_mint = ctx.accounts.share_mint.key();
        fund.oracle = ctx.accounts.oracle.key();
        fund.market = ctx.accounts.market.key();
        fund.config = config;
        fund.state = StateData::from_logic(&logic::FundState::new());
        fund.pending_config = None;
        fund.bump = ctx.bumps.fund;
        Ok(())
    }

    /// 第 2 步：创建基金的 5 个代币账户（分开做，避免单条指令账户过多）
    pub fn init_vaults(_ctx: Context<InitVaults>) -> Result<()> {
        Ok(())
    }

    /// 首次定价前，管理员写入最近几天的历史价格（到昨天为止，最早的在前），
    /// 让均线从第一天就能计算
    pub fn seed_prices(ctx: Context<AdminOnly>, prices: Vec<u64>) -> Result<()> {
        let fund = &mut ctx.accounts.fund;
        require!(fund.state.last_fix_day < 0, FundErrorCode::AlreadyStarted);
        let cfg = fund.config.to_logic();
        let mut st = fund.state.to_logic();
        let yesterday = logic::day_of(Clock::get()?.unix_timestamp) - 1;
        logic::seed_prices(&mut st, &prices, yesterday, &cfg).map_err(map_err)?;
        fund.state = StateData::from_logic(&st);
        Ok(())
    }

    // ------------------------------------------------------------------
    // 治理（只有管理员）
    // ------------------------------------------------------------------

    /// 提议一份完整的新配置，至少 48 小时之后才能 apply_config。
    /// 新的提议取代还没生效的旧提议、重新计时；要撤回就提议当前配置。
    pub fn propose_config(ctx: Context<AdminOnly>, config: ConfigData) -> Result<()> {
        check_program_config(&config)?;
        let now = Clock::get()?.unix_timestamp;
        let current = ctx.accounts.fund.config.to_logic();
        let effective_at =
            logic::propose_config(&current, &config.to_logic(), now).map_err(map_err)?;
        ctx.accounts.fund.pending_config = Some(PendingConfig {
            config,
            effective_at,
        });
        emit!(ConfigProposed { effective_at });
        Ok(())
    }

    /// 时间锁过了之后让提议的配置生效
    pub fn apply_config(ctx: Context<AdminOnly>) -> Result<()> {
        let fund = &mut ctx.accounts.fund;
        let pending = fund
            .pending_config
            .ok_or_else(|| error!(FundErrorCode::NoPendingConfig))?;
        let now = Clock::get()?.unix_timestamp;
        logic::check_timelock(pending.effective_at, now).map_err(map_err)?;
        fund.config = pending.config;
        fund.pending_config = None;
        emit!(ConfigApplied {});
        Ok(())
    }

    /// 立即暂停申购。已经排队和待成交的申购照常处理，赎回、结算和领取不受影响
    pub fn pause_subscriptions(ctx: Context<AdminOnly>) -> Result<()> {
        ctx.accounts.fund.state.subscriptions_paused = true;
        emit!(SubscriptionPauseChanged { paused: true });
        Ok(())
    }

    pub fn resume_subscriptions(ctx: Context<AdminOnly>) -> Result<()> {
        ctx.accounts.fund.state.subscriptions_paused = false;
        emit!(SubscriptionPauseChanged { paused: false });
        Ok(())
    }

    // ------------------------------------------------------------------
    // 申购、赎回、领取
    // ------------------------------------------------------------------

    /// epoch 是这一刻开放的那一期（fund_logic::request_epoch），只用来推出排队记录的地址
    pub fn request_subscription(
        ctx: Context<RequestSubscription>,
        amount: u64,
        epoch: u64,
    ) -> Result<()> {
        let fund = &mut ctx.accounts.fund;
        let ticket = &mut ctx.accounts.ticket;
        if ticket.owner == Pubkey::default() {
            ticket.fund = fund.key();
            ticket.owner = ctx.accounts.user.key();
            ticket.bump = ctx.bumps.ticket;
        }
        let cfg = fund.config.to_logic();
        let mut st = fund.state.to_logic();
        let mut t = ticket.to_logic();
        let now = Clock::get()?.unix_timestamp;
        logic::request_subscription(&mut st, &mut t, amount, now, &cfg).map_err(map_err)?;
        // 排队记录的地址由 epoch 推出，必须正是 fund-logic 定下的那一期
        require!(t.epoch == epoch, FundErrorCode::WrongEpoch);

        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.user_usdc.to_account_info(),
                    to: ctx.accounts.subscription_escrow.to_account_info(),
                    authority: ctx.accounts.user.to_account_info(),
                },
            ),
            amount,
        )?;
        fund.state = StateData::from_logic(&st);
        ticket.set_from_logic(&t);
        Ok(())
    }

    /// epoch 同 request_subscription
    pub fn request_redemption(
        ctx: Context<RequestRedemption>,
        shares: u64,
        epoch: u64,
    ) -> Result<()> {
        let fund = &mut ctx.accounts.fund;
        let ticket = &mut ctx.accounts.ticket;
        if ticket.owner == Pubkey::default() {
            ticket.fund = fund.key();
            ticket.owner = ctx.accounts.user.key();
            ticket.bump = ctx.bumps.ticket;
        }
        let cfg = fund.config.to_logic();
        let mut st = fund.state.to_logic();
        let mut t = ticket.to_logic();
        let now = Clock::get()?.unix_timestamp;
        logic::request_redemption(&mut st, &mut t, shares, now, &cfg).map_err(map_err)?;
        require!(t.epoch == epoch, FundErrorCode::WrongEpoch);

        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.user_shares.to_account_info(),
                    to: ctx.accounts.share_escrow.to_account_info(),
                    authority: ctx.accounts.user.to_account_info(),
                },
            ),
            shares,
        )?;
        fund.state = StateData::from_logic(&st);
        ticket.set_from_logic(&t);
        Ok(())
    }

    /// 关闭这一期的定价时刻之前撤销排队记录：申购款和赎回份额全额退回，记录关闭、退回租金
    pub fn cancel(ctx: Context<Cancel>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let fund = &mut ctx.accounts.fund;
        let cfg = fund.config.to_logic();
        let mut st = fund.state.to_logic();
        let mut t = ctx.accounts.ticket.to_logic();
        let logic::CancelOutcome { usdc, shares } =
            logic::cancel(&mut st, &mut t, now, &cfg).map_err(map_err)?;
        fund.state = StateData::from_logic(&st);

        let admin = fund.admin;
        let seeds: &[&[u8]] = &[b"fund", admin.as_ref(), &[fund.bump]];
        let signer = &[seeds];
        let tp = ctx.accounts.token_program.to_account_info();
        let fund_ai = ctx.accounts.fund.to_account_info();
        fund_transfer(
            &tp,
            &fund_ai,
            signer,
            ctx.accounts.subscription_escrow.to_account_info(),
            ctx.accounts.user_usdc.to_account_info(),
            usdc,
        )?;
        fund_transfer(
            &tp,
            &fund_ai,
            signer,
            ctx.accounts.share_escrow.to_account_info(),
            ctx.accounts.user_shares.to_account_info(),
            shares,
        )?;
        ctx.accounts
            .ticket
            .close(ctx.accounts.user.to_account_info())
    }

    /// epoch_record 传关闭这张排队记录那一期的定价记录：通常期号相同；
    /// 中间的定价被跳过时，是之后第一次定价的记录（first_epoch ≤ 排队记录的期号 ≤ epoch）。
    pub fn claim(ctx: Context<Claim>) -> Result<()> {
        let fund = &ctx.accounts.fund;
        let ticket = &mut ctx.accounts.ticket;
        let epoch = &mut ctx.accounts.epoch_record;
        let mut t = ticket.to_logic();
        let mut r = epoch.to_logic();
        let logic::ClaimOutcome {
            shares,
            payout_usdc,
            refund_usdc,
        } = logic::claim(&mut t, &mut r).map_err(map_err)?;
        epoch.set_unclaimed(&r);
        ticket.set_from_logic(&t);

        let admin = fund.admin;
        let seeds: &[&[u8]] = &[b"fund", admin.as_ref(), &[fund.bump]];
        let signer = &[seeds];
        if shares > 0 {
            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.share_escrow.to_account_info(),
                        to: ctx.accounts.user_shares.to_account_info(),
                        authority: ctx.accounts.fund.to_account_info(),
                    },
                    signer,
                ),
                shares,
            )?;
        }
        let tp = ctx.accounts.token_program.to_account_info();
        let fund_ai = ctx.accounts.fund.to_account_info();
        let user_usdc = ctx.accounts.user_usdc.to_account_info();
        // 赎回款从待领取账户，申购退款从申购托管
        fund_transfer(
            &tp,
            &fund_ai,
            signer,
            ctx.accounts.payout_vault.to_account_info(),
            user_usdc.clone(),
            payout_usdc,
        )?;
        fund_transfer(
            &tp,
            &fund_ai,
            signer,
            ctx.accounts.subscription_escrow.to_account_info(),
            user_usdc,
            refund_usdc,
        )?;
        // 都领完了：关闭排队记录，退回租金（每期一张，不关闭会越积越多）
        if t.is_empty() {
            ctx.accounts
                .ticket
                .close(ctx.accounts.user.to_account_info())?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // 结算：定价步骤 + 一系列交易（任何人都可以调用）
    // ------------------------------------------------------------------

    /// 定价步骤：每天定价时刻之后调用一次。epoch 是当天的日期序号（unix 秒 / 86400），
    /// 用来推出这次定价记录的地址
    pub fn fix(ctx: Context<Fix>, epoch: u64) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let fund_cfg = ctx.accounts.fund.config;
        // 模拟预言机每个价格源只有一个价格：它必须在定价窗口内发布，并且不超过
        // max_staleness_secs，所以目前只能在 1 点后几十秒内定价。
        // 换成 Pyth 后，定价价格用 1 点的历史价格更新。
        let prices = fresh_prices(&ctx.accounts.oracle, now, fund_cfg.max_staleness_secs)?;

        let cfg = fund_cfg.to_logic();
        let mut st = ctx.accounts.fund.state.to_logic();
        let caller = fund_cfg.caller(&ctx.accounts.cranker.key());
        let logic::FixOutcome { record, fee_usdc } =
            logic::fix(&mut st, now, &prices, caller, &cfg).map_err(map_err)?;
        require!(record.epoch == epoch, FundErrorCode::WrongEpochRecord);

        let admin = ctx.accounts.fund.admin;
        let fund_bump = ctx.accounts.fund.bump;
        let fund_seeds: &[&[u8]] = &[b"fund", admin.as_ref(), &[fund_bump]];
        let fund_signer = &[fund_seeds];
        let tp = ctx.accounts.token_program.to_account_info();
        let fund_ai = ctx.accounts.fund.to_account_info();
        let escrow = ctx.accounts.subscription_escrow.to_account_info();
        let vault = ctx.accounts.usdc_vault.to_account_info();
        let payout = ctx.accounts.payout_vault.to_account_info();
        // 1) 申购费从托管转入主账户；轧差的申购款转给赎回人；其余申购款留在托管里待成交
        fund_transfer(
            &tp,
            &fund_ai,
            fund_signer,
            escrow.clone(),
            vault.clone(),
            fee_usdc,
        )?;
        fund_transfer(
            &tp,
            &fund_ai,
            fund_signer,
            escrow,
            payout.clone(),
            record.netted_usdc,
        )?;
        // 2) 赎回资产里的 USDC 转入待领取账户；cbBTC 留在 BTC 账户里，之后卖出
        //    （轧差换给申购人的那部分 cbBTC 已经算进基金）
        fund_transfer(&tp, &fund_ai, fund_signer, vault, payout, record.slice_usdc)?;
        // 3) 销毁本期赎回的份额（用户申请赎回时已转入份额托管账户）
        if record.redemption_shares > 0 {
            token::burn(
                CpiContext::new_with_signer(
                    tp.clone(),
                    Burn {
                        mint: ctx.accounts.share_mint.to_account_info(),
                        from: ctx.accounts.share_escrow.to_account_info(),
                        authority: fund_ai.clone(),
                    },
                    fund_signer,
                ),
                record.redemption_shares,
            )?;
        }
        // 4) 为轧差的申购发行份额，放在份额托管账户，等用户领取
        if record.minted_shares > 0 {
            token::mint_to(
                CpiContext::new_with_signer(
                    tp.clone(),
                    MintTo {
                        mint: ctx.accounts.share_mint.to_account_info(),
                        to: ctx.accounts.share_escrow.to_account_info(),
                        authority: fund_ai.clone(),
                    },
                    fund_signer,
                ),
                record.minted_shares,
            )?;
        }

        let epoch_acc = &mut ctx.accounts.epoch_record;
        epoch_acc.fund = ctx.accounts.fund.key();
        epoch_acc.bump = ctx.bumps.epoch_record;
        epoch_acc.set_from_logic(&record);

        ctx.accounts.fund.state = StateData::from_logic(&st);
        emit!(Fixed {
            epoch: record.epoch,
            first_epoch: record.first_epoch,
            signal_price: record.signal_price,
            valuation_price: record.valuation_price,
            moving_average: record.moving_average,
            target_btc: record.target_btc,
        });
        Ok(())
    }

    /// 一笔交易（或一次不交易的申购成交）。epoch_record 传最早一期未卖完的赎回资产的记录
    /// （fund.state.slices[0].epoch）；没有赎回资产时传最早一期待成交申购的记录
    /// （fund.state.pending[0].epoch）；都没有时不传。基金调仓用不到它。
    pub fn trade(ctx: Context<Trade>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let fund_cfg = ctx.accounts.fund.config;
        let latest = fresh_prices(&ctx.accounts.oracle, now, fund_cfg.max_staleness_secs)?;
        let prices = logic::MarketPrices {
            signal: latest.signal.price,
            valuation: latest.valuation.price,
        };
        let cfg = fund_cfg.to_logic();
        let mut st = ctx.accounts.fund.state.to_logic();
        let caller = fund_cfg.caller(&ctx.accounts.cranker.key());
        let plan = logic::plan_trade(&st, now, &prices, caller, &cfg).map_err(map_err)?;

        let admin = ctx.accounts.fund.admin;
        let fund_bump = ctx.accounts.fund.bump;
        let fund_seeds: &[&[u8]] = &[b"fund", admin.as_ref(), &[fund_bump]];
        let fund_signer = &[fund_seeds];
        let tp = ctx.accounts.token_program.to_account_info();
        let fund_ai = ctx.accounts.fund.to_account_info();
        // 买入用的 USDC：申购的从申购托管出，基金的从主账户出。
        // 卖出所得：赎回资产的进待领取账户，基金的进主账户。
        let usdc_source = match plan.bucket {
            logic::TradeBucket::Subscription { .. } => {
                ctx.accounts.subscription_escrow.to_account_info()
            }
            _ => ctx.accounts.usdc_vault.to_account_info(),
        };
        let usdc_dest = match plan.bucket {
            logic::TradeBucket::Slice { .. } => ctx.accounts.payout_vault.to_account_info(),
            _ => ctx.accounts.usdc_vault.to_account_info(),
        };
        let got = match plan.side {
            // 目标为 USDC：申购款直接进入基金，不经过交易所
            logic::Side::WithoutTrade => {
                fund_transfer(
                    &tp,
                    &fund_ai,
                    fund_signer,
                    usdc_source,
                    ctx.accounts.usdc_vault.to_account_info(),
                    plan.amount_in,
                )?;
                0
            }
            side => {
                // 原型：模拟交易所按与 min_out 相同的最新估值价格成交并收手续费
                let market = &ctx.accounts.market;
                let m_auth = market.authority;
                let m_seeds: &[&[u8]] = &[b"market", m_auth.as_ref(), &[market.bump]];
                let (fund_from, mkt_to, mkt_from, fund_to) = if side == logic::Side::BuyBtc {
                    (
                        usdc_source,
                        ctx.accounts.market_usdc.to_account_info(),
                        ctx.accounts.market_btc.to_account_info(),
                        ctx.accounts.btc_vault.to_account_info(),
                    )
                } else {
                    (
                        ctx.accounts.btc_vault.to_account_info(),
                        ctx.accounts.market_btc.to_account_info(),
                        ctx.accounts.market_usdc.to_account_info(),
                        usdc_dest,
                    )
                };
                let got =
                    logic::market_fill(&plan, prices.valuation, market.fee_bps).map_err(map_err)?;
                fund_transfer(
                    &tp,
                    &fund_ai,
                    fund_signer,
                    fund_from,
                    mkt_to,
                    plan.amount_in,
                )?;
                token::transfer(
                    CpiContext::new_with_signer(
                        tp.clone(),
                        Transfer {
                            from: mkt_from,
                            to: fund_to,
                            authority: market.to_account_info(),
                        },
                        &[m_seeds],
                    ),
                    got,
                )?;
                got
            }
        };

        // 校验成交并记账（成交不足 min_out 会让整笔交易回滚）
        let mut record = ctx.accounts.epoch_record.as_ref().map(|acc| acc.to_logic());
        let minted =
            logic::fill_trade(&mut st, &plan, got, now, record.as_mut()).map_err(map_err)?;
        if let (Some(acc), Some(r)) = (ctx.accounts.epoch_record.as_mut(), record) {
            acc.set_from_logic(&r);
        }
        // 申购成交发行的份额放在份额托管账户，等申购人领取
        if minted > 0 {
            token::mint_to(
                CpiContext::new_with_signer(
                    tp.clone(),
                    MintTo {
                        mint: ctx.accounts.share_mint.to_account_info(),
                        to: ctx.accounts.share_escrow.to_account_info(),
                        authority: fund_ai.clone(),
                    },
                    fund_signer,
                ),
                minted,
            )?;
        }
        ctx.accounts.fund.state = StateData::from_logic(&st);

        emit!(Traded {
            side: match plan.side {
                logic::Side::BuyBtc => TradeSide::BuyBtc,
                logic::Side::SellBtc => TradeSide::SellBtc,
                logic::Side::WithoutTrade => TradeSide::WithoutTrade,
            },
            amount_in: plan.amount_in,
            amount_out: got,
            record_epoch: plan.bucket.record_epoch(),
            minted_shares: minted,
        });
        Ok(())
    }

    /// 回填没有定价的那一天（day，日期序号）的信号价格，任何人都可以调用。
    /// 预言机的信号价格必须发布在那一天的定价窗口内，所以不检查是否过旧
    /// （换成 Pyth 后传那一天的历史价格更新）。
    pub fn backfill_price(ctx: Context<BackfillPrice>, day: i64) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let oracle = &ctx.accounts.oracle;
        let signal = logic::PricePoint {
            price: oracle.signal_price,
            publish_time: oracle.signal_publish_time,
        };
        let fund = &mut ctx.accounts.fund;
        let cfg = fund.config.to_logic();
        let mut st = fund.state.to_logic();
        logic::backfill_price(&mut st, day, &signal, now, &cfg).map_err(map_err)?;
        fund.state = StateData::from_logic(&st);
        emit!(PriceBackfilled {
            day,
            signal_price: signal.price,
        });
        Ok(())
    }

    // ------------------------------------------------------------------
    // 原型专用：模拟预言机和模拟交易所（主网版本删除）
    // ------------------------------------------------------------------

    pub fn init_mock_oracle(
        ctx: Context<InitMockOracle>,
        signal_price: u64,
        valuation_price: u64,
    ) -> Result<()> {
        let o = &mut ctx.accounts.oracle;
        o.authority = ctx.accounts.authority.key();
        o.set(signal_price, valuation_price, Clock::get()?.unix_timestamp);
        Ok(())
    }

    pub fn set_mock_price(
        ctx: Context<SetMockPrice>,
        signal_price: u64,
        valuation_price: u64,
    ) -> Result<()> {
        let o = &mut ctx.accounts.oracle;
        o.set(signal_price, valuation_price, Clock::get()?.unix_timestamp);
        Ok(())
    }

    pub fn init_mock_market(ctx: Context<InitMockMarket>, fee_bps: u16) -> Result<()> {
        require!(fee_bps < 10_000, FundErrorCode::InvalidConfig);
        let m = &mut ctx.accounts.market;
        m.authority = ctx.accounts.authority.key();
        m.usdc_mint = ctx.accounts.usdc_mint.key();
        m.btc_mint = ctx.accounts.btc_mint.key();
        m.fee_bps = fee_bps;
        m.bump = ctx.bumps.market;
        Ok(())
    }

    /// 修改模拟交易所的手续费，用来模拟成交低于 min_out
    pub fn set_mock_market_fee(ctx: Context<SetMockMarketFee>, fee_bps: u16) -> Result<()> {
        require!(fee_bps < 10_000, FundErrorCode::InvalidConfig);
        ctx.accounts.market.fee_bps = fee_bps;
        Ok(())
    }
}

/// fund-logic 看不到的配置项：预言机价格最多多旧、清算地址
fn check_program_config(config: &ConfigData) -> Result<()> {
    require!(config.max_staleness_secs > 0, FundErrorCode::InvalidConfig);
    require!(
        config.keeper != Pubkey::default(),
        FundErrorCode::InvalidConfig
    );
    Ok(())
}

/// 由基金 PDA 签名的代币转账；数量为 0 时跳过
fn fund_transfer<'info>(
    token_program: &AccountInfo<'info>,
    fund: &AccountInfo<'info>,
    signer: &[&[&[u8]]],
    from: AccountInfo<'info>,
    to: AccountInfo<'info>,
    amount: u64,
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    token::transfer(
        CpiContext::new_with_signer(
            token_program.clone(),
            Transfer {
                from,
                to,
                authority: fund.clone(),
            },
            signer,
        ),
        amount,
    )
}

/// 读取模拟预言机的两个价格（信号、估值）及发布时间，检查非零且不超过 max_staleness_secs
fn fresh_prices(
    oracle: &MockOracle,
    now: i64,
    max_staleness_secs: i64,
) -> Result<logic::FixPrices> {
    let prices = logic::FixPrices {
        signal: logic::PricePoint {
            price: oracle.signal_price,
            publish_time: oracle.signal_publish_time,
        },
        valuation: logic::PricePoint {
            price: oracle.valuation_price,
            publish_time: oracle.valuation_publish_time,
        },
    };
    for p in [prices.signal, prices.valuation] {
        require!(p.price > 0, FundErrorCode::BadPrice);
        require!(
            now >= p.publish_time && now - p.publish_time <= max_staleness_secs,
            FundErrorCode::StalePrice
        );
    }
    Ok(prices)
}

// ----------------------------------------------------------------------
// 账户数据
// ----------------------------------------------------------------------

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, Debug)]
pub struct ConfigData {
    pub window: u8,
    pub settle_offset_secs: i64,
    pub max_slippage_bps: u16,
    pub subscription_fee_bps: u16,
    pub redemption_fee_bps: u16,
    pub min_subscription: u64,
    pub depeg_threshold_bps: u16,
    pub trade_cap_usdc: u64,
    pub min_trade_interval_secs: i64,
    pub keeper_window_secs: i64,
    pub public_slippage_bps: u16,
    pub withdrawal_fee_bps: u16,
    pub max_staleness_secs: i64, // 预言机价格最多允许多旧
    pub keeper: Pubkey,          // 清算地址：清算窗口内唯一能驱动结算的签名人
}

impl ConfigData {
    fn to_logic(&self) -> logic::Config {
        logic::Config {
            window: self.window,
            settle_offset_secs: self.settle_offset_secs,
            max_slippage_bps: self.max_slippage_bps,
            subscription_fee_bps: self.subscription_fee_bps,
            redemption_fee_bps: self.redemption_fee_bps,
            min_subscription: self.min_subscription,
            depeg_threshold_bps: self.depeg_threshold_bps,
            trade_cap_usdc: self.trade_cap_usdc,
            min_trade_interval_secs: self.min_trade_interval_secs,
            keeper_window_secs: self.keeper_window_secs,
            public_slippage_bps: self.public_slippage_bps,
            withdrawal_fee_bps: self.withdrawal_fee_bps,
        }
    }

    /// 签名人是不是清算地址
    fn caller(&self, signer: &Pubkey) -> logic::Caller {
        if *signer == self.keeper {
            logic::Caller::Keeper
        } else {
            logic::Caller::Public
        }
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, Debug, Default)]
pub struct SliceData {
    pub epoch: u64,
    pub btc: u64,
    pub usdc: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, Debug, Default)]
pub struct PendingData {
    pub epoch: u64,
    pub usdc: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, Debug)]
pub struct StateData {
    pub usdc: u64,
    pub btc: u64,
    pub share_supply: u64,
    pub queued_subscription_usdc: u64,
    pub queued_redemption_shares: u64,
    pub next_epoch: u64,
    pub next_subscription_usdc: u64,
    pub next_redemption_shares: u64,
    pub prices: [u64; 64], // 必须等于 fund_logic::MAX_WINDOW（IDL 需要字面量长度）
    pub price_days: [i64; 64],
    pub target_btc: bool,
    pub last_fix_day: i64,
    pub last_trade_time: i64,
    pub slices: [SliceData; 8], // 必须等于 fund_logic::MAX_SLICES
    pub slice_count: u8,
    pub pending: [PendingData; 8], // 必须等于 fund_logic::MAX_PENDING
    pub pending_count: u8,
    pub subscriptions_paused: bool,
}

impl StateData {
    fn from_logic(s: &logic::FundState) -> Self {
        StateData {
            usdc: s.usdc,
            btc: s.btc,
            share_supply: s.share_supply,
            queued_subscription_usdc: s.queued_subscription_usdc,
            queued_redemption_shares: s.queued_redemption_shares,
            next_epoch: s.next_epoch,
            next_subscription_usdc: s.next_subscription_usdc,
            next_redemption_shares: s.next_redemption_shares,
            prices: s.prices,
            price_days: s.price_days,
            target_btc: s.target_btc,
            last_fix_day: s.last_fix_day,
            last_trade_time: s.last_trade_time,
            slices: s.slices.map(|x| SliceData {
                epoch: x.epoch,
                btc: x.btc,
                usdc: x.usdc,
            }),
            slice_count: s.slice_count,
            pending: s.pending.map(|x| PendingData {
                epoch: x.epoch,
                usdc: x.usdc,
            }),
            pending_count: s.pending_count,
            subscriptions_paused: s.subscriptions_paused,
        }
    }
    fn to_logic(&self) -> logic::FundState {
        logic::FundState {
            usdc: self.usdc,
            btc: self.btc,
            share_supply: self.share_supply,
            queued_subscription_usdc: self.queued_subscription_usdc,
            queued_redemption_shares: self.queued_redemption_shares,
            next_epoch: self.next_epoch,
            next_subscription_usdc: self.next_subscription_usdc,
            next_redemption_shares: self.next_redemption_shares,
            prices: self.prices,
            price_days: self.price_days,
            target_btc: self.target_btc,
            last_fix_day: self.last_fix_day,
            last_trade_time: self.last_trade_time,
            slices: self.slices.map(|x| logic::Slice {
                epoch: x.epoch,
                btc: x.btc,
                usdc: x.usdc,
            }),
            slice_count: self.slice_count,
            pending: self.pending.map(|x| logic::Pending {
                epoch: x.epoch,
                usdc: x.usdc,
            }),
            pending_count: self.pending_count,
            subscriptions_paused: self.subscriptions_paused,
        }
    }
}

/// 还没生效的配置提议
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, Debug)]
pub struct PendingConfig {
    pub config: ConfigData,
    pub effective_at: i64, // 最早生效时刻（提议时刻 + 48 小时）
}

#[account]
#[derive(InitSpace)]
pub struct Fund {
    pub admin: Pubkey,
    pub usdc_mint: Pubkey,
    pub btc_mint: Pubkey,
    pub share_mint: Pubkey,
    pub oracle: Pubkey,
    pub market: Pubkey,
    pub config: ConfigData,
    pub state: StateData,
    pub bump: u8,
    // 放在最后：Option 的长度会变，放前面会让 state 的偏移随它变化
    pub pending_config: Option<PendingConfig>,
}

#[account]
#[derive(InitSpace)]
pub struct Ticket {
    pub fund: Pubkey,
    pub owner: Pubkey,
    pub epoch: u64,
    pub subscription_usdc: u64,
    pub redemption_shares: u64,
    pub shares_claimed: u64,
    pub bump: u8,
}

impl Ticket {
    fn to_logic(&self) -> logic::Ticket {
        logic::Ticket {
            epoch: self.epoch,
            subscription_usdc: self.subscription_usdc,
            redemption_shares: self.redemption_shares,
            shares_claimed: self.shares_claimed,
        }
    }
    fn set_from_logic(&mut self, t: &logic::Ticket) {
        self.epoch = t.epoch;
        self.subscription_usdc = t.subscription_usdc;
        self.redemption_shares = t.redemption_shares;
        self.shares_claimed = t.shares_claimed;
    }
}

#[account]
#[derive(InitSpace)]
pub struct EpochRecord {
    pub fund: Pubkey,
    pub epoch: u64,
    pub first_epoch: u64,
    pub signal_price: u64,
    pub valuation_price: u64,
    pub moving_average: u64,
    pub subscription_usdc: u64,
    pub minted_shares: u64,
    pub subscription_filled: bool,
    pub netted_usdc: u64,
    pub netted_btc: u64,
    pub refund_usdc: u64,
    pub redemption_shares: u64,
    pub slice_usdc: u64,
    pub slice_btc: u64,
    pub payout_usdc: u64,
    pub slice_sold_out: bool,
    pub target_btc: bool,
    pub unclaimed_subscription_usdc: u64,
    pub unclaimed_shares: u64,
    pub unclaimed_redemption_shares: u64,
    pub unclaimed_payout_usdc: u64,
    pub unclaimed_refund_usdc: u64,
    pub bump: u8,
}

impl EpochRecord {
    fn set_from_logic(&mut self, r: &logic::EpochRecord) {
        self.epoch = r.epoch;
        self.first_epoch = r.first_epoch;
        self.signal_price = r.signal_price;
        self.valuation_price = r.valuation_price;
        self.moving_average = r.moving_average;
        self.subscription_usdc = r.subscription_usdc;
        self.minted_shares = r.minted_shares;
        self.subscription_filled = r.subscription_filled;
        self.netted_usdc = r.netted_usdc;
        self.netted_btc = r.netted_btc;
        self.refund_usdc = r.refund_usdc;
        self.redemption_shares = r.redemption_shares;
        self.slice_usdc = r.slice_usdc;
        self.slice_btc = r.slice_btc;
        self.payout_usdc = r.payout_usdc;
        self.slice_sold_out = r.slice_sold_out;
        self.target_btc = r.target_btc;
        self.set_unclaimed(r);
    }
    fn set_unclaimed(&mut self, r: &logic::EpochRecord) {
        self.unclaimed_subscription_usdc = r.unclaimed_subscription_usdc;
        self.unclaimed_shares = r.unclaimed_shares;
        self.unclaimed_redemption_shares = r.unclaimed_redemption_shares;
        self.unclaimed_payout_usdc = r.unclaimed_payout_usdc;
        self.unclaimed_refund_usdc = r.unclaimed_refund_usdc;
    }
    fn to_logic(&self) -> logic::EpochRecord {
        logic::EpochRecord {
            epoch: self.epoch,
            first_epoch: self.first_epoch,
            signal_price: self.signal_price,
            valuation_price: self.valuation_price,
            moving_average: self.moving_average,
            subscription_usdc: self.subscription_usdc,
            minted_shares: self.minted_shares,
            subscription_filled: self.subscription_filled,
            netted_usdc: self.netted_usdc,
            netted_btc: self.netted_btc,
            refund_usdc: self.refund_usdc,
            redemption_shares: self.redemption_shares,
            slice_usdc: self.slice_usdc,
            slice_btc: self.slice_btc,
            payout_usdc: self.payout_usdc,
            slice_sold_out: self.slice_sold_out,
            target_btc: self.target_btc,
            unclaimed_subscription_usdc: self.unclaimed_subscription_usdc,
            unclaimed_shares: self.unclaimed_shares,
            unclaimed_redemption_shares: self.unclaimed_redemption_shares,
            unclaimed_payout_usdc: self.unclaimed_payout_usdc,
            unclaimed_refund_usdc: self.unclaimed_refund_usdc,
        }
    }
}

/// 原型专用：模拟预言机，两个价格源，单位为“每枚 BTC 的微美元”（USD × 1e6）
#[account]
#[derive(InitSpace)]
pub struct MockOracle {
    pub authority: Pubkey,
    pub signal_price: u64, // BTC/USD
    pub signal_publish_time: i64,
    pub valuation_price: u64, // cbBTC/USD
    pub valuation_publish_time: i64,
}

impl MockOracle {
    fn set(&mut self, signal_price: u64, valuation_price: u64, now: i64) {
        self.signal_price = signal_price;
        self.signal_publish_time = now;
        self.valuation_price = valuation_price;
        self.valuation_publish_time = now;
    }
}

/// 原型专用：模拟交易所，按预言机价格成交任意数量（以注入的流动性为限），收 fee_bps 手续费
#[account]
#[derive(InitSpace)]
pub struct MockMarket {
    pub authority: Pubkey,
    pub usdc_mint: Pubkey,
    pub btc_mint: Pubkey,
    pub fee_bps: u16,
    pub bump: u8,
}

#[event]
pub struct Fixed {
    pub epoch: u64,       // 定价当天的日期序号
    pub first_epoch: u64, // 这次定价关闭的最早一期（中间的定价被跳过时更早）
    pub signal_price: u64,
    pub valuation_price: u64,
    pub moving_average: u64,
    pub target_btc: bool,
}

#[event]
pub struct PriceBackfilled {
    pub day: i64,
    pub signal_price: u64,
}

#[event]
pub struct ConfigProposed {
    pub effective_at: i64,
}

#[event]
pub struct ConfigApplied {}

#[event]
pub struct SubscriptionPauseChanged {
    pub paused: bool,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub enum TradeSide {
    BuyBtc,
    SellBtc,
    WithoutTrade, // 目标为 USDC 时申购款直接进入基金
}

#[event]
pub struct Traded {
    pub side: TradeSide,
    pub amount_in: u64,
    pub amount_out: u64,
    pub record_epoch: Option<u64>, // 服务哪一期的赎回资产或申购；基金调仓为 None
    pub minted_shares: u64,        // 申购成交发行的份额
}

// ----------------------------------------------------------------------
// 指令账户
// ----------------------------------------------------------------------

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(
        init, payer = admin, space = 8 + Fund::INIT_SPACE,
        seeds = [b"fund", admin.key().as_ref()], bump
    )]
    pub fund: Box<Account<'info, Fund>>,
    #[account(constraint = usdc_mint.decimals == 6 @ FundErrorCode::WrongDecimals)]
    pub usdc_mint: Box<Account<'info, Mint>>,
    #[account(constraint = btc_mint.decimals == 8 @ FundErrorCode::WrongDecimals)]
    pub btc_mint: Box<Account<'info, Mint>>,
    #[account(
        init, payer = admin,
        seeds = [b"share_mint", fund.key().as_ref()], bump,
        mint::decimals = 6, mint::authority = fund
    )]
    pub share_mint: Box<Account<'info, Mint>>,
    pub oracle: Box<Account<'info, MockOracle>>,
    #[account(
        constraint = market.usdc_mint == usdc_mint.key() @ FundErrorCode::WrongMint,
        constraint = market.btc_mint == btc_mint.key() @ FundErrorCode::WrongMint
    )]
    pub market: Box<Account<'info, MockMarket>>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct InitVaults<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(seeds = [b"fund", admin.key().as_ref()], bump = fund.bump, has_one = admin,
              has_one = usdc_mint, has_one = btc_mint, has_one = share_mint)]
    pub fund: Box<Account<'info, Fund>>,
    pub usdc_mint: Box<Account<'info, Mint>>,
    pub btc_mint: Box<Account<'info, Mint>>,
    pub share_mint: Box<Account<'info, Mint>>,
    #[account(init, payer = admin, seeds = [b"usdc_vault", fund.key().as_ref()], bump,
              token::mint = usdc_mint, token::authority = fund)]
    pub usdc_vault: Box<Account<'info, TokenAccount>>,
    #[account(init, payer = admin, seeds = [b"btc_vault", fund.key().as_ref()], bump,
              token::mint = btc_mint, token::authority = fund)]
    pub btc_vault: Box<Account<'info, TokenAccount>>,
    #[account(init, payer = admin, seeds = [b"subscription_escrow", fund.key().as_ref()], bump,
              token::mint = usdc_mint, token::authority = fund)]
    pub subscription_escrow: Box<Account<'info, TokenAccount>>,
    #[account(init, payer = admin, seeds = [b"payout_vault", fund.key().as_ref()], bump,
              token::mint = usdc_mint, token::authority = fund)]
    pub payout_vault: Box<Account<'info, TokenAccount>>,
    #[account(init, payer = admin, seeds = [b"share_escrow", fund.key().as_ref()], bump,
              token::mint = share_mint, token::authority = fund)]
    pub share_escrow: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct AdminOnly<'info> {
    pub admin: Signer<'info>,
    #[account(mut, seeds = [b"fund", admin.key().as_ref()], bump = fund.bump, has_one = admin)]
    pub fund: Box<Account<'info, Fund>>,
}

#[derive(Accounts)]
#[instruction(amount: u64, epoch: u64)]
pub struct RequestSubscription<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, seeds = [b"fund", fund.admin.as_ref()], bump = fund.bump)]
    pub fund: Box<Account<'info, Fund>>,
    #[account(
        init_if_needed, payer = user, space = 8 + Ticket::INIT_SPACE,
        seeds = [b"ticket", fund.key().as_ref(), user.key().as_ref(), &epoch.to_le_bytes()], bump
    )]
    pub ticket: Box<Account<'info, Ticket>>,
    #[account(mut, token::mint = fund.usdc_mint, token::authority = user)]
    pub user_usdc: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [b"subscription_escrow", fund.key().as_ref()], bump)]
    pub subscription_escrow: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(shares: u64, epoch: u64)]
pub struct RequestRedemption<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, seeds = [b"fund", fund.admin.as_ref()], bump = fund.bump)]
    pub fund: Box<Account<'info, Fund>>,
    #[account(
        init_if_needed, payer = user, space = 8 + Ticket::INIT_SPACE,
        seeds = [b"ticket", fund.key().as_ref(), user.key().as_ref(), &epoch.to_le_bytes()], bump
    )]
    pub ticket: Box<Account<'info, Ticket>>,
    #[account(mut, token::mint = fund.share_mint, token::authority = user)]
    pub user_shares: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [b"share_escrow", fund.key().as_ref()], bump)]
    pub share_escrow: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Cancel<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, seeds = [b"fund", fund.admin.as_ref()], bump = fund.bump)]
    pub fund: Box<Account<'info, Fund>>,
    #[account(
        mut,
        seeds = [b"ticket", fund.key().as_ref(), user.key().as_ref(), &ticket.epoch.to_le_bytes()],
        bump = ticket.bump,
        constraint = ticket.owner == user.key() @ FundErrorCode::NotOwner
    )]
    pub ticket: Box<Account<'info, Ticket>>,
    #[account(address = fund.share_mint)]
    pub share_mint: Box<Account<'info, Mint>>,
    #[account(
        init_if_needed, payer = user,
        associated_token::mint = share_mint, associated_token::authority = user
    )]
    pub user_shares: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = fund.usdc_mint, token::authority = user)]
    pub user_usdc: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [b"subscription_escrow", fund.key().as_ref()], bump)]
    pub subscription_escrow: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [b"share_escrow", fund.key().as_ref()], bump)]
    pub share_escrow: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Claim<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(seeds = [b"fund", fund.admin.as_ref()], bump = fund.bump)]
    pub fund: Box<Account<'info, Fund>>,
    #[account(
        mut,
        seeds = [b"ticket", fund.key().as_ref(), user.key().as_ref(), &ticket.epoch.to_le_bytes()],
        bump = ticket.bump,
        constraint = ticket.owner == user.key() @ FundErrorCode::NotOwner
    )]
    pub ticket: Box<Account<'info, Ticket>>,
    /// 关闭这张排队记录那一期的定价记录；期号范围由 fund-logic 校验，这里校验它属于这个基金
    #[account(mut, has_one = fund @ FundErrorCode::WrongEpochRecord)]
    pub epoch_record: Box<Account<'info, EpochRecord>>,
    #[account(address = fund.share_mint)]
    pub share_mint: Box<Account<'info, Mint>>,
    #[account(address = fund.usdc_mint)]
    pub usdc_mint: Box<Account<'info, Mint>>,
    #[account(
        init_if_needed, payer = user,
        associated_token::mint = share_mint, associated_token::authority = user
    )]
    pub user_shares: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::mint = fund.usdc_mint, token::authority = user)]
    pub user_usdc: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [b"share_escrow", fund.key().as_ref()], bump)]
    pub share_escrow: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [b"payout_vault", fund.key().as_ref()], bump)]
    pub payout_vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [b"subscription_escrow", fund.key().as_ref()], bump)]
    pub subscription_escrow: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(epoch: u64)]
pub struct Fix<'info> {
    /// 清算窗口内只能是清算地址，之后任何人都可以；支付本期记录账户的租金
    #[account(mut)]
    pub cranker: Signer<'info>,
    #[account(mut, seeds = [b"fund", fund.admin.as_ref()], bump = fund.bump,
              has_one = oracle, has_one = share_mint)]
    pub fund: Box<Account<'info, Fund>>,
    /// 当天的定价记录。已经存在只可能是今天定价过了：这时由 fund-logic 回答 AlreadySettled，
    /// 不会改写它（所以用 init_if_needed，而不是让创建账户失败）
    #[account(
        init_if_needed, payer = cranker, space = 8 + EpochRecord::INIT_SPACE,
        seeds = [b"epoch", fund.key().as_ref(), &epoch.to_le_bytes()], bump
    )]
    pub epoch_record: Box<Account<'info, EpochRecord>>,
    pub oracle: Box<Account<'info, MockOracle>>,
    #[account(mut)]
    pub share_mint: Box<Account<'info, Mint>>,
    #[account(mut, seeds = [b"usdc_vault", fund.key().as_ref()], bump)]
    pub usdc_vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [b"subscription_escrow", fund.key().as_ref()], bump)]
    pub subscription_escrow: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [b"share_escrow", fund.key().as_ref()], bump)]
    pub share_escrow: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [b"payout_vault", fund.key().as_ref()], bump)]
    pub payout_vault: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Trade<'info> {
    /// 清算窗口内只能是清算地址，之后任何人都可以（滑点上限更严）
    pub cranker: Signer<'info>,
    #[account(mut, seeds = [b"fund", fund.admin.as_ref()], bump = fund.bump,
              has_one = oracle, has_one = market)]
    pub fund: Box<Account<'info, Fund>>,
    pub oracle: Box<Account<'info, MockOracle>>,
    #[account(seeds = [b"market", market.authority.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, MockMarket>>,
    #[account(mut, seeds = [b"market_usdc", market.key().as_ref()], bump)]
    pub market_usdc: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [b"market_btc", market.key().as_ref()], bump)]
    pub market_btc: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [b"usdc_vault", fund.key().as_ref()], bump)]
    pub usdc_vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [b"btc_vault", fund.key().as_ref()], bump)]
    pub btc_vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [b"payout_vault", fund.key().as_ref()], bump)]
    pub payout_vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [b"subscription_escrow", fund.key().as_ref()], bump)]
    pub subscription_escrow: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [b"share_escrow", fund.key().as_ref()], bump)]
    pub share_escrow: Box<Account<'info, TokenAccount>>,
    #[account(mut, address = fund.share_mint)]
    pub share_mint: Box<Account<'info, Mint>>,
    /// 最早一期未卖完的赎回资产的记录，没有时传最早一期待成交申购的记录（见 trade）。
    /// 期号由 fund-logic 校验，这里校验它属于这个基金。
    #[account(mut, has_one = fund @ FundErrorCode::WrongEpochRecord)]
    pub epoch_record: Option<Box<Account<'info, EpochRecord>>>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct BackfillPrice<'info> {
    pub caller: Signer<'info>,
    #[account(mut, seeds = [b"fund", fund.admin.as_ref()], bump = fund.bump, has_one = oracle)]
    pub fund: Box<Account<'info, Fund>>,
    pub oracle: Box<Account<'info, MockOracle>>,
}

#[derive(Accounts)]
pub struct InitMockOracle<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(init, payer = authority, space = 8 + MockOracle::INIT_SPACE,
              seeds = [b"oracle", authority.key().as_ref()], bump)]
    pub oracle: Box<Account<'info, MockOracle>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetMockPrice<'info> {
    pub authority: Signer<'info>,
    #[account(mut, has_one = authority)]
    pub oracle: Box<Account<'info, MockOracle>>,
}

#[derive(Accounts)]
pub struct InitMockMarket<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(init, payer = authority, space = 8 + MockMarket::INIT_SPACE,
              seeds = [b"market", authority.key().as_ref()], bump)]
    pub market: Box<Account<'info, MockMarket>>,
    pub usdc_mint: Box<Account<'info, Mint>>,
    pub btc_mint: Box<Account<'info, Mint>>,
    #[account(init, payer = authority, seeds = [b"market_usdc", market.key().as_ref()], bump,
              token::mint = usdc_mint, token::authority = market)]
    pub market_usdc: Box<Account<'info, TokenAccount>>,
    #[account(init, payer = authority, seeds = [b"market_btc", market.key().as_ref()], bump,
              token::mint = btc_mint, token::authority = market)]
    pub market_btc: Box<Account<'info, TokenAccount>>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetMockMarketFee<'info> {
    pub authority: Signer<'info>,
    #[account(mut, seeds = [b"market", authority.key().as_ref()], bump = market.bump,
              has_one = authority)]
    pub market: Box<Account<'info, MockMarket>>,
}

// ----------------------------------------------------------------------
// 错误
// ----------------------------------------------------------------------

#[error_code]
pub enum FundErrorCode {
    #[msg("还没到今天的定价时刻（UTC 0 点 + 设定的偏移）")]
    TooEarly,
    #[msg("今天已经定价过")]
    AlreadySettled,
    #[msg("价格无效")]
    BadPrice,
    #[msg("预言机价格过旧")]
    StalePrice,
    #[msg("成交少于最低可接受数量")]
    SlippageExceeded,
    #[msg("配置无效")]
    InvalidConfig,
    #[msg("数值溢出")]
    Overflow,
    #[msg("没有可领取的内容")]
    NothingToClaim,
    #[msg("这一期还没定价")]
    EpochNotSettled,
    #[msg("低于最低申购金额")]
    BelowMinSubscription,
    #[msg("数量不能为零")]
    ZeroAmount,
    #[msg("历史价格数量超过均线窗口")]
    TooManyPrices,
    #[msg("基金已经开始运行，不能再写入历史价格")]
    AlreadyStarted,
    #[msg("代币精度不对：USDC 应为 6 位，BTC 应为 8 位")]
    WrongDecimals,
    #[msg("代币不匹配")]
    WrongMint,
    #[msg("不是这张排队记录的所有者")]
    NotOwner,
    #[msg("定价价格的发布时间不在定价窗口内")]
    PriceNotAtFixTime,
    #[msg("没有需要交易的东西")]
    NothingToTrade,
    #[msg("距离上一笔交易不到最小间隔")]
    TradeTooSoon,
    #[msg("cbBTC 脱锚，暂停买入")]
    DepegBlocksBuy,
    #[msg("这一期的赎回资产还没卖完")]
    SliceNotSold,
    #[msg("传入的期记录与这一步不符")]
    WrongEpochRecord,
    #[msg("未卖完的赎回资产太多，先卖出再定价")]
    TooManySlices,
    #[msg("已过今天的定价时刻、还没定价，基金调仓等待定价")]
    FixPending,
    #[msg("清算窗口内只有清算地址能驱动结算")]
    KeeperOnly,
    #[msg("排队记录不是这一刻开放的那一期")]
    WrongEpoch,
    #[msg("已过关闭这一期的定价时刻，不能再撤销")]
    TooLateToCancel,
    #[msg("排队记录是空的，没有可撤销的")]
    NothingToCancel,
    #[msg("申购已暂停（赎回不受影响）")]
    SubscriptionsPaused,
    #[msg("配置提议还没过 48 小时时间锁")]
    TimelockNotElapsed,
    #[msg("没有待生效的配置提议")]
    NoPendingConfig,
    #[msg("定价时刻和均线窗口只能在创建时设定")]
    ConfigFieldFixed,
    #[msg("这一天已经有信号价格")]
    PriceAlreadyRecorded,
    #[msg("只能回填过去、最近 64 天内的一天")]
    BackfillOutOfRange,
}

fn map_err(e: logic::FundError) -> Error {
    use logic::FundError as E;
    let code = match e {
        E::TooEarly => FundErrorCode::TooEarly,
        E::AlreadySettled => FundErrorCode::AlreadySettled,
        E::BadPrice => FundErrorCode::BadPrice,
        E::SlippageExceeded => FundErrorCode::SlippageExceeded,
        E::InvalidConfig => FundErrorCode::InvalidConfig,
        E::Overflow => FundErrorCode::Overflow,
        E::NothingToClaim => FundErrorCode::NothingToClaim,
        E::EpochNotSettled => FundErrorCode::EpochNotSettled,
        E::BelowMinSubscription => FundErrorCode::BelowMinSubscription,
        E::ZeroAmount => FundErrorCode::ZeroAmount,
        E::TooManyPrices => FundErrorCode::TooManyPrices,
        E::PriceNotAtFixTime => FundErrorCode::PriceNotAtFixTime,
        E::NothingToTrade => FundErrorCode::NothingToTrade,
        E::TradeTooSoon => FundErrorCode::TradeTooSoon,
        E::DepegBlocksBuy => FundErrorCode::DepegBlocksBuy,
        E::SliceNotSold => FundErrorCode::SliceNotSold,
        E::WrongEpochRecord => FundErrorCode::WrongEpochRecord,
        E::TooManySlices => FundErrorCode::TooManySlices,
        E::FixPending => FundErrorCode::FixPending,
        E::KeeperOnly => FundErrorCode::KeeperOnly,
        E::WrongEpoch => FundErrorCode::WrongEpoch,
        E::TooLateToCancel => FundErrorCode::TooLateToCancel,
        E::NothingToCancel => FundErrorCode::NothingToCancel,
        E::SubscriptionsPaused => FundErrorCode::SubscriptionsPaused,
        E::TimelockNotElapsed => FundErrorCode::TimelockNotElapsed,
        E::ConfigFieldFixed => FundErrorCode::ConfigFieldFixed,
        E::PriceAlreadyRecorded => FundErrorCode::PriceAlreadyRecorded,
        E::BackfillOutOfRange => FundErrorCode::BackfillOutOfRange,
    };
    error!(code)
}
