//! BTC 50 日均线基金（Solana / Anchor，devnet 原型）
//!
//! - 任何人用 USDC 申购、用份额赎回；都先排队，在下一次每日结算时按结算净值成交
//! - 每天 UTC 1 点之后，任何人都可以调用 settle：
//!     记录 1 点的定价价格 -> 算 50 日均线 -> 按净值处理申购赎回 -> 高于均线持有 BTC，否则持有 USDC
//!   净值只取决于 1 点发布的价格，和调用 settle 的时间无关；1 点到当天结算之间暂停申购赎回
//! - 用户在结算之后调用 claim 领取份额或 USDC
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

    /// 首次结算前，管理员写入最近 49 天的历史价格，让均线从第一天就能计算
    pub fn seed_prices(ctx: Context<AdminOnly>, prices: Vec<u64>) -> Result<()> {
        let fund = &mut ctx.accounts.fund;
        require!(
            fund.state.last_settled_day < 0,
            FundErrorCode::AlreadyStarted
        );
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
    // 每日结算（任何人都可以调用）
    // ------------------------------------------------------------------

    pub fn settle(ctx: Context<Settle>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let oracle = &ctx.accounts.oracle;
        let fund_cfg = ctx.accounts.fund.config;
        for (price, publish_time) in [
            (oracle.signal_price, oracle.signal_publish_time),
            (oracle.valuation_price, oracle.valuation_publish_time),
        ] {
            require!(price > 0, FundErrorCode::BadPrice);
            require!(
                now >= publish_time && now - publish_time <= fund_cfg.max_staleness_secs,
                FundErrorCode::StalePrice
            );
        }
        // 模拟预言机每个价格源只有一个价格，同时充当定价价格和最新价格：它必须在定价窗口内发布，
        // 并且不超过 max_staleness_secs，所以目前只能在 1 点后几十秒内结算。
        // 换成 Pyth 后，定价价格用 1 点的历史价格更新，最新价格单独传入。
        let prices = logic::SettlePrices {
            signal_fix: logic::PricePoint {
                price: oracle.signal_price,
                publish_time: oracle.signal_publish_time,
            },
            valuation_fix: logic::PricePoint {
                price: oracle.valuation_price,
                publish_time: oracle.valuation_publish_time,
            },
            signal_exec: oracle.signal_price,
            valuation_exec: oracle.valuation_price,
        };
        // 交易所交易的是 cbBTC：模拟交易所按与 min_out 相同的最新估值价格成交
        let price = prices.valuation_exec;

        let cfg = fund_cfg.to_logic();
        let mut st = ctx.accounts.fund.state.to_logic();
        let mut out = logic::begin_settle(&mut st, now, &prices, &cfg).map_err(map_err)?;

        let admin = ctx.accounts.fund.admin;
        let fund_bump = ctx.accounts.fund.bump;
        let fund_seeds: &[&[u8]] = &[b"fund", admin.as_ref(), &[fund_bump]];
        let fund_signer = &[fund_seeds];
        let tp = ctx.accounts.token_program.to_account_info();
        let fund_ai = ctx.accounts.fund.to_account_info();

        // 1) 申购款从托管账户转入主账户
        if out.record.subscription_usdc > 0 {
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
                out.record.subscription_usdc,
            )?;
        }
        // 2) 销毁本期赎回的份额（用户申请赎回时已转入份额托管账户）
        if out.record.redemption_shares > 0 {
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
                out.record.redemption_shares,
            )?;
        }
        // 3) 为本期申购发行新份额，放在份额托管账户，等用户领取
        if out.record.minted_shares > 0 {
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
                out.record.minted_shares,
            )?;
        }

        // 4) 兑换（原型：模拟交易所，按预言机价格成交并收手续费）
        let actual_out = match out.swap {
            None => None,
            Some(plan) => {
                let market = &ctx.accounts.market;
                let m_auth = market.authority;
                let m_bump = market.bump;
                let m_seeds: &[&[u8]] = &[b"market", m_auth.as_ref(), &[m_bump]];
                let m_signer = &[m_seeds];
                let (fund_from, mkt_to, mkt_from, fund_to, raw_out) = match plan.side {
                    logic::Side::BuyBtc => (
                        ctx.accounts.usdc_vault.to_account_info(),
                        ctx.accounts.market_usdc.to_account_info(),
                        ctx.accounts.market_btc.to_account_info(),
                        ctx.accounts.btc_vault.to_account_info(),
                        logic::usdc_to_btc(plan.amount_in, price).map_err(map_err)?,
                    ),
                    logic::Side::SellBtc => (
                        ctx.accounts.btc_vault.to_account_info(),
                        ctx.accounts.market_btc.to_account_info(),
                        ctx.accounts.market_usdc.to_account_info(),
                        ctx.accounts.usdc_vault.to_account_info(),
                        logic::btc_value(plan.amount_in, price).map_err(map_err)?,
                    ),
                };
                let got =
                    ((raw_out as u128) * (10_000u128 - market.fee_bps as u128) / 10_000u128) as u64;
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
                Some(got)
            }
        };

        // 5) 校验成交、预留赎回款、开启下一期（成交不足会让整笔交易回滚）
        let payout = logic::finish_settle(&mut st, &mut out, actual_out).map_err(map_err)?;
        if payout > 0 {
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
                payout,
            )?;
        }

        let epoch_acc = &mut ctx.accounts.epoch_record;
        epoch_acc.fund = ctx.accounts.fund.key();
        epoch_acc.bump = ctx.bumps.epoch_record;
        epoch_acc.set_from_logic(&out.record);

        ctx.accounts.fund.state = StateData::from_logic(&st);
        emit!(Settled {
            epoch: out.record.epoch,
            day: out.record.day,
            signal_price: out.record.signal_price,
            valuation_price: out.record.valuation_price,
            moving_average: out.record.moving_average,
            nav: out.record.nav,
            in_btc: out.record.in_btc_after,
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
    pub max_staleness_secs: i64, // 预言机价格最多允许多旧
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
        }
    }
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
    pub in_btc: bool,
    pub last_settled_day: i64,
    pub epoch: u64,
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
            in_btc: s.in_btc,
            last_settled_day: s.last_settled_day,
            epoch: s.epoch,
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
            in_btc: self.in_btc,
            last_settled_day: self.last_settled_day,
            epoch: self.epoch,
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
    pub payout_usdc: u64,
    pub in_btc_after: bool,
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
        self.payout_usdc = r.payout_usdc;
        self.in_btc_after = r.in_btc_after;
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
            payout_usdc: self.payout_usdc,
            in_btc_after: self.in_btc_after,
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

/// 原型专用：模拟交易所，按预言机价格成交，收 fee_bps 手续费
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
pub struct Settled {
    pub epoch: u64,
    pub day: i64,
    pub signal_price: u64,
    pub valuation_price: u64,
    pub moving_average: u64,
    pub nav: u128,
    pub in_btc: bool,
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
pub struct Settle<'info> {
    /// 任何人都可以触发结算，并支付本期记录账户的租金
    #[account(mut)]
    pub cranker: Signer<'info>,
    #[account(mut, seeds = [b"fund", fund.admin.as_ref()], bump = fund.bump,
              has_one = oracle, has_one = market, has_one = share_mint)]
    pub fund: Box<Account<'info, Fund>>,
    #[account(
        init, payer = cranker, space = 8 + EpochRecord::INIT_SPACE,
        seeds = [b"epoch", fund.key().as_ref(), &fund.state.epoch.to_le_bytes()], bump
    )]
    pub epoch_record: Box<Account<'info, EpochRecord>>,
    pub oracle: Box<Account<'info, MockOracle>>,
    #[account(seeds = [b"market", market.authority.as_ref()], bump = market.bump)]
    pub market: Box<Account<'info, MockMarket>>,
    #[account(mut, seeds = [b"market_usdc", market.key().as_ref()], bump)]
    pub market_usdc: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [b"market_btc", market.key().as_ref()], bump)]
    pub market_btc: Box<Account<'info, TokenAccount>>,
    #[account(mut)]
    pub share_mint: Box<Account<'info, Mint>>,
    #[account(mut, seeds = [b"usdc_vault", fund.key().as_ref()], bump)]
    pub usdc_vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [b"btc_vault", fund.key().as_ref()], bump)]
    pub btc_vault: Box<Account<'info, TokenAccount>>,
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

// ----------------------------------------------------------------------
// 错误
// ----------------------------------------------------------------------

#[error_code]
pub enum FundErrorCode {
    #[msg("还没到今天的结算时间（UTC 0 点 + 设定的偏移）")]
    TooEarly,
    #[msg("今天已经结算过")]
    AlreadySettled,
    #[msg("价格无效")]
    BadPrice,
    #[msg("预言机价格过旧")]
    StalePrice,
    #[msg("基金有份额但资产为零")]
    FundWiped,
    #[msg("兑换成交少于最低可接受数量")]
    SlippageExceeded,
    #[msg("USDC 不足以支付赎回")]
    InsufficientUsdc,
    #[msg("配置无效")]
    InvalidConfig,
    #[msg("数值溢出")]
    Overflow,
    #[msg("没有可领取的内容")]
    NothingToClaim,
    #[msg("这一期还没结算")]
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
    #[msg("已过今天的定价时刻、还没结算，暂停申购赎回")]
    QueueClosed,
    #[msg("定价价格的发布时间不在定价窗口内")]
    PriceNotAtFixTime,
    #[msg("最新价格与定价价格相差过大")]
    PriceDrift,
}

fn map_err(e: logic::FundError) -> Error {
    use logic::FundError as E;
    let code = match e {
        E::TooEarly => FundErrorCode::TooEarly,
        E::AlreadySettled => FundErrorCode::AlreadySettled,
        E::BadPrice => FundErrorCode::BadPrice,
        E::FundWiped => FundErrorCode::FundWiped,
        E::SlippageExceeded => FundErrorCode::SlippageExceeded,
        E::InsufficientUsdc => FundErrorCode::InsufficientUsdc,
        E::InvalidConfig => FundErrorCode::InvalidConfig,
        E::Overflow => FundErrorCode::Overflow,
        E::NothingToClaim => FundErrorCode::NothingToClaim,
        E::EpochNotSettled => FundErrorCode::EpochNotSettled,
        E::BelowMinSubscription => FundErrorCode::BelowMinSubscription,
        E::ZeroAmount => FundErrorCode::ZeroAmount,
        E::TooManyPrices => FundErrorCode::TooManyPrices,
        E::QueueClosed => FundErrorCode::QueueClosed,
        E::PriceNotAtFixTime => FundErrorCode::PriceNotAtFixTime,
        E::PriceDrift => FundErrorCode::PriceDrift,
    };
    error!(code)
}
