//! BTC 50 日均线基金（Solana / Anchor，devnet 原型）
//!
//! - 任何人用 USDC 申购、用份额赎回；都先排队，在下一次定价时按定价净值成交
//! - 结算分多步（ADR 0001）。定价时刻后 keeper_window_secs 秒内只有清算地址（config.keeper）
//!   能驱动，之后任何人都可以，但其他人的交易用更严的滑点上限：
//!     fix：每天 UTC 1 点之后调用一次，记录 1 点的定价价格 -> 算 50 日均线和目标 ->
//!          按净值发行申购份额、销毁赎回份额并按比例划出赎回资产
//!     trade：之后反复调用，每次一笔不超过 trade_cap_usdc 的交易，间隔至少
//!          min_trade_interval_secs；先卖赎回资产，再把基金调向目标；
//!          没有可交易的时候返回 NothingToTrade / TradeTooSoon / DepegBlocksBuy
//!   净值只取决于 1 点发布的价格，和调用 fix 的时间无关；1 点到当天 fix 之间暂停申购赎回
//! - 用户在定价之后调用 claim 领取份额；赎回人要等这一期的赎回资产卖完才能领取 USDC
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

#[program]
pub mod tide {
    use super::*;

    // ------------------------------------------------------------------
    // 初始化
    // ------------------------------------------------------------------

    /// 第 1 步：创建基金账户和份额代币
    pub fn initialize(ctx: Context<Initialize>, config: ConfigData) -> Result<()> {
        config.to_logic().validate().map_err(map_err)?;
        require!(config.max_staleness_secs > 0, FundErrorCode::InvalidConfig);
        require!(
            config.keeper != Pubkey::default(),
            FundErrorCode::InvalidConfig
        );
        let fund = &mut ctx.accounts.fund;
        fund.admin = ctx.accounts.admin.key();
        fund.usdc_mint = ctx.accounts.usdc_mint.key();
        fund.btc_mint = ctx.accounts.btc_mint.key();
        fund.share_mint = ctx.accounts.share_mint.key();
        fund.oracle = ctx.accounts.oracle.key();
        fund.market = ctx.accounts.market.key();
        fund.config = config;
        fund.state = StateData::from_logic(&logic::FundState::new());
        fund.bump = ctx.bumps.fund;
        Ok(())
    }

    /// 第 2 步：创建基金的 5 个代币账户（分开做，避免单条指令账户过多）
    pub fn init_vaults(_ctx: Context<InitVaults>) -> Result<()> {
        Ok(())
    }

    /// 首次定价前，管理员写入最近 49 天的历史价格，让均线从第一天就能计算
    pub fn seed_prices(ctx: Context<AdminOnly>, prices: Vec<u64>) -> Result<()> {
        let fund = &mut ctx.accounts.fund;
        require!(fund.state.last_fix_day < 0, FundErrorCode::AlreadyStarted);
        let cfg = fund.config.to_logic();
        let mut st = fund.state.to_logic();
        logic::seed_prices(&mut st, &prices, &cfg).map_err(map_err)?;
        fund.state = StateData::from_logic(&st);
        Ok(())
    }

    // ------------------------------------------------------------------
    // 申购、赎回、领取
    // ------------------------------------------------------------------

    pub fn request_subscription(ctx: Context<RequestSubscription>, amount: u64) -> Result<()> {
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
        require!(
            t.is_empty() || t.epoch == st.epoch,
            FundErrorCode::ClaimFirst
        );
        let now = Clock::get()?.unix_timestamp;
        logic::request_subscription(&mut st, &mut t, amount, now, &cfg).map_err(map_err)?;

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

    pub fn request_redemption(ctx: Context<RequestRedemption>, shares: u64) -> Result<()> {
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
        require!(
            t.is_empty() || t.epoch == st.epoch,
            FundErrorCode::ClaimFirst
        );
        let now = Clock::get()?.unix_timestamp;
        logic::request_redemption(&mut st, &mut t, shares, now, &cfg).map_err(map_err)?;

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

    pub fn claim(ctx: Context<Claim>) -> Result<()> {
        let fund = &ctx.accounts.fund;
        let ticket = &mut ctx.accounts.ticket;
        let epoch = &mut ctx.accounts.epoch_record;
        let mut t = ticket.to_logic();
        let mut r = epoch.to_logic();
        let (shares, usdc) = logic::claim(&mut t, &mut r).map_err(map_err)?;
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
        if usdc > 0 {
            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.payout_vault.to_account_info(),
                        to: ctx.accounts.user_usdc.to_account_info(),
                        authority: ctx.accounts.fund.to_account_info(),
                    },
                    signer,
                ),
                usdc,
            )?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // 结算：定价步骤 + 一系列交易（任何人都可以调用）
    // ------------------------------------------------------------------

    /// 定价步骤：每天定价时刻之后调用一次
    pub fn fix(ctx: Context<Fix>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let fund_cfg = ctx.accounts.fund.config;
        // 模拟预言机每个价格源只有一个价格：它必须在定价窗口内发布，并且不超过
        // max_staleness_secs，所以目前只能在 1 点后几十秒内定价。
        // 换成 Pyth 后，定价价格用 1 点的历史价格更新。
        let prices = fresh_prices(&ctx.accounts.oracle, now, fund_cfg.max_staleness_secs)?;

        let cfg = fund_cfg.to_logic();
        let mut st = ctx.accounts.fund.state.to_logic();
        let caller = fund_cfg.caller(&ctx.accounts.cranker.key());
        let record = logic::fix(&mut st, now, &prices, caller, &cfg).map_err(map_err)?;

        let admin = ctx.accounts.fund.admin;
        let fund_bump = ctx.accounts.fund.bump;
        let fund_seeds: &[&[u8]] = &[b"fund", admin.as_ref(), &[fund_bump]];
        let fund_signer = &[fund_seeds];
        let tp = ctx.accounts.token_program.to_account_info();
        let fund_ai = ctx.accounts.fund.to_account_info();

        // 1) 申购款从托管账户转入主账户
        if record.subscription_usdc > 0 {
            token::transfer(
                CpiContext::new_with_signer(
                    tp.clone(),
                    Transfer {
                        from: ctx.accounts.subscription_escrow.to_account_info(),
                        to: ctx.accounts.usdc_vault.to_account_info(),
                        authority: fund_ai.clone(),
                    },
                    fund_signer,
                ),
                record.subscription_usdc,
            )?;
        }
        // 2) 销毁本期赎回的份额（用户申请赎回时已转入份额托管账户）
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
        // 3) 为本期申购发行新份额，放在份额托管账户，等用户领取
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
        // 4) 赎回资产里的 USDC 转入待领取账户；cbBTC 留在 BTC 账户里，由之后的交易卖出
        if record.slice_usdc > 0 {
            token::transfer(
                CpiContext::new_with_signer(
                    tp.clone(),
                    Transfer {
                        from: ctx.accounts.usdc_vault.to_account_info(),
                        to: ctx.accounts.payout_vault.to_account_info(),
                        authority: fund_ai.clone(),
                    },
                    fund_signer,
                ),
                record.slice_usdc,
            )?;
        }

        let epoch_acc = &mut ctx.accounts.epoch_record;
        epoch_acc.fund = ctx.accounts.fund.key();
        epoch_acc.bump = ctx.bumps.epoch_record;
        epoch_acc.set_from_logic(&record);

        ctx.accounts.fund.state = StateData::from_logic(&st);
        emit!(Fixed {
            epoch: record.epoch,
            day: record.day,
            signal_price: record.signal_price,
            valuation_price: record.valuation_price,
            moving_average: record.moving_average,
            nav: record.nav,
            target_btc: record.target_btc,
        });
        Ok(())
    }

    /// 一笔交易。卖出赎回资产时，slice_record 必须是 fund.state.slices[0] 那一期的记录；
    /// 否则传 None。
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

        // 原型：模拟交易所按与 min_out 相同的最新估值价格成交并收手续费
        let price = prices.valuation;
        let market = &ctx.accounts.market;
        let m_auth = market.authority;
        let m_bump = market.bump;
        let m_seeds: &[&[u8]] = &[b"market", m_auth.as_ref(), &[m_bump]];
        let m_signer = &[m_seeds];
        // 卖出赎回资产的所得直接进入待领取账户，不经过基金主账户
        let slice_epoch = plan.bucket.slice_epoch();
        let usdc_dest = match slice_epoch {
            Some(_) => ctx.accounts.payout_vault.to_account_info(),
            None => ctx.accounts.usdc_vault.to_account_info(),
        };
        let (fund_from, mkt_to, mkt_from, fund_to) = match plan.side {
            logic::Side::BuyBtc => (
                ctx.accounts.usdc_vault.to_account_info(),
                ctx.accounts.market_usdc.to_account_info(),
                ctx.accounts.market_btc.to_account_info(),
                ctx.accounts.btc_vault.to_account_info(),
            ),
            logic::Side::SellBtc => (
                ctx.accounts.btc_vault.to_account_info(),
                ctx.accounts.market_btc.to_account_info(),
                ctx.accounts.market_usdc.to_account_info(),
                usdc_dest,
            ),
        };
        let got = logic::market_fill(&plan, price, market.fee_bps).map_err(map_err)?;
        token::transfer(
            CpiContext::new_with_signer(
                tp.clone(),
                Transfer {
                    from: fund_from,
                    to: mkt_to,
                    authority: fund_ai.clone(),
                },
                fund_signer,
            ),
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
                m_signer,
            ),
            got,
        )?;

        // 校验成交并记账（成交不足 min_out 会让整笔交易回滚）
        let mut record = ctx.accounts.slice_record.as_ref().map(|acc| acc.to_logic());
        logic::fill_trade(&mut st, &plan, got, now, record.as_mut()).map_err(map_err)?;
        if let (Some(acc), Some(r)) = (ctx.accounts.slice_record.as_mut(), record) {
            acc.set_from_logic(&r);
        }
        ctx.accounts.fund.state = StateData::from_logic(&st);

        emit!(Traded {
            buy_btc: plan.side == logic::Side::BuyBtc,
            amount_in: plan.amount_in,
            amount_out: got,
            slice_epoch,
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

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, Debug)]
pub struct StateData {
    pub usdc: u64,
    pub btc: u64,
    pub share_supply: u64,
    pub queued_subscription_usdc: u64,
    pub queued_redemption_shares: u64,
    pub prices: [u64; 64], // 必须等于 fund_logic::MAX_WINDOW（IDL 需要字面量长度）
    pub price_count: u8,
    pub price_head: u8,
    pub target_btc: bool,
    pub last_fix_day: i64,
    pub last_trade_time: i64,
    pub epoch: u64,
    pub slices: [SliceData; 8], // 必须等于 fund_logic::MAX_SLICES
    pub slice_count: u8,
}

impl StateData {
    fn from_logic(s: &logic::FundState) -> Self {
        StateData {
            usdc: s.usdc,
            btc: s.btc,
            share_supply: s.share_supply,
            queued_subscription_usdc: s.queued_subscription_usdc,
            queued_redemption_shares: s.queued_redemption_shares,
            prices: s.prices,
            price_count: s.price_count,
            price_head: s.price_head,
            target_btc: s.target_btc,
            last_fix_day: s.last_fix_day,
            last_trade_time: s.last_trade_time,
            epoch: s.epoch,
            slices: s.slices.map(|x| SliceData {
                epoch: x.epoch,
                btc: x.btc,
                usdc: x.usdc,
            }),
            slice_count: s.slice_count,
        }
    }
    fn to_logic(&self) -> logic::FundState {
        logic::FundState {
            usdc: self.usdc,
            btc: self.btc,
            share_supply: self.share_supply,
            queued_subscription_usdc: self.queued_subscription_usdc,
            queued_redemption_shares: self.queued_redemption_shares,
            prices: self.prices,
            price_count: self.price_count,
            price_head: self.price_head,
            target_btc: self.target_btc,
            last_fix_day: self.last_fix_day,
            last_trade_time: self.last_trade_time,
            epoch: self.epoch,
            slices: self.slices.map(|x| logic::Slice {
                epoch: x.epoch,
                btc: x.btc,
                usdc: x.usdc,
            }),
            slice_count: self.slice_count,
        }
    }
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
}

#[account]
#[derive(InitSpace)]
pub struct Ticket {
    pub fund: Pubkey,
    pub owner: Pubkey,
    pub epoch: u64,
    pub subscription_usdc: u64,
    pub redemption_shares: u64,
    pub bump: u8,
}

impl Ticket {
    fn to_logic(&self) -> logic::Ticket {
        logic::Ticket {
            epoch: self.epoch,
            subscription_usdc: self.subscription_usdc,
            redemption_shares: self.redemption_shares,
        }
    }
    fn set_from_logic(&mut self, t: &logic::Ticket) {
        self.epoch = t.epoch;
        self.subscription_usdc = t.subscription_usdc;
        self.redemption_shares = t.redemption_shares;
    }
}

#[account]
#[derive(InitSpace)]
pub struct EpochRecord {
    pub fund: Pubkey,
    pub epoch: u64,
    pub day: i64,
    pub signal_price: u64,
    pub valuation_price: u64,
    pub moving_average: u64,
    pub nav: u128,
    pub subscription_usdc: u64,
    pub minted_shares: u64,
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
    pub bump: u8,
}

impl EpochRecord {
    fn set_from_logic(&mut self, r: &logic::EpochRecord) {
        self.epoch = r.epoch;
        self.day = r.day;
        self.signal_price = r.signal_price;
        self.valuation_price = r.valuation_price;
        self.moving_average = r.moving_average;
        self.nav = r.nav;
        self.subscription_usdc = r.subscription_usdc;
        self.minted_shares = r.minted_shares;
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
    }
    fn to_logic(&self) -> logic::EpochRecord {
        logic::EpochRecord {
            epoch: self.epoch,
            day: self.day,
            signal_price: self.signal_price,
            valuation_price: self.valuation_price,
            moving_average: self.moving_average,
            nav: self.nav,
            subscription_usdc: self.subscription_usdc,
            minted_shares: self.minted_shares,
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
    pub epoch: u64,
    pub day: i64,
    pub signal_price: u64,
    pub valuation_price: u64,
    pub moving_average: u64,
    pub nav: u128,
    pub target_btc: bool,
}

#[event]
pub struct Traded {
    pub buy_btc: bool,
    pub amount_in: u64,
    pub amount_out: u64,
    pub slice_epoch: Option<u64>, // 卖出的是哪一期的赎回资产；基金调仓为 None
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
pub struct RequestSubscription<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, seeds = [b"fund", fund.admin.as_ref()], bump = fund.bump)]
    pub fund: Box<Account<'info, Fund>>,
    #[account(
        init_if_needed, payer = user, space = 8 + Ticket::INIT_SPACE,
        seeds = [b"ticket", fund.key().as_ref(), user.key().as_ref()], bump
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
pub struct RequestRedemption<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(mut, seeds = [b"fund", fund.admin.as_ref()], bump = fund.bump)]
    pub fund: Box<Account<'info, Fund>>,
    #[account(
        init_if_needed, payer = user, space = 8 + Ticket::INIT_SPACE,
        seeds = [b"ticket", fund.key().as_ref(), user.key().as_ref()], bump
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
pub struct Claim<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(seeds = [b"fund", fund.admin.as_ref()], bump = fund.bump)]
    pub fund: Box<Account<'info, Fund>>,
    #[account(
        mut, seeds = [b"ticket", fund.key().as_ref(), user.key().as_ref()], bump = ticket.bump,
        constraint = ticket.owner == user.key() @ FundErrorCode::NotOwner
    )]
    pub ticket: Box<Account<'info, Ticket>>,
    #[account(
        mut, seeds = [b"epoch", fund.key().as_ref(), &ticket.epoch.to_le_bytes()], bump = epoch_record.bump
    )]
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
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Fix<'info> {
    /// 清算窗口内只能是清算地址，之后任何人都可以；支付本期记录账户的租金
    #[account(mut)]
    pub cranker: Signer<'info>,
    #[account(mut, seeds = [b"fund", fund.admin.as_ref()], bump = fund.bump,
              has_one = oracle, has_one = share_mint)]
    pub fund: Box<Account<'info, Fund>>,
    #[account(
        init, payer = cranker, space = 8 + EpochRecord::INIT_SPACE,
        seeds = [b"epoch", fund.key().as_ref(), &fund.state.epoch.to_le_bytes()], bump
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
    /// 最早一期未卖完的赎回资产的记录（fund.state.slices[0].epoch）；没有赎回资产时不传。
    /// 期号由 fund-logic 校验，这里校验它属于这个基金。
    #[account(mut, has_one = fund @ FundErrorCode::WrongSliceRecord)]
    pub slice_record: Option<Box<Account<'info, EpochRecord>>>,
    pub token_program: Program<'info, Token>,
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
    #[msg("基金有份额但资产为零")]
    FundWiped,
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
    #[msg("上一期还没领取，请先 claim")]
    ClaimFirst,
    #[msg("基金已经开始运行，不能再写入历史价格")]
    AlreadyStarted,
    #[msg("代币精度不对：USDC 应为 6 位，BTC 应为 8 位")]
    WrongDecimals,
    #[msg("代币不匹配")]
    WrongMint,
    #[msg("不是这张排队记录的所有者")]
    NotOwner,
    #[msg("已过今天的定价时刻、还没定价，暂停申购赎回")]
    QueueClosed,
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
    #[msg("赎回资产的期记录不对")]
    WrongSliceRecord,
    #[msg("未卖完的赎回资产太多，先卖出再定价")]
    TooManySlices,
    #[msg("已过今天的定价时刻、还没定价，基金调仓等待定价")]
    FixPending,
    #[msg("清算窗口内只有清算地址能驱动结算")]
    KeeperOnly,
}

fn map_err(e: logic::FundError) -> Error {
    use logic::FundError as E;
    let code = match e {
        E::TooEarly => FundErrorCode::TooEarly,
        E::AlreadySettled => FundErrorCode::AlreadySettled,
        E::BadPrice => FundErrorCode::BadPrice,
        E::FundWiped => FundErrorCode::FundWiped,
        E::SlippageExceeded => FundErrorCode::SlippageExceeded,
        E::InvalidConfig => FundErrorCode::InvalidConfig,
        E::Overflow => FundErrorCode::Overflow,
        E::NothingToClaim => FundErrorCode::NothingToClaim,
        E::EpochNotSettled => FundErrorCode::EpochNotSettled,
        E::BelowMinSubscription => FundErrorCode::BelowMinSubscription,
        E::ZeroAmount => FundErrorCode::ZeroAmount,
        E::TooManyPrices => FundErrorCode::TooManyPrices,
        E::QueueClosed => FundErrorCode::QueueClosed,
        E::PriceNotAtFixTime => FundErrorCode::PriceNotAtFixTime,
        E::NothingToTrade => FundErrorCode::NothingToTrade,
        E::TradeTooSoon => FundErrorCode::TradeTooSoon,
        E::DepegBlocksBuy => FundErrorCode::DepegBlocksBuy,
        E::SliceNotSold => FundErrorCode::SliceNotSold,
        E::WrongSliceRecord => FundErrorCode::WrongSliceRecord,
        E::TooManySlices => FundErrorCode::TooManySlices,
        E::FixPending => FundErrorCode::FixPending,
        E::KeeperOnly => FundErrorCode::KeeperOnly,
    };
    error!(code)
}
