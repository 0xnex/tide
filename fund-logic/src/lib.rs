//! 均线基金的全部记账逻辑（纯 Rust，不依赖 Solana）。
//!
//! 单位约定：
//! - USDC：6 位小数，原始整数单位
//! - BTC（链上包装 BTC，如 cbBTC）：8 位小数，原始整数单位
//! - 份额：6 位小数，初始 1 份 = 1 USDC
//! - 价格：每 1 枚 BTC 值多少“微美元”（USD × 1e6），即 USDC 原始单位
//! - 净值：每 1 个份额原始单位值多少 USDC 原始单位，再乘以 NAV_SCALE
//!
//! 规则：
//! - 申购、赎回先排队，在下一次结算（每天 UTC 1 点之后）按结算时的净值成交，
//!   防止有人用旧净值套利。
//! - 净值和信号用“定价时刻”（当天 UTC 0 点 + settle_offset）发布的价格，
//!   不用调用 settle 那一刻的价格：settle 谁都能调，如果按调用时价格成交，
//!   调用人就能挑一天里对自己最有利的时刻。
//! - 定价时刻到当天结算完成之间，暂停申购和赎回：定价价格已经公开，
//!   这段时间排队等于按已知的旧净值成交。
//! - 结算时记录当天价格，如果价格高于最近 window 个价格的平均值，就持有 BTC；
//!   否则全部持有 USDC。价格不足 window 个时持有 USDC。
//! - 资产用内部记账，不读取代币账户余额，别人直接转账进来不会影响净值
//!   （防止“捐款抬高净值”攻击）。

pub const NAV_SCALE: u128 = 1_000_000_000_000; // 1e12
pub const BTC_UNIT: u128 = 100_000_000; // 1e8，1 枚 BTC 的原始单位
pub const BPS: u128 = 10_000;
pub const SECONDS_PER_DAY: i64 = 86_400;
pub const MAX_WINDOW: usize = 64;
/// 定价价格的发布时间必须落在 [定价时刻, 定价时刻 + FIX_WINDOW_SECS) 内。
/// 调用人只能在这几十秒内发布的价格里挑，可利用的价差可以忽略。
pub const FIX_WINDOW_SECS: i64 = 60;
/// 兑换时的最新价格与定价价格最多相差多少（基点）。
/// 晚结算时，两者之差由基金承担；全部赎回时差额会落到留下的人身上，
/// 所以超过这个幅度就拒绝结算（当天回滚，第二天按新的定价价格继续，不会永久卡住）。
pub const MAX_PRICE_DRIFT_BPS: u16 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FundError {
    TooEarly,         // 还没到当天结算时间
    AlreadySettled,   // 今天已经结算过
    BadPrice,         // 价格为 0
    FundWiped,        // 有份额但资产为 0
    SlippageExceeded, // 实际成交少于最低可接受数量
    InsufficientUsdc, // 结算后 USDC 不够支付赎回
    InvalidConfig,
    Overflow,
    NothingToClaim,
    EpochNotSettled,
    BelowMinDeposit,
    ZeroAmount,
    TooManyPrices,
    QueueClosed,       // 已过定价时刻、当天还没结算，暂停申购赎回
    PriceNotAtFixTime, // 定价价格的发布时间不在定价窗口内
    PriceDrift,        // 最新价格与定价价格相差超过 MAX_PRICE_DRIFT_BPS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub window: u8,              // 均线窗口（天），默认 50
    pub settle_offset_secs: i64, // 每天 UTC 0 点之后多少秒可以结算，默认 3600（1 点）
    pub max_slippage_bps: u16,   // 兑换相对预言机价格的最大滑点
    pub deposit_fee_bps: u16,    // 申购费，留在基金里，补偿其他持有人承担的买入成本
    pub withdraw_fee_bps: u16,   // 赎回费，同上
    pub min_deposit: u64,        // 最低申购金额（USDC 原始单位）
}

impl Config {
    pub fn default_config() -> Self {
        Config {
            window: 50,
            settle_offset_secs: 3_600,
            // 在 Binance 成交：0.075% 手续费 + 约 0.1% 滑点，再留出预言机与 Binance 的价差
            max_slippage_bps: 30,
            deposit_fee_bps: 0,
            withdraw_fee_bps: 0,
            min_deposit: 1_000_000, // 1 USDC
        }
    }

    pub fn validate(&self) -> Result<(), FundError> {
        if self.window == 0 || self.window as usize > MAX_WINDOW {
            return Err(FundError::InvalidConfig);
        }
        if !(0..SECONDS_PER_DAY).contains(&self.settle_offset_secs) {
            return Err(FundError::InvalidConfig);
        }
        if self.max_slippage_bps as u128 >= BPS
            || self.deposit_fee_bps as u128 >= BPS
            || self.withdraw_fee_bps as u128 >= BPS
        {
            return Err(FundError::InvalidConfig);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FundState {
    pub usdc: u64,                 // 基金主账户 USDC（不含排队中的申购款、待领取的赎回款）
    pub btc: u64,                  // 基金持有的 BTC
    pub share_supply: u64,         // 已发行份额（含排队赎回、尚未销毁的份额）
    pub pending_deposit_usdc: u64, // 本期排队的申购款
    pub pending_withdraw_shares: u64, // 本期排队赎回的份额
    pub prices: [u64; MAX_WINDOW], // 每日价格环形缓冲
    pub price_count: u8,           // 已记录的价格数量（最多 window）
    pub price_head: u8,            // 下一个写入位置
    pub in_btc: bool,
    pub last_settled_day: i64, // 最近一次结算的日期序号（unix 秒 / 86400），-1 表示从未结算
    pub epoch: u64,            // 当前开放申购/赎回的期号
}

impl FundState {
    pub fn new() -> Self {
        FundState {
            usdc: 0,
            btc: 0,
            share_supply: 0,
            pending_deposit_usdc: 0,
            pending_withdraw_shares: 0,
            prices: [0; MAX_WINDOW],
            price_count: 0,
            price_head: 0,
            in_btc: false,
            last_settled_day: -1,
            epoch: 0,
        }
    }
}

impl Default for FundState {
    fn default() -> Self {
        Self::new()
    }
}

/// 一期结算的结果，存到链上，用户据此领取份额或 USDC。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpochRecord {
    pub epoch: u64,
    pub day: i64,
    pub price: u64,
    pub moving_average: u64,  // 价格不足 window 个时为 0
    pub nav: u128,            // 结算净值（兑换前）
    pub deposit_usdc: u64,    // 本期申购总额
    pub minted_shares: u64,   // 本期新发行份额（按申购额比例分给申购人）
    pub withdraw_shares: u64, // 本期赎回份额
    pub payout_usdc: u64,     // 本期赎回应付 USDC（按份额比例分给赎回人）
    pub in_btc_after: bool,
    // 尚未领取的部分。每次领取后递减，最后一位领取人拿到精确的余数，
    // 不会留下取整“尘埃”。
    pub unclaimed_deposit_usdc: u64,
    pub unclaimed_shares: u64,
    pub unclaimed_withdraw_shares: u64,
    pub unclaimed_payout_usdc: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    BuyBtc,  // 用 USDC 买 BTC
    SellBtc, // 卖 BTC 换 USDC
}

/// 结算后需要执行的兑换。min_out 由预言机价格和最大滑点算出，
/// 外部兑换（模拟交易所或 Jupiter）实际成交少于 min_out 必须失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwapPlan {
    pub side: Side,
    pub amount_in: u64,
    pub min_out: u64,
}

/// 结算用的两个价格。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettlePrices {
    pub fix: u64,              // 定价时刻发布的价格：用于均线、信号和净值
    pub fix_publish_time: i64, // fix 的发布时间（unix 秒）
    pub exec: u64,             // 调用时的最新价格：只用于兑换数量和 min_out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettleOutcome {
    pub record: EpochRecord,
    pub swap: Option<SwapPlan>,
}

// ---------------------------------------------------------------------------
// 基础计算
// ---------------------------------------------------------------------------

fn to_u64(x: u128) -> Result<u64, FundError> {
    u64::try_from(x).map_err(|_| FundError::Overflow)
}

/// BTC 原始单位按价格折算成 USDC 原始单位（向下取整）
pub fn btc_value(btc: u64, price: u64) -> Result<u64, FundError> {
    to_u64((btc as u128) * (price as u128) / BTC_UNIT)
}

/// USDC 原始单位按价格能买多少 BTC 原始单位（向下取整）
pub fn usdc_to_btc(usdc: u64, price: u64) -> Result<u64, FundError> {
    if price == 0 {
        return Err(FundError::BadPrice);
    }
    to_u64((usdc as u128) * BTC_UNIT / (price as u128))
}

fn apply_bps_down(x: u64, bps: u16) -> Result<u64, FundError> {
    to_u64((x as u128) * (BPS - bps as u128) / BPS)
}

pub fn total_assets(s: &FundState, price: u64) -> Result<u64, FundError> {
    let v = btc_value(s.btc, price)?;
    s.usdc.checked_add(v).ok_or(FundError::Overflow)
}

/// 当前净值（每份额原始单位的 USDC 原始单位 × NAV_SCALE）。
pub fn nav(s: &FundState, price: u64) -> Result<u128, FundError> {
    if s.share_supply == 0 {
        return Ok(NAV_SCALE); // 尚无份额：1 份 = 1 USDC
    }
    let assets = total_assets(s, price)? as u128;
    if assets == 0 {
        return Err(FundError::FundWiped);
    }
    Ok(assets * NAV_SCALE / s.share_supply as u128)
}

/// 当前价格窗口的简单平均。价格不足 window 个时返回 None。
pub fn moving_average(s: &FundState, window: u8) -> Option<u64> {
    if s.price_count < window || window == 0 {
        return None;
    }
    let w = window as usize;
    let mut sum: u128 = 0;
    for k in 0..w {
        let idx = (s.price_head as usize + MAX_WINDOW - 1 - k) % MAX_WINDOW;
        sum += s.prices[idx] as u128;
    }
    Some((sum / w as u128) as u64)
}

pub fn push_price(s: &mut FundState, price: u64, window: u8) {
    s.prices[s.price_head as usize] = price;
    s.price_head = ((s.price_head as usize + 1) % MAX_WINDOW) as u8;
    if s.price_count < window {
        s.price_count += 1;
    }
}

/// 管理员在首次结算前写入历史价格，让均线从第一天就能计算。
/// 只能在从未结算过时调用（由外层检查）。
pub fn seed_prices(s: &mut FundState, prices: &[u64], cfg: &Config) -> Result<(), FundError> {
    if prices.len() > cfg.window as usize {
        return Err(FundError::TooManyPrices);
    }
    for &p in prices {
        if p == 0 {
            return Err(FundError::BadPrice);
        }
        push_price(s, p, cfg.window);
    }
    Ok(())
}

pub fn day_of(unix: i64) -> i64 {
    unix.div_euclid(SECONDS_PER_DAY)
}

/// 当天的定价时刻（unix 秒）
pub fn fix_time(day: i64, cfg: &Config) -> i64 {
    day * SECONDS_PER_DAY + cfg.settle_offset_secs
}

/// 申购赎回是否开放：定价时刻之后、当天结算之前关闭。
pub fn check_queue_open(s: &FundState, now_unix: i64, cfg: &Config) -> Result<(), FundError> {
    let day = day_of(now_unix);
    if now_unix >= fix_time(day, cfg) && s.last_settled_day < day {
        return Err(FundError::QueueClosed);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 申购、赎回排队
// ---------------------------------------------------------------------------

/// 用户的排队记录。一个用户同时只能有一期未领取的记录。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ticket {
    pub epoch: u64,
    pub deposit_usdc: u64,
    pub withdraw_shares: u64,
}

impl Ticket {
    pub fn is_empty(&self) -> bool {
        self.deposit_usdc == 0 && self.withdraw_shares == 0
    }
}

/// 外层必须先确认：ticket 为空，或 ticket.epoch == s.epoch（同一期可以追加）。
pub fn request_deposit(
    s: &mut FundState,
    t: &mut Ticket,
    amount: u64,
    now_unix: i64,
    cfg: &Config,
) -> Result<(), FundError> {
    check_queue_open(s, now_unix, cfg)?;
    if amount == 0 {
        return Err(FundError::ZeroAmount);
    }
    if amount < cfg.min_deposit {
        return Err(FundError::BelowMinDeposit);
    }
    if !t.is_empty() && t.epoch != s.epoch {
        return Err(FundError::NothingToClaim); // 上一期还没领取，外层应先 claim
    }
    t.epoch = s.epoch;
    t.deposit_usdc = t
        .deposit_usdc
        .checked_add(amount)
        .ok_or(FundError::Overflow)?;
    s.pending_deposit_usdc = s
        .pending_deposit_usdc
        .checked_add(amount)
        .ok_or(FundError::Overflow)?;
    Ok(())
}

pub fn request_withdraw(
    s: &mut FundState,
    t: &mut Ticket,
    shares: u64,
    now_unix: i64,
    cfg: &Config,
) -> Result<(), FundError> {
    check_queue_open(s, now_unix, cfg)?;
    if shares == 0 {
        return Err(FundError::ZeroAmount);
    }
    if !t.is_empty() && t.epoch != s.epoch {
        return Err(FundError::NothingToClaim);
    }
    t.epoch = s.epoch;
    t.withdraw_shares = t
        .withdraw_shares
        .checked_add(shares)
        .ok_or(FundError::Overflow)?;
    s.pending_withdraw_shares = s
        .pending_withdraw_shares
        .checked_add(shares)
        .ok_or(FundError::Overflow)?;
    Ok(())
}

/// 从剩余池子里按比例领取：part / pool_basis × pool_amount，向下取整；
/// 领取后两个池子都扣减。最后一位（part == pool_basis）拿走全部余数。
fn take_pro_rata(part: u64, pool_basis: &mut u64, pool_amount: &mut u64) -> Result<u64, FundError> {
    if part == 0 {
        return Ok(0);
    }
    if part > *pool_basis {
        return Err(FundError::Overflow);
    }
    let out = if part == *pool_basis {
        *pool_amount
    } else {
        to_u64((part as u128) * (*pool_amount as u128) / (*pool_basis as u128))?
    };
    *pool_basis -= part;
    *pool_amount -= out;
    Ok(out)
}

/// 结算之后，用户按本期总额的比例领取。返回 (领取的份额, 领取的 USDC)。
/// 领取之和精确等于本期总额，不留尘埃。
pub fn claim(t: &mut Ticket, r: &mut EpochRecord) -> Result<(u64, u64), FundError> {
    if t.is_empty() {
        return Err(FundError::NothingToClaim);
    }
    if t.epoch != r.epoch {
        return Err(FundError::EpochNotSettled);
    }
    let shares = take_pro_rata(
        t.deposit_usdc,
        &mut r.unclaimed_deposit_usdc,
        &mut r.unclaimed_shares,
    )?;
    let usdc = take_pro_rata(
        t.withdraw_shares,
        &mut r.unclaimed_withdraw_shares,
        &mut r.unclaimed_payout_usdc,
    )?;
    *t = Ticket::default();
    Ok((shares, usdc))
}

// ---------------------------------------------------------------------------
// 每日结算
// ---------------------------------------------------------------------------

/// 第一步：记账和决定兑换。调用后外层执行 swap（如果有），再调用 finish_settle。
///
/// 顺序：
///   1. 检查时间：今天还没结算，且已过定价时刻（UTC 0 点 + settle_offset）；
///      定价价格在定价窗口内发布；最新价格与定价价格相差不超过 MAX_PRICE_DRIFT_BPS
///   2. 记录定价价格，计算均线和信号
///   3. 按定价价格、“兑换前”的净值处理赎回（销毁份额，计算应付）和申购（发行份额）
///   4. 按最新价格决定兑换：信号为持有 BTC 时，除了赎回应付以外的 USDC 全部买 BTC；
///      信号为现金时，卖出全部 BTC；
///      持有 BTC 但 USDC 不够付赎回时，卖出刚好够的 BTC（多卖一点覆盖滑点）
///
/// 无论什么时候调用，净值和信号都只取决于定价价格，调用时机不影响成交价。
pub fn begin_settle(
    s: &mut FundState,
    now_unix: i64,
    prices: &SettlePrices,
    cfg: &Config,
) -> Result<SettleOutcome, FundError> {
    cfg.validate()?;
    if prices.fix == 0 || prices.exec == 0 {
        return Err(FundError::BadPrice);
    }
    let day = day_of(now_unix);
    if day <= s.last_settled_day {
        return Err(FundError::AlreadySettled);
    }
    let fix_at = fix_time(day, cfg);
    if now_unix < fix_at {
        return Err(FundError::TooEarly);
    }
    let t = prices.fix_publish_time;
    if t < fix_at || t >= fix_at + FIX_WINDOW_SECS || t > now_unix {
        return Err(FundError::PriceNotAtFixTime);
    }
    let drift = (prices.exec as u128).abs_diff(prices.fix as u128);
    if drift * BPS > (prices.fix as u128) * MAX_PRICE_DRIFT_BPS as u128 {
        return Err(FundError::PriceDrift);
    }
    let price = prices.fix;

    // 2. 价格与信号
    push_price(s, price, cfg.window);
    let ma = moving_average(s, cfg.window);
    let signal_btc = match ma {
        Some(m) => price > m,
        None => false,
    };

    // 3. 净值（兑换前），先赎回后申购，二者都按同一净值
    let nav_now = nav(s, price)?;

    let withdraw_shares = s.pending_withdraw_shares;
    let gross_payout = (withdraw_shares as u128) * nav_now / NAV_SCALE;
    let payout = apply_bps_down(to_u64(gross_payout)?, cfg.withdraw_fee_bps)?;
    s.share_supply = s
        .share_supply
        .checked_sub(withdraw_shares)
        .ok_or(FundError::Overflow)?;
    s.pending_withdraw_shares = 0;

    let deposit = s.pending_deposit_usdc;
    let net_deposit = apply_bps_down(deposit, cfg.deposit_fee_bps)?;
    let minted = to_u64((net_deposit as u128) * NAV_SCALE / nav_now)?;
    s.share_supply = s
        .share_supply
        .checked_add(minted)
        .ok_or(FundError::Overflow)?;
    s.usdc = s.usdc.checked_add(deposit).ok_or(FundError::Overflow)?; // 申购费留在基金里
    s.pending_deposit_usdc = 0;

    // 4. 兑换计划：按真正成交时的最新价格计算数量和 min_out
    let price = prices.exec;
    let slip = cfg.max_slippage_bps;
    let swap = if signal_btc {
        if s.usdc >= payout {
            let spend = s.usdc - payout;
            if spend > 0 {
                let expected = usdc_to_btc(spend, price)?;
                Some(SwapPlan {
                    side: Side::BuyBtc,
                    amount_in: spend,
                    min_out: apply_bps_down(expected, slip)?,
                })
            } else {
                None
            }
        } else {
            // USDC 不够付赎回：卖出 deficit / (1 - slip) 对应的 BTC，向上取整，不超过持有量
            let deficit = (payout - s.usdc) as u128;
            let need_usdc = (deficit * BPS + (BPS - slip as u128) - 1) / (BPS - slip as u128);
            let btc_needed = (need_usdc * BTC_UNIT + price as u128 - 1) / price as u128;
            let sell = to_u64(btc_needed.min(s.btc as u128))?;
            let expected = btc_value(sell, price)?;
            Some(SwapPlan {
                side: Side::SellBtc,
                amount_in: sell,
                min_out: apply_bps_down(expected, slip)?,
            })
        }
    } else if s.btc > 0 {
        let expected = btc_value(s.btc, price)?;
        Some(SwapPlan {
            side: Side::SellBtc,
            amount_in: s.btc,
            min_out: apply_bps_down(expected, slip)?,
        })
    } else {
        None
    };

    // 价格过低导致买入数量为 0 时，跳过兑换
    let swap = swap.filter(|p| p.amount_in > 0 && p.min_out > 0);

    let record = EpochRecord {
        epoch: s.epoch,
        day,
        price: prices.fix,
        moving_average: ma.unwrap_or(0),
        nav: nav_now,
        deposit_usdc: deposit,
        minted_shares: minted,
        withdraw_shares,
        payout_usdc: payout,
        in_btc_after: signal_btc,
        unclaimed_deposit_usdc: deposit,
        unclaimed_shares: minted,
        unclaimed_withdraw_shares: withdraw_shares,
        unclaimed_payout_usdc: payout,
    };
    s.last_settled_day = day;
    Ok(SettleOutcome { record, swap })
}

/// 第二步：记录兑换的实际成交，预留赎回款，开启下一期。
/// actual_out 为外部兑换实际收到的数量；没有兑换时传 None。
/// 返回需要从主账户转到“待领取”账户的赎回款。
///
/// 如果为了支付赎回而卖出 BTC，成交成本让 USDC 略少于应付金额，
/// 差额由本期赎回人承担（按比例减少应付），而不是让结算卡住、
/// 或者让留下的人替他们付成本。调整后的金额写回 out.record.payout_usdc。
pub fn finish_settle(
    s: &mut FundState,
    out: &mut SettleOutcome,
    actual_out: Option<u64>,
) -> Result<u64, FundError> {
    if let Some(plan) = out.swap {
        let got = actual_out.ok_or(FundError::SlippageExceeded)?;
        if got < plan.min_out {
            return Err(FundError::SlippageExceeded);
        }
        match plan.side {
            Side::BuyBtc => {
                s.usdc = s
                    .usdc
                    .checked_sub(plan.amount_in)
                    .ok_or(FundError::Overflow)?;
                s.btc = s.btc.checked_add(got).ok_or(FundError::Overflow)?;
            }
            Side::SellBtc => {
                s.btc = s
                    .btc
                    .checked_sub(plan.amount_in)
                    .ok_or(FundError::Overflow)?;
                s.usdc = s.usdc.checked_add(got).ok_or(FundError::Overflow)?;
            }
        }
    }
    let mut payout = out.record.payout_usdc;
    if s.usdc < payout {
        let sold_for_payout = matches!(
            out.swap,
            Some(SwapPlan {
                side: Side::SellBtc,
                ..
            })
        );
        if !sold_for_payout {
            return Err(FundError::InsufficientUsdc);
        }
        payout = s.usdc;
        out.record.payout_usdc = payout;
        out.record.unclaimed_payout_usdc = payout;
    }
    s.usdc -= payout;
    s.in_btc = s.btc > 0 && out.record.in_btc_after;
    s.epoch = s.epoch.checked_add(1).ok_or(FundError::Overflow)?;
    Ok(payout)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const USDC: u64 = 1_000_000;
    const DAY: i64 = SECONDS_PER_DAY;
    const ONE_AM: i64 = 3_600;

    fn cfg() -> Config {
        Config::default_config()
    }

    /// 价格在定价时刻发布，调用时没有变化
    fn at_fix(day: i64, price: u64) -> SettlePrices {
        SettlePrices {
            fix: price,
            fix_publish_time: day * DAY + ONE_AM,
            exec: price,
        }
    }

    /// 测试用的模拟交易所：按价格成交，收取 fee_bps
    fn exec(plan: &SwapPlan, price: u64, fee_bps: u16) -> u64 {
        let raw = match plan.side {
            Side::BuyBtc => usdc_to_btc(plan.amount_in, price).unwrap(),
            Side::SellBtc => btc_value(plan.amount_in, price).unwrap(),
        };
        apply_bps_down(raw, fee_bps).unwrap()
    }

    /// 完整结算一天，返回 (记录, 应付赎回款)
    fn settle_day(s: &mut FundState, day: i64, price: u64, fee_bps: u16) -> (EpochRecord, u64) {
        let (r, p, _) = settle_day_swap(s, day, price, fee_bps);
        (r, p)
    }

    fn settle_day_swap(
        s: &mut FundState,
        day: i64,
        price: u64,
        fee_bps: u16,
    ) -> (EpochRecord, u64, Option<SwapPlan>) {
        settle_at(s, day * DAY + ONE_AM, &at_fix(day, price), fee_bps)
    }

    /// 在 now 调用结算，交易所按最新价格 p.exec 成交
    fn settle_at(
        s: &mut FundState,
        now: i64,
        p: &SettlePrices,
        fee_bps: u16,
    ) -> (EpochRecord, u64, Option<SwapPlan>) {
        let mut out = begin_settle(s, now, p, &cfg()).unwrap();
        let got = out.swap.map(|plan| exec(&plan, p.exec, fee_bps));
        let payout = finish_settle(s, &mut out, got).unwrap();
        (out.record, payout, out.swap)
    }

    fn seeded(price: u64) -> FundState {
        let mut s = FundState::new();
        seed_prices(&mut s, &vec![price; 49], &cfg()).unwrap();
        s
    }

    #[test]
    fn first_deposit_is_one_to_one() {
        let mut s = FundState::new();
        let mut t = Ticket::default();
        request_deposit(&mut s, &mut t, 100 * USDC, 100 * DAY, &cfg()).unwrap();
        let (mut r, _) = settle_day(&mut s, 100, 60_000 * USDC, 0);
        assert_eq!(r.minted_shares, 100 * USDC);
        let (sh, u) = claim(&mut t, &mut r).unwrap();
        assert_eq!((sh, u), (100 * USDC, 0));
        assert_eq!(s.share_supply, 100 * USDC);
        assert!(!s.in_btc, "价格不足 50 个，应持有现金");
    }

    #[test]
    fn cannot_settle_before_1am_or_twice() {
        let mut s = FundState::new();
        assert_eq!(
            begin_settle(&mut s, 100 * DAY + ONE_AM - 1, &at_fix(100, 1), &cfg()),
            Err(FundError::TooEarly)
        );
        let mut out = begin_settle(&mut s, 100 * DAY + ONE_AM, &at_fix(100, 1), &cfg()).unwrap();
        finish_settle(&mut s, &mut out, None).unwrap();
        assert_eq!(
            begin_settle(&mut s, 100 * DAY + 20 * 3600, &at_fix(100, 1), &cfg()),
            Err(FundError::AlreadySettled)
        );
        assert!(begin_settle(&mut s, 101 * DAY + ONE_AM, &at_fix(101, 1), &cfg()).is_ok());
    }

    #[test]
    fn switches_to_btc_above_ma_and_back_to_cash_below() {
        let p0 = 50_000 * USDC;
        let mut s = seeded(p0);
        let mut t = Ticket::default();
        request_deposit(&mut s, &mut t, 1_000 * USDC, 10 * DAY, &cfg()).unwrap();
        // 第 50 个价格高于前 49 个 -> 高于均线 -> 买入
        let (mut r, _) = settle_day(&mut s, 10, 55_000 * USDC, 0);
        assert!(r.in_btc_after && s.in_btc);
        assert_eq!(s.usdc, 0);
        assert!(s.btc > 0);
        claim(&mut t, &mut r).unwrap();
        // 价格跌回均线下方 -> 全部卖出
        let (mut r2, _) = settle_day(&mut s, 11, 45_000 * USDC, 0);
        assert!(!r2.in_btc_after && !s.in_btc);
        assert_eq!(s.btc, 0);
        assert!(s.usdc > 0);
        // 1000 USDC 以 55000 买入，以 45000 卖出 -> 约 818 USDC
        let expect = 1_000.0 * 45_000.0 / 55_000.0;
        let got = s.usdc as f64 / USDC as f64;
        assert!((got - expect).abs() < 0.01, "got {got}, expect {expect}");
    }

    #[test]
    fn late_depositor_cannot_use_stale_nav() {
        // 老持有人 A 在 BTC 里；B 在价格大涨之后才申购，必须按大涨后的净值成交
        let p0 = 50_000 * USDC;
        let mut s = seeded(p0);
        let mut ta = Ticket::default();
        request_deposit(&mut s, &mut ta, 1_000 * USDC, 10 * DAY, &cfg()).unwrap();
        let (mut r, _) = settle_day(&mut s, 10, 51_000 * USDC, 0); // 买入 BTC
        claim(&mut ta, &mut r).unwrap();
        let a_shares = s.share_supply;

        let mut tb = Ticket::default();
        request_deposit(&mut s, &mut tb, 1_000 * USDC, 11 * DAY, &cfg()).unwrap();
        // 第二天 BTC 涨 20%
        let (mut r2, _) = settle_day(&mut s, 11, 61_200 * USDC, 0);
        let (b_shares, _) = claim(&mut tb, &mut r2).unwrap();
        // B 拿到的份额应约为 A 的 1/1.2（价格涨了 20%，净值也涨了 20%）
        let ratio = b_shares as f64 / a_shares as f64;
        assert!((ratio - 1.0 / 1.2).abs() < 1e-4, "ratio {ratio}");
        // A 的价值不被稀释：A 的份额 × 净值 ≈ 1000 × 1.2
        let a_value =
            (a_shares as u128 * nav(&s, 61_200 * USDC).unwrap() / NAV_SCALE) as f64 / USDC as f64;
        assert!((a_value - 1_200.0).abs() < 0.05, "a_value {a_value}");
    }

    #[test]
    fn withdraw_paid_at_settlement_nav_and_btc_sold_if_needed() {
        let p0 = 50_000 * USDC;
        let mut s = seeded(p0);
        let mut ta = Ticket::default();
        let mut tb = Ticket::default();
        request_deposit(&mut s, &mut ta, 1_000 * USDC, 10 * DAY, &cfg()).unwrap();
        request_deposit(&mut s, &mut tb, 3_000 * USDC, 10 * DAY, &cfg()).unwrap();
        let (mut r, _) = settle_day(&mut s, 10, 51_000 * USDC, 0);
        let (a_sh, _) = claim(&mut ta, &mut r).unwrap();
        claim(&mut tb, &mut r).unwrap();
        assert_eq!(s.usdc, 0, "全部在 BTC 里");

        // A 赎回全部份额；第二天价格继续上涨（仍高于均线），基金要卖出一部分 BTC 付款
        request_withdraw(&mut s, &mut ta, a_sh, 11 * DAY, &cfg()).unwrap();
        let (mut r2, payout) = settle_day(&mut s, 11, 56_100 * USDC, 0);
        let (_, a_usdc) = claim(&mut ta, &mut r2).unwrap();
        assert_eq!(payout, r2.payout_usdc);
        assert_eq!(a_usdc, payout);
        // A 应拿回约 1000 × 1.1 = 1100 USDC
        let got = a_usdc as f64 / USDC as f64;
        assert!((got - 1_100.0).abs() < 0.05, "got {got}");
        assert!(s.in_btc, "剩余资金继续持有 BTC");
        assert_eq!(s.share_supply, r.minted_shares - a_sh);
    }

    #[test]
    fn slippage_guard_rejects_bad_fill() {
        let mut s = seeded(50_000 * USDC);
        let mut t = Ticket::default();
        request_deposit(&mut s, &mut t, 1_000 * USDC, 10 * DAY, &cfg()).unwrap();
        let price = 51_000 * USDC;
        let mut out = begin_settle(&mut s, 10 * DAY + ONE_AM, &at_fix(10, price), &cfg()).unwrap();
        let plan = out.swap.unwrap();
        let bad = exec(&plan, price, 40); // 0.4%，超过 0.3% 上限
        assert_eq!(
            finish_settle(&mut s.clone(), &mut out.clone(), Some(bad)),
            Err(FundError::SlippageExceeded)
        );
        let ok = exec(&plan, price, 18); // 0.18%：Binance 手续费 + 滑点
        assert!(finish_settle(&mut s, &mut out, Some(ok)).is_ok());
    }

    #[test]
    fn fees_stay_in_fund() {
        let c = Config {
            deposit_fee_bps: 100,
            ..cfg()
        };
        let mut s = FundState::new();
        let mut ta = Ticket::default();
        request_deposit(&mut s, &mut ta, 1_000 * USDC, 10 * DAY, &c).unwrap();
        let mut out =
            begin_settle(&mut s, 10 * DAY + ONE_AM, &at_fix(10, 50_000 * USDC), &c).unwrap();
        finish_settle(&mut s, &mut out, None).unwrap();
        // 第一个人：1% 申购费留在基金里，他自己的份额净值随之提高
        assert_eq!(out.record.minted_shares, 990 * USDC);
        assert_eq!(s.usdc, 1_000 * USDC);
        let n = nav(&s, 50_000 * USDC).unwrap();
        assert!(n > NAV_SCALE);
    }

    #[test]
    fn claim_requires_settled_epoch_and_only_once() {
        let mut s = FundState::new();
        let mut t = Ticket::default();
        request_deposit(&mut s, &mut t, 5 * USDC, 1 * DAY, &cfg()).unwrap();
        let (mut r, _) = settle_day(&mut s, 1, 1 * USDC, 0);
        let mut wrong = r;
        wrong.epoch = 99;
        assert_eq!(
            claim(&mut t.clone(), &mut wrong),
            Err(FundError::EpochNotSettled)
        );
        claim(&mut t, &mut r).unwrap();
        assert_eq!(claim(&mut t, &mut r), Err(FundError::NothingToClaim));
    }

    #[test]
    fn min_deposit_enforced() {
        let mut s = FundState::new();
        let mut t = Ticket::default();
        assert_eq!(
            request_deposit(&mut s, &mut t, USDC - 1, 1 * DAY, &cfg()),
            Err(FundError::BelowMinDeposit)
        );
    }

    #[test]
    fn moving_average_uses_last_window_prices() {
        let c = Config { window: 3, ..cfg() };
        let mut s = FundState::new();
        for p in [10, 20, 30, 40] {
            push_price(&mut s, p, c.window);
        }
        assert_eq!(moving_average(&s, 3), Some(30)); // (20+30+40)/3
        let s2 = FundState::new();
        assert_eq!(moving_average(&s2, 3), None);
    }

    /// 随机模拟：许多用户随机申购赎回、价格随机波动，检查不变量：
    ///  - 所有用户持有的份额之和 == 发行总量
    ///  - 领取的份额、USDC 之和不超过每期总额
    ///  - 没有手续费和滑点时，资产总值 = 所有用户的价值之和（误差只来自向下取整）
    #[test]
    fn random_simulation_invariants() {
        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> u64 {
                self.0 = self
                    .0
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                self.0 >> 33
            }
            fn below(&mut self, n: u64) -> u64 {
                self.next() % n
            }
        }
        for seed in 1..=20u64 {
            let mut rng = Rng(seed);
            let mut s = seeded(50_000 * USDC);
            let n_users = 8;
            let mut tickets = vec![Ticket::default(); n_users];
            let mut held = vec![0u64; n_users];
            let mut cash_out = vec![0u64; n_users];
            let mut cash_in = vec![0u64; n_users];
            let mut price = 50_000 * USDC;
            for day in 10..400i64 {
                // 用户行为
                for u in 0..n_users {
                    if !tickets[u].is_empty() {
                        continue;
                    }
                    match rng.below(10) {
                        0 | 1 => {
                            let amt = (1 + rng.below(5_000)) * USDC;
                            request_deposit(&mut s, &mut tickets[u], amt, day * DAY, &cfg())
                                .unwrap();
                            cash_in[u] += amt;
                        }
                        2 if held[u] > 0 => {
                            let sh = 1 + rng.below(held[u]);
                            held[u] -= sh;
                            request_withdraw(&mut s, &mut tickets[u], sh, day * DAY, &cfg())
                                .unwrap();
                        }
                        _ => {}
                    }
                }
                // 价格随机游走（±8%）
                let step = rng.below(1601) as i64 - 800; // -800..=800 bps
                price = ((price as i128) * (10_000 + step as i128) / 10_000)
                    .max(1_000 * USDC as i128) as u64;
                let (mut r, payout) = settle_day(&mut s, day, price, 0);
                let mut minted_claimed = 0u64;
                let mut paid_claimed = 0u64;
                for u in 0..n_users {
                    if !tickets[u].is_empty() && tickets[u].epoch == r.epoch {
                        let (sh, us) = claim(&mut tickets[u], &mut r).unwrap();
                        held[u] += sh;
                        cash_out[u] += us;
                        minted_claimed += sh;
                        paid_claimed += us;
                    }
                }
                // 所有人都领取后，领取之和精确等于本期总额，没有尘埃
                assert_eq!(minted_claimed, r.minted_shares);
                assert_eq!(paid_claimed, payout);
                assert_eq!(r.unclaimed_shares, 0);
                assert_eq!(r.unclaimed_payout_usdc, 0);
                let held_total: u64 = held.iter().sum();
                // 已发行份额 = 用户持有 + 本期排队赎回中的份额
                let queued: u64 = tickets.iter().map(|t| t.withdraw_shares).sum();
                assert_eq!(s.share_supply, held_total + queued);
            }
            // 价值守恒：基金总资产 ≈ 用户份额价值之和（误差为取整尘埃）
            let n = nav(&s, price).unwrap();
            let user_value: u128 = held.iter().map(|&h| h as u128 * n / NAV_SCALE).sum();
            let assets = total_assets(&s, price).unwrap() as u128;
            assert!(assets >= user_value);
            assert!(
                assets - user_value < 1_000 * USDC as u128 / 1000,
                "seed {seed}: 尘埃过大 {}",
                assets - user_value
            );
        }
    }

    #[test]
    fn everyone_exits_while_in_btc_does_not_get_stuck() {
        let mut s = seeded(50_000 * USDC);
        let mut ta = Ticket::default();
        let mut tb = Ticket::default();
        request_deposit(&mut s, &mut ta, 1_000 * USDC, 10 * DAY, &cfg()).unwrap();
        request_deposit(&mut s, &mut tb, 2_000 * USDC, 10 * DAY, &cfg()).unwrap();
        let (mut r, _) = settle_day(&mut s, 10, 51_000 * USDC, 18);
        let (a, _) = claim(&mut ta, &mut r).unwrap();
        let (b, _) = claim(&mut tb, &mut r).unwrap();
        // 两人同时赎回全部份额，信号仍为持有 BTC，交易所收 0.18%
        request_withdraw(&mut s, &mut ta, a, 11 * DAY, &cfg()).unwrap();
        request_withdraw(&mut s, &mut tb, b, 11 * DAY, &cfg()).unwrap();
        let (mut r2, payout, swap) = settle_day_swap(&mut s, 11, 52_000 * USDC, 18);
        assert!(matches!(
            swap,
            Some(SwapPlan {
                side: Side::SellBtc,
                ..
            })
        ));
        let (_, ua) = claim(&mut ta, &mut r2).unwrap();
        let (_, ub) = claim(&mut tb, &mut r2).unwrap();
        assert!(ua + ub <= payout);
        assert_eq!(s.share_supply, 0);
        // 两人按 1:2 分配，成交成本由他们承担
        let ratio = ub as f64 / ua as f64;
        assert!((ratio - 2.0).abs() < 1e-3, "ratio {ratio}");
        // 基金还能继续运作
        let mut tc = Ticket::default();
        request_deposit(&mut s, &mut tc, 10 * USDC, 12 * DAY, &cfg()).unwrap();
        settle_day(&mut s, 12, 53_000 * USDC, 18);
    }

    #[test]
    fn late_settle_fills_at_fix_price_whatever_the_call_time() {
        let mut s = seeded(50_000 * USDC);
        let mut ta = Ticket::default();
        request_deposit(&mut s, &mut ta, 1_000 * USDC, 10 * DAY, &cfg()).unwrap();
        let (mut r, _) = settle_day(&mut s, 10, 51_000 * USDC, 0); // 买入 BTC
        let (a_sh, _) = claim(&mut ta, &mut r).unwrap();
        let mut tb = Ticket::default();
        request_withdraw(&mut s, &mut ta, a_sh / 10, 11 * DAY, &cfg()).unwrap();
        request_deposit(&mut s, &mut tb, 500 * USDC, 11 * DAY, &cfg()).unwrap();

        let fix = 52_000 * USDC;
        let fix_at = 11 * DAY + ONE_AM;
        let on_time = at_fix(11, fix);
        // 晚上 9 点才结算，这时 BTC 已经涨了约 0.96%
        let late = SettlePrices {
            fix,
            fix_publish_time: fix_at + 30,
            exec: 52_500 * USDC,
        };
        let a = begin_settle(&mut s.clone(), fix_at, &on_time, &cfg()).unwrap();
        let mut s_late = s.clone();
        let mut b = begin_settle(&mut s_late, 11 * DAY + 21 * 3600, &late, &cfg()).unwrap();

        // 净值、发行份额、赎回应付都只取决于定价价格，和调用时间无关
        assert_eq!(a.record, b.record);
        assert_eq!(b.record.price, fix);
        assert!(b.record.payout_usdc > 0 && b.record.minted_shares > 0);

        // 兑换数量和 min_out 按真正成交时的价格计算
        let (pa, pb) = (a.swap.unwrap(), b.swap.unwrap());
        assert_eq!((pa.side, pb.side), (Side::BuyBtc, Side::BuyBtc));
        assert_eq!(pa.amount_in, pb.amount_in);
        let slip = cfg().max_slippage_bps;
        let expect =
            |price| apply_bps_down(usdc_to_btc(pb.amount_in, price).unwrap(), slip).unwrap();
        assert_eq!(pa.min_out, expect(fix));
        assert_eq!(pb.min_out, expect(late.exec));
        let got = exec(&pb, late.exec, 18);
        assert!(finish_settle(&mut s_late, &mut b, Some(got)).is_ok());
    }

    #[test]
    fn fix_price_must_be_published_in_fix_window() {
        let s = FundState::new();
        let fix_at = 10 * DAY + ONE_AM;
        let now = fix_at + 5 * 3600;
        let at = |t: i64| SettlePrices {
            fix: USDC,
            fix_publish_time: t,
            exec: USDC,
        };
        let run = |now: i64, t: i64| begin_settle(&mut s.clone(), now, &at(t), &cfg()).map(|_| ());
        assert_eq!(run(now, fix_at), Ok(()));
        assert_eq!(run(now, fix_at + FIX_WINDOW_SECS - 1), Ok(()));
        // 定价时刻之前、窗口之后（比如调用时的最新价格）、前一天的定价价格都不行
        let bad = Err(FundError::PriceNotAtFixTime);
        assert_eq!(run(now, fix_at - 1), bad);
        assert_eq!(run(now, fix_at + FIX_WINDOW_SECS), bad);
        assert_eq!(run(now, now), bad);
        assert_eq!(run(now, fix_at - DAY), bad);
        // 发布时间不能晚于调用时间
        assert_eq!(run(fix_at + 10, fix_at + 20), bad);
    }

    #[test]
    fn price_drift_beyond_limit_is_rejected() {
        let s = FundState::new();
        let fix = 50_000 * USDC;
        let limit = fix / BPS as u64 * MAX_PRICE_DRIFT_BPS as u64;
        let run = |exec: u64| {
            let p = SettlePrices {
                exec,
                ..at_fix(10, fix)
            };
            begin_settle(&mut s.clone(), 10 * DAY + 12 * 3600, &p, &cfg()).map(|_| ())
        };
        assert_eq!(run(fix + limit), Ok(()));
        assert_eq!(run(fix - limit), Ok(()));
        assert_eq!(run(fix + limit + 1), Err(FundError::PriceDrift));
        assert_eq!(run(fix - limit - 1), Err(FundError::PriceDrift));
    }

    #[test]
    fn queue_closed_between_fix_time_and_settlement() {
        let mut s = seeded(50_000 * USDC);
        let fix_at = 10 * DAY + ONE_AM;
        let mut ta = Ticket::default();
        request_deposit(&mut s, &mut ta, 100 * USDC, fix_at - 1, &cfg()).unwrap(); // 00:59:59 还开放

        // 定价价格公开之后，到当天结算之前，申购赎回都关闭
        let mut tb = Ticket::default();
        let closed = Err(FundError::QueueClosed);
        assert_eq!(
            request_deposit(&mut s, &mut tb, 100 * USDC, fix_at, &cfg()),
            closed
        );
        assert_eq!(
            request_withdraw(&mut s, &mut tb, 1, fix_at + 12 * 3600, &cfg()),
            closed
        );
        assert_eq!(tb, Ticket::default());

        // 结算之后重新开放，进入下一期
        let (mut r, _) = settle_day(&mut s, 10, 51_000 * USDC, 0);
        claim(&mut ta, &mut r).unwrap();
        request_deposit(&mut s, &mut tb, 100 * USDC, fix_at + 60, &cfg()).unwrap();
        assert_eq!(tb.epoch, r.epoch + 1);

        // 某天没人结算：定价时刻之后一直关闭，第二天 0 点重新开放
        let mut tc = Ticket::default();
        assert_eq!(
            request_deposit(&mut s, &mut tc, 100 * USDC, 11 * DAY + ONE_AM, &cfg()),
            closed
        );
        assert_eq!(
            request_deposit(&mut s, &mut tc, 100 * USDC, 12 * DAY - 1, &cfg()),
            closed
        );
        request_deposit(&mut s, &mut tc, 100 * USDC, 12 * DAY, &cfg()).unwrap();
    }

    #[test]
    fn random_simulation_with_trading_costs_never_gets_stuck() {
        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> u64 {
                self.0 = self
                    .0
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                self.0 >> 33
            }
        }
        let (mut buys, mut sells_exit, mut sells_payout, mut late) = (0, 0, 0, 0);
        for seed in 1..=20u64 {
            let mut rng = Rng(seed * 7919);
            let mut s = seeded(50_000 * USDC);
            let mut tickets = vec![Ticket::default(); 6];
            let mut held = vec![0u64; 6];
            let mut price = 50_000 * USDC;
            for day in 10..300i64 {
                for u in 0..6 {
                    if !tickets[u].is_empty() {
                        continue;
                    }
                    let r = rng.next() % 10;
                    if r < 2 {
                        request_deposit(
                            &mut s,
                            &mut tickets[u],
                            (1 + rng.next() % 3_000) * USDC,
                            day * DAY,
                            &cfg(),
                        )
                        .unwrap();
                    } else if r < 4 && held[u] > 0 {
                        let sh = if r == 2 {
                            held[u]
                        } else {
                            1 + rng.next() % held[u]
                        };
                        held[u] -= sh;
                        request_withdraw(&mut s, &mut tickets[u], sh, day * DAY, &cfg()).unwrap();
                    }
                }
                let step = (rng.next() % 1601) as i64 - 800;
                price = ((price as i128) * (10_000 + step as i128) / 10_000)
                    .max(1_000 * USDC as i128) as u64;
                // 当天任意时刻结算；最新价格与定价价格最多差 MAX_PRICE_DRIFT_BPS
                let fix_at = day * DAY + ONE_AM;
                let now = fix_at + (rng.next() % (DAY - ONE_AM) as u64) as i64;
                let drift = (rng.next() % 201) as i128 - 100; // -100..=100 bps
                let p = SettlePrices {
                    fix: price,
                    fix_publish_time: (fix_at + (rng.next() % FIX_WINDOW_SECS as u64) as i64)
                        .min(now),
                    // 价差向零取整，保证不超过限额
                    exec: (price as i128 + price as i128 * drift / 10_000) as u64,
                };
                if now > fix_at {
                    late += 1;
                    let t = fix_at + (rng.next() % (now - fix_at) as u64) as i64;
                    assert_eq!(
                        request_deposit(&mut s, &mut Ticket::default(), USDC, t, &cfg()),
                        Err(FundError::QueueClosed)
                    );
                }
                let was_in = s.in_btc;
                let (mut r, _, swap) = settle_at(&mut s, now, &p, 25); // 交易所收 0.25%，接近 0.3% 滑点上限
                match swap {
                    Some(SwapPlan {
                        side: Side::BuyBtc, ..
                    }) => buys += 1,
                    Some(SwapPlan {
                        side: Side::SellBtc,
                        ..
                    }) if r.in_btc_after => sells_payout += 1,
                    Some(SwapPlan {
                        side: Side::SellBtc,
                        ..
                    }) => {
                        assert!(was_in || s.btc == 0);
                        sells_exit += 1
                    }
                    None => {}
                }
                for u in 0..6 {
                    if !tickets[u].is_empty() && tickets[u].epoch == r.epoch {
                        let (sh, _) = claim(&mut tickets[u], &mut r).unwrap();
                        held[u] += sh;
                    }
                }
                let queued: u64 = tickets.iter().map(|t| t.withdraw_shares).sum();
                assert_eq!(s.share_supply, held.iter().sum::<u64>() + queued);
            }
        }
        // 三条兑换路径和晚结算都应被覆盖到
        assert!(
            buys > 0 && sells_exit > 0 && sells_payout > 0 && late > 0,
            "{buys} {sells_exit} {sells_payout} {late}"
        );
    }

    #[test]
    fn config_validation() {
        assert!(Config { window: 0, ..cfg() }.validate().is_err());
        assert!(Config {
            window: 65,
            ..cfg()
        }
        .validate()
        .is_err());
        assert!(Config {
            settle_offset_secs: 86_400,
            ..cfg()
        }
        .validate()
        .is_err());
        assert!(Config {
            max_slippage_bps: 10_000,
            ..cfg()
        }
        .validate()
        .is_err());
        assert!(cfg().validate().is_ok());
    }
}
