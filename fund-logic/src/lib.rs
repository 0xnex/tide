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
//! - 申购、赎回先排队，在下一次定价（每天 UTC 1 点）按定价净值成交，
//!   防止有人用旧净值套利。
//! - 结算分多步（ADR 0001）：定价步骤（fix）记录价格、算出目标和净值、
//!   销毁赎回份额并按比例划出赎回资产；之后是一系列交易（plan_trade + fill_trade），
//!   每笔不超过 trade_cap_usdc，间隔至少 min_trade_interval_secs。
//! - 交易优先级：先卖未卖完的赎回资产（最早的一期优先），再把基金调向目标。
//! - 赎回人拿到自己那份赎回资产实际卖出的 USDC（ADR 0002），整期卖完之后才能领取。
//! - 下一个定价时刻到来时没做完的交易延续下去：赎回资产继续优先卖出，
//!   基金调仓按新的目标重新计算，结算永远不会卡住。
//! - 净值和信号用“定价时刻”（当天 UTC 0 点 + settle_offset）发布的价格，
//!   不用调用 fix 那一刻的价格：调用人不能挑一天里对自己最有利的时刻。
//! - 定价时刻到当天定价步骤之间，暂停申购和赎回：定价价格已经公开，
//!   这段时间排队等于按已知的旧净值成交。
//! - 两个价格（ADR 0003）：信号价格（BTC/USD）用于均线和信号，因为策略跟踪的是 BTC；
//!   估值价格（cbBTC/USD）用于净值和交易的 min_out，因为基金实际持有的是 cbBTC，
//!   cbBTC 脱锚时按 BTC 价格估值会多付给赎回人、由留下的人承担。
//! - 定价时记录当天的信号价格，如果高于最近 window 个信号价格的平均值，目标是持有 cbBTC；
//!   否则全部持有 USDC。价格不足 window 个时持有 USDC。
//! - 每笔交易前检查两个最新价格：相差超过 depeg_threshold_bps（脱锚）时不买入 cbBTC；
//!   卖出和定价照常进行，持续脱锚也不会让基金卡住。
//! - 资产用内部记账，不读取代币账户余额，别人直接转账进来不会影响净值
//!   （防止“捐款抬高净值”攻击）。赎回资产单独记账，不算基金资产。

pub const NAV_SCALE: u128 = 1_000_000_000_000; // 1e12
pub const BTC_UNIT: u128 = 100_000_000; // 1e8，1 枚 BTC 的原始单位
pub const BPS: u128 = 10_000;
pub const SECONDS_PER_DAY: i64 = 86_400;
pub const MAX_WINDOW: usize = 64;
/// 同时未卖完的赎回资产最多几期。正常情况下当天就能卖完；
/// 满了之后定价步骤暂时失败，当期申购赎回顺延到下一次定价，卖出照常进行。
pub const MAX_SLICES: usize = 8;
/// 定价价格的发布时间必须落在 [定价时刻, 定价时刻 + FIX_WINDOW_SECS) 内。
/// 调用人只能在这几十秒内发布的价格里挑，可利用的价差可以忽略。
pub const FIX_WINDOW_SECS: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FundError {
    TooEarly,         // 还没到当天定价时刻
    AlreadySettled,   // 今天已经定价过
    BadPrice,         // 价格为 0
    FundWiped,        // 有份额但资产为 0
    SlippageExceeded, // 实际成交少于 min_out
    InvalidConfig,
    Overflow,
    NothingToClaim,
    EpochNotSettled,
    BelowMinSubscription,
    ZeroAmount,
    TooManyPrices,
    QueueClosed,       // 已过定价时刻、当天还没定价，暂停申购赎回
    PriceNotAtFixTime, // 定价价格的发布时间不在定价窗口内
    NothingToTrade,    // 没有需要交易的东西
    TradeTooSoon,      // 距离上一笔交易不到 min_trade_interval_secs
    DepegBlocksBuy,    // 需要买入 cbBTC，但 cbBTC 脱锚
    SliceNotSold,      // 这一期的赎回资产还没卖完
    WrongSliceRecord,  // 卖出赎回资产时传入的期记录与交易计划不符
    TooManySlices,     // 未卖完的赎回资产已有 MAX_SLICES 期
    FixPending,        // 已过今天的定价时刻、还没定价，基金调仓等待定价
    KeeperOnly,        // 清算窗口内只有清算地址能驱动结算
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub window: u8,                   // 均线窗口（天），默认 50
    pub settle_offset_secs: i64,      // 定价时刻：每天 UTC 0 点之后多少秒，默认 3600（1 点）
    pub max_slippage_bps: u16,        // 交易相对估值价格的最大滑点
    pub subscription_fee_bps: u16,    // 申购费，留在基金里
    pub redemption_fee_bps: u16,      // 赎回费，同上
    pub min_subscription: u64,        // 最低申购金额（USDC 原始单位）
    pub depeg_threshold_bps: u16,     // cbBTC/USD 与 BTC/USD 相差超过这个幅度就不买入 cbBTC
    pub trade_cap_usdc: u64,          // 单笔交易上限（按估值价格折算的 USDC 名义金额）
    pub min_trade_interval_secs: i64, // 两笔交易之间至少间隔多少秒
    pub keeper_window_secs: i64,      // 定价时刻之后多少秒内只有清算地址能驱动结算
    pub public_slippage_bps: u16,     // 窗口之后其他人交易的滑点上限，比 max_slippage_bps 更严
}

impl Config {
    pub fn default_config() -> Self {
        Config {
            window: 50,
            settle_offset_secs: 3_600,
            // 0.075% 手续费 + 约 0.1% 滑点，再留出预言机与成交价的价差
            max_slippage_bps: 30,
            subscription_fee_bps: 0,
            redemption_fee_bps: 0,
            min_subscription: 1_000_000, // 1 USDC
            depeg_threshold_bps: 200,
            trade_cap_usdc: 200_000_000_000, // 200,000 USDC
            min_trade_interval_secs: 60,
            keeper_window_secs: 1_800,
            public_slippage_bps: 10,
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
            || self.subscription_fee_bps as u128 >= BPS
            || self.redemption_fee_bps as u128 >= BPS
            || self.depeg_threshold_bps == 0
            || self.depeg_threshold_bps as u128 >= BPS
        {
            return Err(FundError::InvalidConfig);
        }
        if self.trade_cap_usdc == 0 || self.min_trade_interval_secs < 0 {
            return Err(FundError::InvalidConfig);
        }
        // 窗口只按当天的定价时刻判断，所以不能跨过午夜（两者都已限制在一天以内，不会溢出）
        if !(0..SECONDS_PER_DAY).contains(&self.keeper_window_secs)
            || self.settle_offset_secs + self.keeper_window_secs > SECONDS_PER_DAY
            || self.public_slippage_bps > self.max_slippage_bps
        {
            return Err(FundError::InvalidConfig);
        }
        Ok(())
    }
}

/// 一期赎回资产中还没卖完的部分。cbBTC 留在基金的 BTC 账户里，
/// USDC（定价时划出的部分 + 卖出所得）在待领取账户里，都不算基金资产。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Slice {
    pub epoch: u64,
    pub btc: u64,  // 还没卖出的 cbBTC
    pub usdc: u64, // 已有的 USDC：定价时划出的 + 已卖出所得
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FundState {
    pub usdc: u64,                     // 基金持有的 USDC（不含排队中的申购款、赎回资产）
    pub btc: u64,                      // 基金持有的 cbBTC（不含赎回资产）
    pub share_supply: u64,             // 已发行份额（含排队赎回、尚未销毁的份额）
    pub queued_subscription_usdc: u64, // 本期排队的申购款
    pub queued_redemption_shares: u64, // 本期排队赎回的份额
    pub prices: [u64; MAX_WINDOW],     // 每日信号价格环形缓冲
    pub price_count: u8,               // 已记录的价格数量（最多 window）
    pub price_head: u8,                // 下一个写入位置
    pub target_btc: bool,              // 最近一次定价的目标：true 持有 cbBTC，false 持有 USDC
    pub last_fix_day: i64,             // 最近一次定价的日期序号（unix 秒 / 86400），-1 表示从未定价
    pub last_trade_time: i64,          // 最近一笔交易的时间（unix 秒）
    pub epoch: u64,                    // 当前开放申购/赎回的期号
    pub slices: [Slice; MAX_SLICES],   // 未卖完的赎回资产，最早的在前
    pub slice_count: u8,
}

impl FundState {
    pub fn new() -> Self {
        FundState {
            usdc: 0,
            btc: 0,
            share_supply: 0,
            queued_subscription_usdc: 0,
            queued_redemption_shares: 0,
            prices: [0; MAX_WINDOW],
            price_count: 0,
            price_head: 0,
            target_btc: false,
            last_fix_day: -1,
            last_trade_time: 0,
            epoch: 0,
            slices: [Slice::default(); MAX_SLICES],
            slice_count: 0,
        }
    }

    /// 最早的一期未卖完的赎回资产（下一笔交易优先卖它）
    pub fn oldest_slice(&self) -> Option<&Slice> {
        self.slices[..self.slice_count as usize].first()
    }
}

impl Default for FundState {
    fn default() -> Self {
        Self::new()
    }
}

/// 一期定价的结果，存到链上，用户据此领取份额或 USDC。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpochRecord {
    pub epoch: u64,
    pub day: i64,
    pub signal_price: u64,      // 定价时刻的 BTC/USD，记入均线
    pub valuation_price: u64,   // 定价时刻的 cbBTC/USD，用于净值
    pub moving_average: u64,    // 价格不足 window 个时为 0
    pub nav: u128,              // 定价净值
    pub subscription_usdc: u64, // 本期申购总额
    pub minted_shares: u64,     // 本期新发行份额（按申购额比例分给申购人）
    pub redemption_shares: u64, // 本期赎回（已销毁）的份额
    pub slice_usdc: u64,        // 定价时划给赎回人的 USDC
    pub slice_btc: u64,         // 定价时划给赎回人、需要卖出的 cbBTC
    pub payout_usdc: u64,       // 赎回资产全部卖完后的 USDC 总额（按份额比例分给赎回人）
    pub slice_sold_out: bool,   // 赎回资产已全部卖完，赎回人可以领取
    pub target_btc: bool,       // 本次定价的目标（信号）
    // 尚未领取的部分。每次领取后递减，最后一位领取人拿到精确的余数，
    // 不会留下取整“尘埃”。
    pub unclaimed_subscription_usdc: u64,
    pub unclaimed_shares: u64,
    pub unclaimed_redemption_shares: u64,
    pub unclaimed_payout_usdc: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    BuyBtc,  // 用 USDC 买 cbBTC
    SellBtc, // 卖 cbBTC 换 USDC
}

/// 一个预言机价格及其发布时间（unix 秒）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PricePoint {
    pub price: u64,
    pub publish_time: i64,
}

/// 定价步骤用的价格：两个价格源在定价窗口内发布的价格。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixPrices {
    pub signal: PricePoint,    // BTC/USD：均线和信号
    pub valuation: PricePoint, // cbBTC/USD：净值
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

/// x × part / whole，向下取整
fn pro_rata(x: u64, part: u64, whole: u64) -> Result<u64, FundError> {
    if whole == 0 {
        return Err(FundError::Overflow);
    }
    to_u64((x as u128) * (part as u128) / (whole as u128))
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

/// 管理员在首次定价前写入历史价格，让均线从第一天就能计算。
/// 只能在从未定价过时调用（由外层检查）。
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

/// cbBTC 是否脱锚：估值价格与信号价格之差超过信号价格的 threshold_bps。
pub fn is_depegged(signal: u64, valuation: u64, threshold_bps: u16) -> bool {
    let gap = (valuation as u128).abs_diff(signal as u128);
    gap * BPS > (signal as u128) * threshold_bps as u128
}

pub fn day_of(unix: i64) -> i64 {
    unix.div_euclid(SECONDS_PER_DAY)
}

/// 当天的定价时刻（unix 秒）
pub fn fix_time(day: i64, cfg: &Config) -> i64 {
    day * SECONDS_PER_DAY + cfg.settle_offset_secs
}

/// 申购赎回是否开放：定价时刻之后、当天定价步骤之前关闭。
pub fn check_queue_open(s: &FundState, now_unix: i64, cfg: &Config) -> Result<(), FundError> {
    let day = day_of(now_unix);
    if now_unix >= fix_time(day, cfg) && s.last_fix_day < day {
        return Err(FundError::QueueClosed);
    }
    Ok(())
}

/// 驱动结算（定价、交易）的调用人
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    Keeper, // 配置里的清算地址
    Public, // 其他任何人
}

/// 清算窗口（当天定价时刻起 keeper_window_secs 秒）内只有清算地址能驱动结算，
/// 防止第三方抢在清算地址之前交易。窗口之后谁都可以，保证清算地址停机时基金照常推进。
fn check_keeper_window(now_unix: i64, caller: Caller, cfg: &Config) -> Result<(), FundError> {
    let fix_at = fix_time(day_of(now_unix), cfg);
    let in_window = now_unix >= fix_at && now_unix < fix_at.saturating_add(cfg.keeper_window_secs);
    if in_window && caller == Caller::Public {
        return Err(FundError::KeeperOnly);
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
    pub subscription_usdc: u64,
    pub redemption_shares: u64,
}

impl Ticket {
    pub fn is_empty(&self) -> bool {
        self.subscription_usdc == 0 && self.redemption_shares == 0
    }
}

/// 外层必须先确认：ticket 为空，或 ticket.epoch == s.epoch（同一期可以追加）。
pub fn request_subscription(
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
    if amount < cfg.min_subscription {
        return Err(FundError::BelowMinSubscription);
    }
    if !t.is_empty() && t.epoch != s.epoch {
        return Err(FundError::NothingToClaim); // 上一期还没领取，外层应先 claim
    }
    t.epoch = s.epoch;
    t.subscription_usdc = t
        .subscription_usdc
        .checked_add(amount)
        .ok_or(FundError::Overflow)?;
    s.queued_subscription_usdc = s
        .queued_subscription_usdc
        .checked_add(amount)
        .ok_or(FundError::Overflow)?;
    Ok(())
}

pub fn request_redemption(
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
    t.redemption_shares = t
        .redemption_shares
        .checked_add(shares)
        .ok_or(FundError::Overflow)?;
    s.queued_redemption_shares = s
        .queued_redemption_shares
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

/// 定价之后，用户按本期总额的比例领取。返回 (领取的份额, 领取的 USDC)。
/// 有赎回的记录要等这一期的赎回资产全部卖完才能领取。
/// 领取之和精确等于本期总额，不留尘埃。
pub fn claim(t: &mut Ticket, r: &mut EpochRecord) -> Result<(u64, u64), FundError> {
    if t.is_empty() {
        return Err(FundError::NothingToClaim);
    }
    if t.epoch != r.epoch {
        return Err(FundError::EpochNotSettled);
    }
    if t.redemption_shares > 0 && !r.slice_sold_out {
        return Err(FundError::SliceNotSold);
    }
    let shares = take_pro_rata(
        t.subscription_usdc,
        &mut r.unclaimed_subscription_usdc,
        &mut r.unclaimed_shares,
    )?;
    let usdc = take_pro_rata(
        t.redemption_shares,
        &mut r.unclaimed_redemption_shares,
        &mut r.unclaimed_payout_usdc,
    )?;
    *t = Ticket::default();
    Ok((shares, usdc))
}

// ---------------------------------------------------------------------------
// 定价步骤
// ---------------------------------------------------------------------------

/// 定价步骤：在定价时刻之后调用一次，开启当天的结算。返回本期记录。
/// 外层据此转账：申购款从托管转入基金、销毁赎回份额、发行新份额、
/// 把 slice_usdc 转入待领取账户（slice_btc 留在 BTC 账户里，之后卖出）。
///
/// 顺序：
///   1. 检查调用人和时间：清算窗口内只有清算地址；今天还没定价，且已过定价时刻；
///      两个定价价格都在定价窗口内发布
///   2. 记录信号价格，计算均线和目标
///   3. 按估值价格计算净值
///   4. 销毁赎回份额，按份额比例从基金划出赎回资产（USDC 和 cbBTC 都向下取整，对留下的人有利）
///   5. 申购按同一净值发行份额，申购款进入基金
///
/// 无论什么时候调用，净值和信号都只取决于定价价格，调用时机不影响成交价。
/// 未卖完的赎回资产和基金调仓不受影响：它们在之后的交易里继续。
pub fn fix(
    s: &mut FundState,
    now_unix: i64,
    prices: &FixPrices,
    caller: Caller,
    cfg: &Config,
) -> Result<EpochRecord, FundError> {
    cfg.validate()?;
    check_keeper_window(now_unix, caller, cfg)?;
    if prices.signal.price == 0 || prices.valuation.price == 0 {
        return Err(FundError::BadPrice);
    }
    let day = day_of(now_unix);
    if day <= s.last_fix_day {
        return Err(FundError::AlreadySettled);
    }
    let fix_at = fix_time(day, cfg);
    if now_unix < fix_at {
        return Err(FundError::TooEarly);
    }
    for t in [prices.signal.publish_time, prices.valuation.publish_time] {
        if t < fix_at || t >= fix_at.saturating_add(FIX_WINDOW_SECS) || t > now_unix {
            return Err(FundError::PriceNotAtFixTime);
        }
    }

    // 先算出所有数量再修改状态，出错时状态不变
    let nav_now = nav(s, prices.valuation.price)?;
    let redemption_shares = s.queued_redemption_shares;
    let (slice_usdc, slice_btc) = if redemption_shares == 0 {
        (0, 0)
    } else {
        let supply = s.share_supply;
        let fee = cfg.redemption_fee_bps; // 赎回费留在基金里
        (
            apply_bps_down(pro_rata(s.usdc, redemption_shares, supply)?, fee)?,
            apply_bps_down(pro_rata(s.btc, redemption_shares, supply)?, fee)?,
        )
    };
    if slice_btc > 0 && s.slice_count as usize >= MAX_SLICES {
        return Err(FundError::TooManySlices);
    }
    let subscription_usdc = s.queued_subscription_usdc;
    let net_subscription = apply_bps_down(subscription_usdc, cfg.subscription_fee_bps)?;
    let minted = to_u64((net_subscription as u128) * NAV_SCALE / nav_now)?;
    let share_supply = s
        .share_supply
        .checked_sub(redemption_shares)
        .and_then(|x| x.checked_add(minted))
        .ok_or(FundError::Overflow)?;
    let usdc = s
        .usdc
        .checked_sub(slice_usdc)
        .and_then(|x| x.checked_add(subscription_usdc)) // 申购费留在基金里
        .ok_or(FundError::Overflow)?;
    let btc = s.btc.checked_sub(slice_btc).ok_or(FundError::Overflow)?;

    // 2. 信号价格进均线，决定目标
    let signal = prices.signal.price;
    push_price(s, signal, cfg.window);
    let ma = moving_average(s, cfg.window);
    let target_btc = matches!(ma, Some(m) if signal > m);

    // 4、5. 赎回资产离开基金，申购款进入基金
    s.usdc = usdc;
    s.btc = btc;
    s.share_supply = share_supply;
    s.queued_redemption_shares = 0;
    s.queued_subscription_usdc = 0;
    // 没有 cbBTC 要卖的赎回资产当场就完成了
    let slice_sold_out = slice_btc == 0;
    if !slice_sold_out {
        s.slices[s.slice_count as usize] = Slice {
            epoch: s.epoch,
            btc: slice_btc,
            usdc: slice_usdc,
        };
        s.slice_count += 1; // 上面已检查 slice_count < MAX_SLICES
    }
    let payout = if slice_sold_out { slice_usdc } else { 0 };

    let record = EpochRecord {
        epoch: s.epoch,
        day,
        signal_price: signal,
        valuation_price: prices.valuation.price,
        moving_average: ma.unwrap_or(0),
        nav: nav_now,
        subscription_usdc,
        minted_shares: minted,
        redemption_shares,
        slice_usdc,
        slice_btc,
        payout_usdc: payout,
        slice_sold_out,
        target_btc,
        unclaimed_subscription_usdc: subscription_usdc,
        unclaimed_shares: minted,
        unclaimed_redemption_shares: redemption_shares,
        unclaimed_payout_usdc: payout,
    };
    s.target_btc = target_btc;
    s.last_fix_day = day;
    s.epoch = s.epoch.checked_add(1).ok_or(FundError::Overflow)?;
    Ok(record)
}

// ---------------------------------------------------------------------------
// 交易
// ---------------------------------------------------------------------------

/// 调用时两个价格源的最新价格
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketPrices {
    pub signal: u64,    // BTC/USD：只用于脱锚检查
    pub valuation: u64, // cbBTC/USD：交易数量、min_out 和脱锚检查
}

/// 一笔交易服务的对象
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeBucket {
    Slice { epoch: u64 }, // 卖出第 epoch 期的赎回资产，所得归这一期的赎回人
    Fund,                 // 把基金调向目标
}

impl TradeBucket {
    /// 卖出的是哪一期的赎回资产；基金调仓为 None
    pub fn slice_epoch(&self) -> Option<u64> {
        match *self {
            TradeBucket::Slice { epoch } => Some(epoch),
            TradeBucket::Fund => None,
        }
    }
}

/// 一笔交易。min_out 由执行时的估值价格和最大滑点算出，
/// 外部兑换（模拟交易所或 Jupiter）实际成交少于 min_out 必须失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradePlan {
    pub side: Side,
    pub amount_in: u64,
    pub min_out: u64,
    pub bucket: TradeBucket,
}

/// 规划下一笔交易。清算窗口内其他调用人得到 KeeperOnly。没有交易时返回原因：
/// - FixPending：已过今天的定价时刻、还没定价，基金调仓要等定价
/// - NothingToTrade：赎回资产都卖完了，基金也已经是目标持仓
/// - DepegBlocksBuy：下一笔是买入 cbBTC，但 cbBTC 脱锚
/// - TradeTooSoon：距离上一笔交易不到最小间隔
///
/// 前三个原因先于 TradeTooSoon 判断：它们表示等多久都没有交易（直到下一次定价或恢复锚定），
/// 驱动交易的脚本据此停下；TradeTooSoon 表示稍后重试。
///
/// 优先级：最早一期未卖完的赎回资产，然后是基金调仓。
/// 每笔按当前估值价格折算不超过 trade_cap_usdc。
pub fn plan_trade(
    s: &FundState,
    now_unix: i64,
    prices: &MarketPrices,
    caller: Caller,
    cfg: &Config,
) -> Result<TradePlan, FundError> {
    cfg.validate()?;
    check_keeper_window(now_unix, caller, cfg)?;
    if prices.signal == 0 || prices.valuation == 0 {
        return Err(FundError::BadPrice);
    }
    let price = prices.valuation;
    // 窗口之后的其他调用人用更严的滑点上限：能从一笔交易里挤出的价值更少
    let slip = match caller {
        Caller::Keeper => cfg.max_slippage_bps,
        Caller::Public => cfg.public_slippage_bps,
    };
    // 上限折算成 cbBTC 向下取整；至少 1 sat，保证价格再高也能卖完
    let cap_btc = usdc_to_btc(cfg.trade_cap_usdc, price)?.max(1);
    let plan = if let Some(slice) = s.oldest_slice() {
        // 赎回资产无论多小都要卖完：min_out 为 0 时成交 0 也接受，赎回人承担这点尘埃
        let amount = slice.btc.min(cap_btc);
        TradePlan {
            side: Side::SellBtc,
            amount_in: amount,
            min_out: apply_bps_down(btc_value(amount, price)?, slip)?,
            bucket: TradeBucket::Slice { epoch: slice.epoch },
        }
    } else if now_unix >= fix_time(day_of(now_unix), cfg) && s.last_fix_day < day_of(now_unix) {
        // 定价净值按定价时刻的价格给基金资产估值。定价时刻之后的调仓成交如果算进去，
        // 当天的申购赎回就会按定价之后的成交价成交（赎回资产不算基金资产，不受影响）
        return Err(FundError::FixPending);
    } else if s.target_btc {
        let spend = s.usdc.min(cfg.trade_cap_usdc);
        let min_out = apply_bps_down(usdc_to_btc(spend, price)?, slip)?;
        // 买不到 1 sat 的零头留在基金里
        if min_out == 0 {
            return Err(FundError::NothingToTrade);
        }
        if is_depegged(prices.signal, price, cfg.depeg_threshold_bps) {
            return Err(FundError::DepegBlocksBuy);
        }
        TradePlan {
            side: Side::BuyBtc,
            amount_in: spend,
            min_out,
            bucket: TradeBucket::Fund,
        }
    } else {
        if s.btc == 0 {
            return Err(FundError::NothingToTrade);
        }
        let amount = s.btc.min(cap_btc);
        TradePlan {
            side: Side::SellBtc,
            amount_in: amount,
            min_out: apply_bps_down(btc_value(amount, price)?, slip)?,
            bucket: TradeBucket::Fund,
        }
    };
    if now_unix
        < s.last_trade_time
            .saturating_add(cfg.min_trade_interval_secs)
    {
        return Err(FundError::TradeTooSoon);
    }
    Ok(plan)
}

/// 按价格成交、收 fee_bps 手续费的交易所会给出多少（向下取整）。
/// devnet 的模拟交易所和测试用它；真实交易所的成交由外部决定。
pub fn market_fill(plan: &TradePlan, price: u64, fee_bps: u16) -> Result<u64, FundError> {
    if fee_bps as u128 >= BPS {
        return Err(FundError::InvalidConfig);
    }
    let raw = match plan.side {
        Side::BuyBtc => usdc_to_btc(plan.amount_in, price)?,
        Side::SellBtc => btc_value(plan.amount_in, price)?,
    };
    apply_bps_down(raw, fee_bps)
}

/// 记录一笔交易的实际成交 actual_out。成交少于 min_out 返回 SlippageExceeded，
/// 状态不变（外层整笔交易回滚）。
///
/// 卖出赎回资产时必须传入那一期的记录：卖完的那一笔把全部 USDC 写入记录的应付款，
/// 赎回人从此可以领取。所得 USDC 由外层转入待领取账户，不进入基金。
pub fn fill_trade(
    s: &mut FundState,
    plan: &TradePlan,
    actual_out: u64,
    now_unix: i64,
    slice_record: Option<&mut EpochRecord>,
) -> Result<(), FundError> {
    if actual_out < plan.min_out {
        return Err(FundError::SlippageExceeded);
    }
    match plan.bucket {
        TradeBucket::Slice { epoch } => {
            let r = slice_record.ok_or(FundError::WrongSliceRecord)?;
            let oldest = s.oldest_slice().copied();
            let slice = match oldest {
                Some(sl) if sl.epoch == epoch && r.epoch == epoch => sl,
                _ => return Err(FundError::WrongSliceRecord),
            };
            if plan.side != Side::SellBtc {
                return Err(FundError::WrongSliceRecord);
            }
            let usdc = slice
                .usdc
                .checked_add(actual_out)
                .ok_or(FundError::Overflow)?;
            let btc = slice
                .btc
                .checked_sub(plan.amount_in)
                .ok_or(FundError::WrongSliceRecord)?;
            if btc > 0 {
                s.slices[0] = Slice { epoch, btc, usdc };
            } else {
                r.payout_usdc = usdc;
                r.unclaimed_payout_usdc = usdc;
                r.slice_sold_out = true;
                let n = s.slice_count as usize;
                s.slices.copy_within(1..n, 0);
                s.slices[n - 1] = Slice::default();
                s.slice_count = (n - 1) as u8; // 有最早的一期，n ≥ 1
            }
        }
        TradeBucket::Fund => match plan.side {
            Side::BuyBtc => {
                let usdc = s
                    .usdc
                    .checked_sub(plan.amount_in)
                    .ok_or(FundError::Overflow)?;
                s.btc = s.btc.checked_add(actual_out).ok_or(FundError::Overflow)?;
                s.usdc = usdc;
            }
            Side::SellBtc => {
                let btc = s
                    .btc
                    .checked_sub(plan.amount_in)
                    .ok_or(FundError::Overflow)?;
                s.usdc = s.usdc.checked_add(actual_out).ok_or(FundError::Overflow)?;
                s.btc = btc;
            }
        },
    }
    s.last_trade_time = now_unix;
    Ok(())
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

    /// 两个价格都在定价时刻发布
    fn at_fix2(day: i64, signal: u64, valuation: u64) -> FixPrices {
        let t = day * DAY + ONE_AM;
        FixPrices {
            signal: PricePoint {
                price: signal,
                publish_time: t,
            },
            valuation: PricePoint {
                price: valuation,
                publish_time: t,
            },
        }
    }

    fn at_fix(day: i64, price: u64) -> FixPrices {
        at_fix2(day, price, price)
    }

    /// 基金状态加上每一期的记录（下标即期号），模拟链上的账户。
    /// usdc_tokens / btc_tokens 按程序的转账记录基金名下的代币（托管、主账户、待领取账户），
    /// 用来核对内部记账：不能凭空多出或少掉资产。
    struct Harness {
        s: FundState,
        records: Vec<EpochRecord>,
        c: Config,
        usdc_tokens: u64,
        btc_tokens: u64,
    }

    impl Harness {
        fn new() -> Self {
            Harness {
                s: FundState::new(),
                records: vec![],
                c: cfg(),
                usdc_tokens: 0,
                btc_tokens: 0,
            }
        }

        fn seeded(price: u64) -> Self {
            let mut h = Self::new();
            seed_prices(&mut h.s, &vec![price; 49], &h.c).unwrap();
            h
        }

        fn subscribe(&mut self, t: &mut Ticket, amount: u64, now: i64) {
            request_subscription(&mut self.s, t, amount, now, &self.c).unwrap();
            self.usdc_tokens += amount;
        }

        fn redeem(&mut self, t: &mut Ticket, shares: u64, now: i64) {
            request_redemption(&mut self.s, t, shares, now, &self.c).unwrap();
        }

        fn fix_with(&mut self, now: i64, p: &FixPrices) -> Result<EpochRecord, FundError> {
            self.fix_as(now, p, Caller::Keeper)
        }

        fn fix_as(
            &mut self,
            now: i64,
            p: &FixPrices,
            caller: Caller,
        ) -> Result<EpochRecord, FundError> {
            let r = fix(&mut self.s, now, p, caller, &self.c)?;
            assert_eq!(r.epoch as usize, self.records.len());
            self.records.push(r);
            // 赎回资产按比例向下取整、发行份额向下取整：留下的人的净值不会因为别人的申购赎回下降
            // （误差不超过 2 个 USDC 原始单位分摊到全部份额）
            if self.s.share_supply > 0 {
                let after = nav(&self.s, p.valuation.price).unwrap();
                let tolerance = 2 * NAV_SCALE / self.s.share_supply as u128 + 1;
                assert!(after + tolerance >= r.nav, "净值 {} -> {after}", r.nav);
            }
            self.assert_ledger();
            Ok(r)
        }

        /// 基金名下的代币 = 基金资产 + 排队的申购款 + 未卖完的赎回资产 + 待领取的赎回款
        fn assert_ledger(&self) {
            let slices = &self.s.slices[..self.s.slice_count as usize];
            let payouts: u64 = self
                .records
                .iter()
                .filter(|r| r.slice_sold_out)
                .map(|r| r.unclaimed_payout_usdc)
                .sum();
            assert_eq!(
                self.usdc_tokens,
                self.s.usdc
                    + self.s.queued_subscription_usdc
                    + slices.iter().map(|x| x.usdc).sum::<u64>()
                    + payouts
            );
            assert_eq!(
                self.btc_tokens,
                self.s.btc + slices.iter().map(|x| x.btc).sum::<u64>()
            );
        }

        /// 当天 1 点定价，两个价格相同
        fn fix_day(&mut self, day: i64, price: u64) -> EpochRecord {
            self.fix_with(day * DAY + ONE_AM, &at_fix(day, price))
                .unwrap()
        }

        fn claim(&mut self, t: &mut Ticket) -> Result<(u64, u64), FundError> {
            let r = &mut self.records[t.epoch as usize];
            let (shares, usdc) = claim(t, r)?;
            self.usdc_tokens -= usdc;
            Ok((shares, usdc))
        }

        /// 成交一笔计划好的交易；卖出赎回资产时传入那一期的记录
        fn fill(&mut self, now: i64, plan: &TradePlan, got: u64) -> Result<(), FundError> {
            let r = match plan.bucket {
                TradeBucket::Slice { epoch } => Some(&mut self.records[epoch as usize]),
                TradeBucket::Fund => None,
            };
            fill_trade(&mut self.s, plan, got, now, r)?;
            match plan.side {
                Side::BuyBtc => {
                    self.usdc_tokens -= plan.amount_in;
                    self.btc_tokens += got;
                }
                Side::SellBtc => {
                    self.btc_tokens -= plan.amount_in;
                    self.usdc_tokens += got;
                }
            }
            self.assert_ledger();
            Ok(())
        }

        /// 在 now 规划并成交一笔交易：交易所按最新估值价格成交，收 fee_bps
        fn trade_at(
            &mut self,
            now: i64,
            p: MarketPrices,
            fee_bps: u16,
        ) -> Result<TradePlan, FundError> {
            self.trade_as(now, p, Caller::Keeper, fee_bps)
        }

        /// 以 caller 的身份规划并成交一笔交易
        fn trade_as(
            &mut self,
            now: i64,
            p: MarketPrices,
            caller: Caller,
            fee_bps: u16,
        ) -> Result<TradePlan, FundError> {
            let plan = plan_trade(&self.s, now, &p, caller, &self.c)?;
            self.fill(now, &plan, exec(&plan, p.valuation, fee_bps))?;
            Ok(plan)
        }

        /// 从 start 起每隔最小间隔交易一笔，价格不变，直到没有可交易的。
        /// 返回所有成交的计划、停下的原因和最后一笔交易之后的时间。
        fn trade_until_done(
            &mut self,
            start: i64,
            p: MarketPrices,
            fee_bps: u16,
        ) -> (Vec<TradePlan>, FundError, i64) {
            let mut now = start;
            let mut plans = vec![];
            loop {
                match self.trade_at(now, p, fee_bps) {
                    Ok(plan) => plans.push(plan),
                    Err(e) => return (plans, e, now),
                }
                now += self.c.min_trade_interval_secs;
            }
        }

        /// 定价并交易到完成（两个价格相同），返回本期记录的最新状态
        fn settle_day(&mut self, day: i64, price: u64, fee_bps: u16) -> EpochRecord {
            let r = self.fix_day(day, price);
            let (_, why, _) = self.trade_until_done(day * DAY + ONE_AM, same(price), fee_bps);
            assert_eq!(why, FundError::NothingToTrade);
            self.records[r.epoch as usize]
        }
    }

    fn same(price: u64) -> MarketPrices {
        MarketPrices {
            signal: price,
            valuation: price,
        }
    }

    /// 测试用的模拟交易所：按价格成交，收取 fee_bps
    fn exec(plan: &TradePlan, price: u64, fee_bps: u16) -> u64 {
        market_fill(plan, price, fee_bps).unwrap()
    }

    #[test]
    fn first_subscription_is_one_to_one() {
        let mut h = Harness::new();
        let mut t = Ticket::default();
        h.subscribe(&mut t, 100 * USDC, 100 * DAY);
        let r = h.fix_day(100, 60_000 * USDC);
        assert_eq!(r.minted_shares, 100 * USDC);
        assert_eq!(h.claim(&mut t).unwrap(), (100 * USDC, 0));
        assert_eq!(h.s.share_supply, 100 * USDC);
        assert_eq!(h.s.usdc, 100 * USDC);
        assert!(!h.s.target_btc, "价格不足 50 个，应持有现金");
    }

    #[test]
    fn cannot_fix_before_1am_or_twice() {
        let mut h = Harness::new();
        let fix_at = 100 * DAY + ONE_AM;
        assert_eq!(
            h.fix_with(fix_at - 1, &at_fix(100, USDC)),
            Err(FundError::TooEarly)
        );
        h.fix_with(fix_at, &at_fix(100, USDC)).unwrap();
        assert_eq!(
            h.fix_with(100 * DAY + 20 * 3600, &at_fix(100, USDC)),
            Err(FundError::AlreadySettled)
        );
        assert!(h.fix_with(101 * DAY + ONE_AM, &at_fix(101, USDC)).is_ok());
    }

    #[test]
    fn redemption_from_all_usdc_fund_is_paid_its_slice_at_fix() {
        // 基金全部是 USDC：赎回资产只有 USDC，定价时就完成，按比例领取
        let mut h = Harness::new();
        let (mut ta, mut tb) = (Ticket::default(), Ticket::default());
        h.subscribe(&mut ta, 1_000 * USDC, 10 * DAY);
        h.subscribe(&mut tb, 3_000 * USDC, 10 * DAY);
        h.fix_day(10, 50_000 * USDC);
        let (a, _) = h.claim(&mut ta).unwrap();
        h.claim(&mut tb).unwrap();

        h.redeem(&mut ta, a / 2, 11 * DAY);
        let r = h.fix_day(11, 50_000 * USDC);
        assert!(r.slice_sold_out);
        assert_eq!((r.slice_usdc, r.slice_btc), (500 * USDC, 0));
        assert_eq!(h.claim(&mut ta).unwrap(), (0, 500 * USDC));
        assert_eq!(h.s.usdc, 3_500 * USDC);
        assert_eq!(h.s.share_supply, 3_500 * USDC);
    }

    #[test]
    fn fees_stay_in_fund() {
        let mut h = Harness::new();
        h.c.subscription_fee_bps = 100;
        let mut ta = Ticket::default();
        h.subscribe(&mut ta, 1_000 * USDC, 10 * DAY);
        let r = h.fix_day(10, 50_000 * USDC);
        // 第一个人：1% 申购费留在基金里，他自己的份额净值随之提高
        assert_eq!(r.minted_shares, 990 * USDC);
        assert_eq!(h.s.usdc, 1_000 * USDC);
        assert!(nav(&h.s, 50_000 * USDC).unwrap() > NAV_SCALE);

        // 赎回费同样留在基金里：赎回一半份额，拿到的少于一半资产
        h.c.redemption_fee_bps = 100;
        h.claim(&mut ta).unwrap();
        h.redeem(&mut ta, 495 * USDC, 11 * DAY);
        let r = h.fix_day(11, 50_000 * USDC);
        assert_eq!(r.slice_usdc, 495 * USDC);
        assert_eq!(h.s.usdc, 505 * USDC);
    }

    #[test]
    fn claim_requires_settled_epoch_and_only_once() {
        let mut h = Harness::new();
        let mut t = Ticket::default();
        h.subscribe(&mut t, 5 * USDC, 1 * DAY);
        let r = h.fix_day(1, 1 * USDC);
        let mut wrong = r;
        wrong.epoch = 99;
        assert_eq!(
            claim(&mut t.clone(), &mut wrong),
            Err(FundError::EpochNotSettled)
        );
        h.claim(&mut t).unwrap();
        assert_eq!(h.claim(&mut t), Err(FundError::NothingToClaim));
    }

    #[test]
    fn min_subscription_enforced() {
        let mut s = FundState::new();
        let mut t = Ticket::default();
        assert_eq!(
            request_subscription(&mut s, &mut t, USDC - 1, 1 * DAY, &cfg()),
            Err(FundError::BelowMinSubscription)
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

    #[test]
    fn fix_price_must_be_published_in_fix_window() {
        let s = FundState::new();
        let fix_at = 10 * DAY + ONE_AM;
        let now = fix_at + 5 * 3600;
        // 两个价格的发布时间分别设置
        let at = |ts: i64, tv: i64| FixPrices {
            signal: PricePoint {
                price: USDC,
                publish_time: ts,
            },
            valuation: PricePoint {
                price: USDC,
                publish_time: tv,
            },
        };
        let run_both = |now: i64, ts: i64, tv: i64| {
            fix(&mut s.clone(), now, &at(ts, tv), Caller::Keeper, &cfg()).map(|_| ())
        };
        let run = |now: i64, t: i64| run_both(now, t, t);
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
        // 两个价格都必须在窗口内：只有一个在窗口内也不行
        assert_eq!(run_both(now, fix_at, now), bad);
        assert_eq!(run_both(now, now, fix_at), bad);
    }

    #[test]
    fn queue_closed_between_fix_time_and_fixing_step() {
        let mut h = Harness::seeded(50_000 * USDC);
        let fix_at = 10 * DAY + ONE_AM;
        let mut ta = Ticket::default();
        h.subscribe(&mut ta, 100 * USDC, fix_at - 1); // 00:59:59 还开放

        // 定价价格公开之后，到当天定价步骤之前，申购赎回都关闭
        let mut tb = Ticket::default();
        let closed = Err(FundError::QueueClosed);
        assert_eq!(
            request_subscription(&mut h.s, &mut tb, 100 * USDC, fix_at, &cfg()),
            closed
        );
        assert_eq!(
            request_redemption(&mut h.s, &mut tb, 1, fix_at + 12 * 3600, &cfg()),
            closed
        );
        assert_eq!(tb, Ticket::default());

        // 定价之后重新开放，进入下一期
        let r = h.fix_day(10, 51_000 * USDC);
        h.claim(&mut ta).unwrap();
        h.subscribe(&mut tb, 100 * USDC, fix_at + 60);
        assert_eq!(tb.epoch, r.epoch + 1);

        // 某天没人定价：定价时刻之后一直关闭，第二天 0 点重新开放
        let mut tc = Ticket::default();
        assert_eq!(
            request_subscription(&mut h.s, &mut tc, 100 * USDC, 11 * DAY + ONE_AM, &cfg()),
            closed
        );
        assert_eq!(
            request_subscription(&mut h.s, &mut tc, 100 * USDC, 12 * DAY - 1, &cfg()),
            closed
        );
        h.subscribe(&mut tc, 100 * USDC, 12 * DAY);
    }

    #[test]
    fn config_validation() {
        let bad = [
            Config { window: 0, ..cfg() },
            Config {
                window: 65,
                ..cfg()
            },
            Config {
                settle_offset_secs: 86_400,
                ..cfg()
            },
            Config {
                max_slippage_bps: 10_000,
                ..cfg()
            },
            Config {
                depeg_threshold_bps: 0,
                ..cfg()
            },
            Config {
                depeg_threshold_bps: 10_000,
                ..cfg()
            },
            Config {
                trade_cap_usdc: 0,
                ..cfg()
            },
            Config {
                min_trade_interval_secs: -1,
                ..cfg()
            },
            Config {
                keeper_window_secs: -1,
                ..cfg()
            },
            Config {
                keeper_window_secs: SECONDS_PER_DAY,
                ..cfg()
            },
            // 清算窗口不能跨过午夜：窗口只按当天的定价时刻判断
            Config {
                settle_offset_secs: 23 * 3_600 + 50 * 60,
                ..cfg()
            },
            // 公开滑点上限必须不比清算地址的宽
            Config {
                public_slippage_bps: 31,
                ..cfg()
            },
        ];
        for c in bad {
            assert_eq!(c.validate(), Err(FundError::InvalidConfig), "{c:?}");
        }
        assert!(cfg().validate().is_ok());
        assert_eq!(cfg().trade_cap_usdc, 200_000 * USDC);
        assert_eq!(cfg().min_trade_interval_secs, 60);
        assert_eq!(cfg().keeper_window_secs, 1_800);
        assert_eq!(cfg().public_slippage_bps, 10);
    }

    #[test]
    fn signal_price_drives_average_and_valuation_price_drives_nav() {
        let mut h = Harness::seeded(50_000 * USDC);
        let mut t = Ticket::default();
        h.subscribe(&mut t, 1_000 * USDC, 10 * DAY);
        h.fix_day(10, 51_000 * USDC);
        h.claim(&mut t).unwrap();
        // 基金一半 USDC、一半 cbBTC（直接设置，代币账户同步）
        (h.s.usdc, h.usdc_tokens) = (500 * USDC, 500 * USDC);
        (h.s.btc, h.btc_tokens) = (1_000_000, 1_000_000); // 0.01 cbBTC
        let before = h.s.clone();

        // BTC/USD 52,000，cbBTC/USD 51,500（约低 1%，未脱锚）
        let r2 = h
            .fix_with(
                11 * DAY + ONE_AM,
                &at_fix2(11, 52_000 * USDC, 51_500 * USDC),
            )
            .unwrap();
        assert_eq!(r2.signal_price, 52_000 * USDC);
        assert_eq!(r2.valuation_price, 51_500 * USDC);
        // 净值按基金实际持有的 cbBTC 估值：500 + 515 = 1015 USDC / 1000 份
        assert_eq!(r2.nav, nav(&before, 51_500 * USDC).unwrap());
        assert_eq!(r2.nav, NAV_SCALE * 1_015 / 1_000);
        // 均线只记录信号价格：48 × 50,000 + 51,000 + 52,000
        assert_eq!(
            r2.moving_average,
            (48 * 50_000 + 51_000 + 52_000) * USDC / 50
        );
        assert!(r2.target_btc && h.s.target_btc);
    }

    #[test]
    fn switches_to_btc_above_ma_and_back_to_cash_below() {
        let p0 = 50_000 * USDC;
        let mut h = Harness::seeded(p0);
        let mut t = Ticket::default();
        h.subscribe(&mut t, 1_000 * USDC, 10 * DAY);
        // 第 50 个价格高于前 49 个 -> 高于均线 -> 买入
        let r = h.settle_day(10, 55_000 * USDC, 0);
        assert!(r.target_btc && h.s.target_btc);
        assert_eq!(h.s.usdc, 0);
        assert!(h.s.btc > 0);
        h.claim(&mut t).unwrap();
        // 价格跌回均线下方 -> 全部卖出
        let r2 = h.settle_day(11, 45_000 * USDC, 0);
        assert!(!r2.target_btc);
        assert_eq!(h.s.btc, 0);
        // 1000 USDC 以 55000 买入，以 45000 卖出 -> 约 818 USDC
        let expect = 1_000.0 * 45_000.0 / 55_000.0;
        let got = h.s.usdc as f64 / USDC as f64;
        assert!((got - expect).abs() < 0.01, "got {got}, expect {expect}");
    }

    #[test]
    fn late_subscriber_cannot_use_stale_nav() {
        // 老持有人 A 在 BTC 里；B 在价格大涨之后才申购，必须按大涨后的净值成交
        let mut h = Harness::seeded(50_000 * USDC);
        let mut ta = Ticket::default();
        h.subscribe(&mut ta, 1_000 * USDC, 10 * DAY);
        h.settle_day(10, 51_000 * USDC, 0); // 买入 BTC
        h.claim(&mut ta).unwrap();
        let a_shares = h.s.share_supply;

        let mut tb = Ticket::default();
        h.subscribe(&mut tb, 1_000 * USDC, 11 * DAY);
        // 第二天 BTC 涨 20%
        h.settle_day(11, 61_200 * USDC, 0);
        let (b_shares, _) = h.claim(&mut tb).unwrap();
        // B 拿到的份额应约为 A 的 1/1.2（价格涨了 20%，净值也涨了 20%）
        let ratio = b_shares as f64 / a_shares as f64;
        assert!((ratio - 1.0 / 1.2).abs() < 1e-4, "ratio {ratio}");
        // A 的价值不被稀释：A 的份额 × 净值 ≈ 1000 × 1.2
        let a_value =
            (a_shares as u128 * nav(&h.s, 61_200 * USDC).unwrap() / NAV_SCALE) as f64 / USDC as f64;
        assert!((a_value - 1_200.0).abs() < 0.05, "a_value {a_value}");
    }

    #[test]
    fn rebalance_is_split_into_capped_trades_spaced_by_the_interval() {
        let mut h = Harness::seeded(50_000 * USDC);
        h.c.trade_cap_usdc = 1_000 * USDC;
        let mut t = Ticket::default();
        h.subscribe(&mut t, 2_500 * USDC, 10 * DAY);
        h.fix_day(10, 51_000 * USDC);
        let t0 = 10 * DAY + ONE_AM;
        let p = same(51_000 * USDC);

        let first = h.trade_at(t0, p, 0).unwrap();
        assert_eq!(
            (first.side, first.amount_in, first.bucket),
            (Side::BuyBtc, 1_000 * USDC, TradeBucket::Fund)
        );
        assert_eq!(h.trade_at(t0 + 59, p, 0), Err(FundError::TradeTooSoon));
        assert_eq!(h.trade_at(t0 + 60, p, 0).unwrap().amount_in, 1_000 * USDC);
        assert_eq!(h.trade_at(t0 + 120, p, 0).unwrap().amount_in, 500 * USDC);
        // 没有剩余时直接回答“没有可交易的”，不必等间隔
        assert_eq!(h.trade_at(t0 + 121, p, 0), Err(FundError::NothingToTrade));
        assert_eq!(
            h.trade_at(t0 + DAY - 1, p, 0),
            Err(FundError::NothingToTrade)
        );
        // 1000 USDC @ 51,000 = 1,960,784 sat（两次），500 USDC = 980,392 sat
        assert_eq!((h.s.usdc, h.s.btc), (0, 4_901_960));
    }

    #[test]
    fn only_the_keeper_drives_settlement_inside_the_keeper_window() {
        let mut h = Harness::seeded(50_000 * USDC);
        h.c.trade_cap_usdc = 1_000 * USDC; // 3 笔才能买完
        let mut t = Ticket::default();
        h.subscribe(&mut t, 3_000 * USDC, 10 * DAY);
        let fix_at = 10 * DAY + ONE_AM;
        let window_end = fix_at + h.c.keeper_window_secs;
        let p = at_fix(10, 51_000 * USDC);
        // 窗口内（01:00–01:30）其他人不能定价
        assert_eq!(
            fix(&mut h.s.clone(), fix_at + 10, &p, Caller::Public, &h.c),
            Err(FundError::KeeperOnly)
        );
        assert_eq!(
            fix(&mut h.s.clone(), window_end - 1, &p, Caller::Public, &h.c),
            Err(FundError::KeeperOnly)
        );
        // 窗口之后谁都可以（定价价格仍然是 1 点发布的）
        assert!(fix(&mut h.s.clone(), window_end, &p, Caller::Public, &h.c).is_ok());
        h.fix_with(fix_at + 10, &p).unwrap(); // 清算地址

        // 交易同理
        let m = same(51_000 * USDC);
        assert_eq!(
            h.trade_as(fix_at + 60, m, Caller::Public, 0),
            Err(FundError::KeeperOnly)
        );
        h.trade_as(window_end, m, Caller::Public, 0).unwrap();
        // 窗口之外的其他时刻也不受限制
        h.trade_as(10 * DAY + 20 * 3600, m, Caller::Public, 0)
            .unwrap();
    }

    #[test]
    fn public_trades_use_the_tighter_slippage_bound() {
        let mut h = Harness::seeded(50_000 * USDC);
        let mut t = Ticket::default();
        h.subscribe(&mut t, 1_000 * USDC, 10 * DAY);
        h.fix_day(10, 51_000 * USDC);
        let now = 10 * DAY + 2 * ONE_AM; // 窗口之后
        let p = same(50_000 * USDC);
        // 1000 USDC @ 50,000 = 2,000,000 sat；清算地址减 0.3%，其他人减 0.1%
        let keeper = plan_trade(&h.s, now, &p, Caller::Keeper, &h.c).unwrap();
        let public = plan_trade(&h.s, now, &p, Caller::Public, &h.c).unwrap();
        assert_eq!((keeper.min_out, public.min_out), (1_994_000, 1_998_000));
        // 交易所收 0.18%：清算地址能成交，其他人的这一笔回滚
        assert_eq!(
            h.trade_as(now, p, Caller::Public, 18),
            Err(FundError::SlippageExceeded)
        );
        h.trade_as(now, p, Caller::Keeper, 18).unwrap();
    }

    #[test]
    fn trade_min_out_uses_the_valuation_price_at_execution() {
        let mut h = Harness::seeded(50_000 * USDC);
        let mut t = Ticket::default();
        h.subscribe(&mut t, 1_000 * USDC, 10 * DAY);
        h.fix_day(10, 51_000 * USDC);
        // 晚上 9 点才交易，这时 cbBTC 是 52,500（BTC/USD 52,600，未脱锚）
        let now = 10 * DAY + 21 * 3600;
        let p = MarketPrices {
            signal: 52_600 * USDC,
            valuation: 52_500 * USDC,
        };
        let plan = plan_trade(&h.s, now, &p, Caller::Keeper, &h.c).unwrap();
        // 1000 USDC @ 52,500 = 1,904,761 sat，减 0.3% = 1,899,046
        assert_eq!(plan.min_out, 1_899_046);
    }

    #[test]
    fn fill_below_min_out_reverts_that_trade_only() {
        let mut h = Harness::seeded(50_000 * USDC);
        let mut t = Ticket::default();
        h.subscribe(&mut t, 1_000 * USDC, 10 * DAY);
        h.fix_day(10, 51_000 * USDC);
        let t0 = 10 * DAY + ONE_AM;
        let p = same(51_000 * USDC);
        let plan = plan_trade(&h.s, t0, &p, Caller::Keeper, &h.c).unwrap();
        let before = h.s.clone();
        assert_eq!(
            h.fill(t0, &plan, plan.min_out - 1),
            Err(FundError::SlippageExceeded)
        );
        assert_eq!(h.s, before, "失败的交易不改变状态，也不占用交易间隔");
        assert_eq!(
            h.trade_at(t0, p, 40),
            Err(FundError::SlippageExceeded),
            "0.4% 超过 0.3% 上限"
        );
        // 下一次尝试照常成交
        h.trade_at(t0, p, 18).unwrap();
        assert!(h.s.btc > 0);
    }

    #[test]
    fn depeg_blocks_buys_but_not_sells_or_fixing() {
        let mut h = Harness::seeded(50_000 * USDC);
        let mut t = Ticket::default();
        h.subscribe(&mut t, 1_000 * USDC, 10 * DAY);
        // 信号要求持有 cbBTC，但 cbBTC 比 BTC 低 3%：定价照常，买入被拒绝
        let r = h
            .fix_with(
                10 * DAY + ONE_AM,
                &at_fix2(10, 51_000 * USDC, 49_470 * USDC),
            )
            .unwrap();
        assert!(r.target_btc, "目标仍是 cbBTC");
        let t0 = 10 * DAY + ONE_AM;
        let depegged = MarketPrices {
            signal: 51_000 * USDC,
            valuation: 49_470 * USDC,
        };
        assert_eq!(h.trade_at(t0, depegged, 0), Err(FundError::DepegBlocksBuy));
        assert_eq!((h.s.usdc, h.s.btc), (1_000 * USDC, 0));
        assert_eq!(h.claim(&mut t).unwrap(), (1_000 * USDC, 0));

        // 每笔交易前都检查：当天恢复锚定后照常买入
        let plan = h.trade_at(t0 + 3_600, same(51_000 * USDC), 0).unwrap();
        assert_eq!(plan.side, Side::BuyBtc);

        // 脱锚期间卖出不受影响：持有人赎回一半，信号跌破均线
        h.redeem(&mut t, 500 * USDC, 11 * DAY);
        h.fix_with(
            11 * DAY + ONE_AM,
            &at_fix2(11, 40_000 * USDC, 38_000 * USDC),
        )
        .unwrap();
        let depegged = MarketPrices {
            signal: 40_000 * USDC,
            valuation: 38_000 * USDC,
        };
        let (plans, why, _) = h.trade_until_done(11 * DAY + ONE_AM, depegged, 0);
        assert_eq!(why, FundError::NothingToTrade);
        let buckets: Vec<_> = plans.iter().map(|p| (p.side, p.bucket)).collect();
        assert_eq!(
            buckets,
            [
                (Side::SellBtc, TradeBucket::Slice { epoch: 1 }),
                (Side::SellBtc, TradeBucket::Fund)
            ]
        );
        assert_eq!(h.s.btc, 0);
        assert!(h.claim(&mut t).unwrap().1 > 0);
    }

    #[test]
    fn fund_trades_wait_for_the_fixing_step_but_slices_keep_selling() {
        // 定价时刻之后、当天定价之前，基金调仓暂停：否则这段时间的成交会混进当天的定价净值，
        // 而当天的申购赎回按这个净值成交。赎回资产不算基金资产，照常卖出。
        let mut h = Harness::seeded(50_000 * USDC);
        h.c.trade_cap_usdc = 1_000 * USDC;
        let (mut ta, mut tb) = (Ticket::default(), Ticket::default());
        h.subscribe(&mut ta, 1_000 * USDC, 10 * DAY);
        h.subscribe(&mut tb, 3_000 * USDC, 10 * DAY);
        h.fix_day(10, 51_000 * USDC);
        h.trade_at(10 * DAY + ONE_AM, same(51_000 * USDC), 0)
            .unwrap();
        h.claim(&mut ta).unwrap();
        h.redeem(&mut ta, 1_000 * USDC, 10 * DAY + 2 * ONE_AM);
        // 第 11 天 0 点到 1 点之间，前一天的调仓照常继续
        let p = same(51_000 * USDC);
        assert_eq!(
            h.trade_at(11 * DAY, p, 0).unwrap().bucket,
            TradeBucket::Fund
        );
        // 1 点之后、定价之前：基金调仓等待定价
        let fix_at = 11 * DAY + ONE_AM;
        assert_eq!(h.trade_at(fix_at, p, 0), Err(FundError::FixPending));
        assert_eq!(
            h.trade_at(fix_at + 5 * 3600, p, 0),
            Err(FundError::FixPending)
        );
        // 定价之后：先卖 A 的赎回资产，然后基金调仓恢复
        h.fix_with(fix_at + 5 * 3600, &at_fix(11, 51_000 * USDC))
            .unwrap();
        let (plans, why, _) = h.trade_until_done(fix_at + 5 * 3600, p, 0);
        assert_eq!(why, FundError::NothingToTrade);
        assert_eq!(plans[0].bucket, TradeBucket::Slice { epoch: 1 });
        assert_eq!(plans[1].bucket, TradeBucket::Fund);

        // 未卖完的赎回资产在定价之前也照常卖出
        h.claim(&mut ta).unwrap();
        h.claim(&mut tb).unwrap();
        h.redeem(&mut tb, 1_000 * USDC, 12 * DAY);
        h.fix_day(12, 51_000 * USDC);
        let fix_at = 13 * DAY + ONE_AM;
        let plan = h.trade_at(fix_at + 60, p, 0).unwrap();
        assert_eq!(plan.bucket, TradeBucket::Slice { epoch: 2 });
    }

    #[test]
    fn redemption_slice_is_paid_its_actual_proceeds_once_sold_out() {
        let mut h = Harness::seeded(50_000 * USDC);
        h.c.trade_cap_usdc = 1_000 * USDC;
        let (mut ta, mut tb) = (Ticket::default(), Ticket::default());
        h.subscribe(&mut ta, 1_000 * USDC, 10 * DAY);
        h.subscribe(&mut tb, 2_000 * USDC, 10 * DAY);
        h.settle_day(10, 51_000 * USDC, 0); // 3 笔，每笔 1000 USDC = 1,960,784 sat
        h.claim(&mut ta).unwrap();
        h.claim(&mut tb).unwrap();
        assert_eq!((h.s.usdc, h.s.btc), (0, 5_882_352));

        // A 赎回全部 1000 份，B 赎回 2000 份中的 1000 份：赎回资产是 2/3 的 cbBTC
        h.redeem(&mut ta, 1_000 * USDC, 11 * DAY);
        h.redeem(&mut tb, 1_000 * USDC, 11 * DAY);
        let r = h.fix_day(11, 51_000 * USDC);
        assert_eq!((r.slice_usdc, r.slice_btc), (0, 3_921_568));
        assert!(!r.slice_sold_out);
        assert_eq!(h.s.btc, 1_960_784, "留下的 cbBTC 不变");
        assert_eq!(h.s.share_supply, 1_000 * USDC);

        // 两笔卖出，第二笔时价格跌到 50,000
        let t0 = 11 * DAY + ONE_AM;
        let first = h.trade_at(t0, same(51_000 * USDC), 0).unwrap();
        assert_eq!(first.bucket, TradeBucket::Slice { epoch: 1 });
        assert_eq!(first.amount_in, 1_960_784);
        assert_eq!(h.claim(&mut ta), Err(FundError::SliceNotSold));
        h.trade_at(t0 + 60, same(50_000 * USDC), 0).unwrap();
        // 1,960,784 sat × 51,000 + 1,960,784 sat × 50,000
        let proceeds = 999_999_840 + 980_392_000;
        let r = h.records[1];
        assert!(r.slice_sold_out);
        assert_eq!(r.payout_usdc, proceeds);

        let (_, a) = h.claim(&mut ta).unwrap();
        let (_, b) = h.claim(&mut tb).unwrap();
        assert_eq!((a, b), (proceeds / 2, proceeds / 2));
        assert_eq!(h.records[1].unclaimed_payout_usdc, 0);
        // 卖出所得不进入基金：基金还是只有留下的 cbBTC
        assert_eq!((h.s.usdc, h.s.btc), (0, 1_960_784));
        assert_eq!(
            h.trade_at(t0 + 120, same(52_000 * USDC), 0),
            Err(FundError::NothingToTrade)
        );
    }

    #[test]
    fn unsold_slices_keep_priority_across_the_next_fixing() {
        let mut h = Harness::seeded(50_000 * USDC);
        h.c.trade_cap_usdc = 1_000 * USDC;
        let (mut ta, mut tb) = (Ticket::default(), Ticket::default());
        h.subscribe(&mut ta, 1_000 * USDC, 10 * DAY);
        h.subscribe(&mut tb, 2_000 * USDC, 10 * DAY);
        h.settle_day(10, 51_000 * USDC, 0);
        h.claim(&mut ta).unwrap();
        h.claim(&mut tb).unwrap();

        // 第 11 天：A 全部赎回，但当天没有成交（比如流动性不足）
        h.redeem(&mut ta, 1_000 * USDC, 11 * DAY);
        let r1 = h.fix_day(11, 51_000 * USDC);
        assert_eq!(r1.slice_btc, 1_960_784);

        // 第 12 天：B 赎回一半，价格跌破均线，目标变成 USDC
        h.redeem(&mut tb, 1_000 * USDC, 12 * DAY);
        let r2 = h.fix_day(12, 40_000 * USDC);
        assert!(!r2.target_btc);
        // 净值只按基金资产计算，A 那份没卖完的 cbBTC 不算在内：
        // 3,921,568 sat × 40,000 = 1,568.6272 USDC / 2000 份
        assert_eq!(r2.nav, 1_568_627_200 * NAV_SCALE / (2_000 * USDC as u128));
        assert_eq!(r2.slice_btc, 1_960_784);

        // 先卖第 11 天的，再卖第 12 天的，最后才是基金调仓
        let (plans, why, _) = h.trade_until_done(12 * DAY + ONE_AM, same(40_000 * USDC), 0);
        assert_eq!(why, FundError::NothingToTrade);
        let buckets: Vec<_> = plans.iter().map(|p| p.bucket).collect();
        assert_eq!(
            buckets,
            [
                TradeBucket::Slice { epoch: 1 },
                TradeBucket::Slice { epoch: 2 },
                TradeBucket::Fund
            ]
        );
        // 1,960,784 sat × 40,000 = 784.3136 USDC，各自拿到自己那份的卖出所得
        assert_eq!(h.claim(&mut ta).unwrap(), (0, 784_313_600));
        assert_eq!(h.claim(&mut tb).unwrap(), (0, 784_313_600));
        assert_eq!((h.s.usdc, h.s.btc), (784_313_600, 0));
    }

    #[test]
    fn unfinished_rebalance_is_recomputed_at_the_next_fixing() {
        let mut h = Harness::seeded(50_000 * USDC);
        h.c.trade_cap_usdc = 1_000 * USDC;
        let mut t = Ticket::default();
        h.subscribe(&mut t, 3_000 * USDC, 10 * DAY);
        h.fix_day(10, 51_000 * USDC);
        // 当天只成交了一笔
        h.trade_at(10 * DAY + ONE_AM, same(51_000 * USDC), 0)
            .unwrap();
        assert_eq!((h.s.usdc, h.s.btc), (2_000 * USDC, 1_960_784));

        // 第二天目标变成 USDC：不再买剩下的，而是卖掉已经买的
        h.fix_day(11, 40_000 * USDC);
        let (plans, why, _) = h.trade_until_done(11 * DAY + ONE_AM, same(40_000 * USDC), 0);
        assert_eq!(why, FundError::NothingToTrade);
        assert_eq!(plans.len(), 1);
        assert_eq!(
            (plans[0].side, plans[0].amount_in),
            (Side::SellBtc, 1_960_784)
        );
        assert_eq!(h.s.btc, 0);
    }

    #[test]
    fn full_exit_while_holding_cbbtc_completes_over_several_trades() {
        let mut h = Harness::seeded(50_000 * USDC);
        let (mut ta, mut tb) = (Ticket::default(), Ticket::default());
        h.subscribe(&mut ta, 500_000 * USDC, 10 * DAY);
        h.subscribe(&mut tb, 500_000 * USDC, 10 * DAY);
        h.settle_day(10, 51_000 * USDC, 18);
        let (a, _) = h.claim(&mut ta).unwrap();
        let (b, _) = h.claim(&mut tb).unwrap();
        assert_eq!(h.s.usdc, 0);

        // 两人同时全部赎回，信号仍为持有 cbBTC，交易所收 0.18%
        h.redeem(&mut ta, a, 11 * DAY);
        h.redeem(&mut tb, b, 11 * DAY);
        h.fix_day(11, 52_000 * USDC);
        assert_eq!(h.s.share_supply, 0);
        let (plans, why, _) = h.trade_until_done(11 * DAY + ONE_AM, same(52_000 * USDC), 18);
        assert_eq!(why, FundError::NothingToTrade);
        // 约 1,017,000 USDC 的 cbBTC，每笔最多 200,000
        assert_eq!(plans.len(), 6);
        assert!(plans
            .iter()
            .all(|p| p.bucket == TradeBucket::Slice { epoch: 1 }
                && btc_value(p.amount_in, 52_000 * USDC).unwrap() <= 200_000 * USDC));
        let (_, ua) = h.claim(&mut ta).unwrap();
        let (_, ub) = h.claim(&mut tb).unwrap();
        assert_eq!(ua + ub, h.records[1].payout_usdc);
        assert!(ua.abs_diff(ub) <= 1);
        assert_eq!((h.s.usdc, h.s.btc), (0, 0));
        // 基金还能继续运作
        let mut tc = Ticket::default();
        h.subscribe(&mut tc, 10 * USDC, 12 * DAY);
        h.settle_day(12, 53_000 * USDC, 18);
        assert_eq!(h.claim(&mut tc).unwrap(), (10 * USDC, 0));
    }

    #[test]
    fn near_full_exit_with_costs_never_sells_the_stayers_btc() {
        // A 持有 99.8%，B 持有 0.2%；A 全部赎回时价格比定价低 1%，交易所收 0.25%。
        // 成交成本只由 A 的赎回资产承担，B 那一份 cbBTC 原样留在基金里。
        let mut h = Harness::seeded(50_000 * USDC);
        let (mut ta, mut tb) = (Ticket::default(), Ticket::default());
        h.subscribe(&mut ta, 3_000 * USDC, 10 * DAY);
        h.subscribe(&mut tb, 5 * USDC, 10 * DAY);
        h.settle_day(10, 51_000 * USDC, 0);
        let (a, _) = h.claim(&mut ta).unwrap();
        let (b, _) = h.claim(&mut tb).unwrap();
        let btc_before = h.s.btc;

        h.redeem(&mut ta, a, 11 * DAY);
        let fix = 52_000 * USDC;
        h.fix_day(11, fix);
        let (_, why, _) = h.trade_until_done(11 * DAY + ONE_AM, same(fix / 100 * 99), 25);
        assert_eq!(why, FundError::NothingToTrade);
        let (_, a_usdc) = h.claim(&mut ta).unwrap();
        assert_eq!(a_usdc, h.records[1].payout_usdc);

        // 按份额比例，向下取整对留下的人有利
        let b_btc = btc_before - (btc_before as u128 * a as u128 / (a + b) as u128) as u64;
        assert_eq!(h.s.btc, b_btc);
        assert_eq!(h.s.share_supply, b);
        h.settle_day(12, 53_000 * USDC, 25);
    }

    #[test]
    fn dust_redemption_while_fully_in_btc_does_not_get_stuck() {
        // 基金全部在 cbBTC 里，有人只赎回 100 个份额原始单位：对应的 cbBTC 向下取整为 0，
        // 赎回资产当场完成（价值为 0），不会留下卖不出去的赎回资产。
        let mut h = Harness::seeded(50_000 * USDC);
        let mut t = Ticket::default();
        h.subscribe(&mut t, 50_000 * USDC, 10 * DAY);
        h.settle_day(10, 51_000 * USDC, 0);
        h.claim(&mut t).unwrap();
        assert_eq!(h.s.usdc, 0);

        h.redeem(&mut t, 100, 11 * DAY);
        let r = h.settle_day(11, 52_000 * USDC, 0);
        assert!(r.slice_sold_out);
        assert_eq!(h.claim(&mut t).unwrap(), (0, 0));
        assert_eq!(h.s.share_supply, 50_000 * USDC - 100);
        h.settle_day(12, 53_000 * USDC, 0);
    }

    #[test]
    fn late_fix_uses_fix_prices_whatever_the_call_time() {
        let mut h = Harness::seeded(50_000 * USDC);
        let mut ta = Ticket::default();
        h.subscribe(&mut ta, 1_000 * USDC, 10 * DAY);
        h.settle_day(10, 51_000 * USDC, 0);
        let (a_sh, _) = h.claim(&mut ta).unwrap();
        let mut tb = Ticket::default();
        h.redeem(&mut ta, a_sh / 10, 11 * DAY);
        h.subscribe(&mut tb, 500 * USDC, 11 * DAY);

        let fix_at = 11 * DAY + ONE_AM;
        let on_time = at_fix(11, 52_000 * USDC);
        let late = FixPrices {
            signal: PricePoint {
                price: 52_000 * USDC,
                publish_time: fix_at + 30,
            },
            valuation: PricePoint {
                price: 52_000 * USDC,
                publish_time: fix_at + 30,
            },
        };
        // 晚上 9 点才定价：净值、发行份额、赎回资产都只取决于定价价格
        let a = fix(&mut h.s.clone(), fix_at, &on_time, Caller::Keeper, &h.c).unwrap();
        let b = fix(
            &mut h.s.clone(),
            11 * DAY + 21 * 3600,
            &late,
            Caller::Keeper,
            &h.c,
        )
        .unwrap();
        assert_eq!(a, b);
        assert!(b.slice_btc > 0 && b.minted_shares > 0);
    }

    #[test]
    fn too_many_unsold_slices_delays_fixing_until_they_sell() {
        let mut h = Harness::seeded(50_000 * USDC);
        let mut holder = Ticket::default();
        h.subscribe(&mut holder, 10_000 * USDC, 10 * DAY);
        h.settle_day(10, 51_000 * USDC, 0);
        h.claim(&mut holder).unwrap();

        // 连续 8 天有人赎回，但所有成交都低于 min_out（交易所收 1%）
        let mut tickets = vec![];
        for day in 11..11 + MAX_SLICES as i64 {
            let mut t = Ticket::default();
            h.redeem(&mut t, 100 * USDC, day * DAY);
            h.fix_day(day, 51_000 * USDC);
            let (plans, why, _) = h.trade_until_done(day * DAY + ONE_AM, same(51_000 * USDC), 100);
            assert_eq!((plans.len(), why), (0, FundError::SlippageExceeded));
            tickets.push(t);
        }
        // 第 9 期：定价步骤暂时失败，状态不变，这期的请求留在开放的一期里
        let day = 11 + MAX_SLICES as i64;
        let mut t = Ticket::default();
        h.redeem(&mut t, 100 * USDC, day * DAY);
        let before = h.s.clone();
        assert_eq!(
            h.fix_with(day * DAY + ONE_AM, &at_fix(day, 51_000 * USDC)),
            Err(FundError::TooManySlices)
        );
        assert_eq!(h.s, before);

        // 卖出照常进行；流动性恢复后全部卖完（基金调仓等下一次定价），
        // 下一次定价把顺延的请求一起处理
        let (plans, why, _) = h.trade_until_done(day * DAY + ONE_AM, same(51_000 * USDC), 0);
        assert_eq!((plans.len(), why), (MAX_SLICES, FundError::FixPending));
        for t in tickets.iter_mut() {
            assert!(h.claim(t).unwrap().1 > 0);
        }
        h.settle_day(day + 1, 51_000 * USDC, 0);
        assert!(h.claim(&mut t).unwrap().1 > 0);
        assert_eq!(h.s.slice_count, 0);
    }

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
        /// x 随机偏离 -bps..=bps
        fn wiggle(&mut self, x: u64, bps: u64) -> u64 {
            let d = self.below(2 * bps + 1) as i128 - bps as i128;
            (x as i128 + x as i128 * d / BPS as i128) as u64
        }
    }

    #[derive(Debug, Default)]
    struct Tally {
        buys: u32,
        slice_sells: u32,
        fund_sells: u32,
        failed_fills: u32,
        too_soon: u32,
        depeg_blocked: u32,
        depeg_sells: u32,
        keeper_only: u32,
        public_plans: u32,
    }

    /// 在 [from, until) 之间随机时刻尝试交易：每次价格在 p 附近小幅波动，
    /// 交易所收 fee_bps，fail_pct% 的成交低于 min_out（必须被拒绝且状态不变）。
    #[allow(clippy::too_many_arguments)]
    fn random_trades(
        h: &mut Harness,
        rng: &mut Rng,
        from: i64,
        until: i64,
        p: MarketPrices,
        fee_bps: u16,
        fail_pct: u64,
        tally: &mut Tally,
    ) {
        let mut now = from;
        loop {
            now += rng.below(150) as i64; // 有时不到最小间隔
            if now >= until {
                return;
            }
            let m = MarketPrices {
                signal: rng.wiggle(p.signal, 30),
                valuation: rng.wiggle(p.valuation, 30),
            };
            let depegged = is_depegged(m.signal, m.valuation, h.c.depeg_threshold_bps);
            let caller = if rng.below(4) == 0 {
                Caller::Public
            } else {
                Caller::Keeper
            };
            let fix_at = fix_time(day_of(now), &h.c);
            let in_window = (fix_at..fix_at + h.c.keeper_window_secs).contains(&now);
            let plan = match plan_trade(&h.s, now, &m, caller, &h.c) {
                Ok(plan) => plan,
                // 都做完了，或者基金调仓要等今天的定价
                Err(FundError::NothingToTrade | FundError::FixPending) => return,
                Err(FundError::TradeTooSoon) => {
                    assert!(now < h.s.last_trade_time + h.c.min_trade_interval_secs);
                    tally.too_soon += 1;
                    continue;
                }
                Err(FundError::DepegBlocksBuy) => {
                    assert!(depegged && h.s.oldest_slice().is_none() && h.s.target_btc);
                    tally.depeg_blocked += 1;
                    continue;
                }
                Err(FundError::KeeperOnly) => {
                    assert!(caller == Caller::Public && in_window);
                    tally.keeper_only += 1;
                    continue;
                }
                Err(e) => panic!("plan_trade: {e:?}"),
            };
            assert!(
                !(caller == Caller::Public && in_window),
                "窗口内只有清算地址能交易"
            );
            if caller == Caller::Public {
                tally.public_plans += 1;
            }
            assert!(btc_value_of(&plan, m.valuation) <= h.c.trade_cap_usdc);
            // 随机失败，或者交易所的手续费超过这个调用人的滑点上限
            let got = if plan.min_out > 0 && rng.below(100) < fail_pct {
                plan.min_out - 1
            } else {
                exec(&plan, m.valuation, fee_bps)
            };
            if got < plan.min_out {
                let before = h.s.clone();
                assert_eq!(h.fill(now, &plan, got), Err(FundError::SlippageExceeded));
                assert_eq!(h.s, before);
                tally.failed_fills += 1;
                continue;
            }
            h.fill(now, &plan, got).unwrap();
            match (plan.side, plan.bucket) {
                (Side::BuyBtc, _) => {
                    assert!(!depegged, "脱锚时不能买入");
                    tally.buys += 1
                }
                (Side::SellBtc, TradeBucket::Slice { .. }) => tally.slice_sells += 1,
                (Side::SellBtc, TradeBucket::Fund) => tally.fund_sells += 1,
            }
            if depegged && plan.side == Side::SellBtc {
                tally.depeg_sells += 1;
            }
        }
    }

    /// 交易的 USDC 名义金额（按执行时的估值价格）
    fn btc_value_of(plan: &TradePlan, price: u64) -> u64 {
        match plan.side {
            Side::BuyBtc => plan.amount_in,
            Side::SellBtc => btc_value(plan.amount_in, price).unwrap(),
        }
    }

    /// 模拟里的用户：排队记录、持有的份额，以及每一期已领取的总额
    struct Users {
        tickets: Vec<Ticket>,
        held: Vec<u64>,
        claimed: Vec<(u64, u64)>, // 每一期领取的 (份额, USDC)
    }

    impl Users {
        fn new(n: usize) -> Self {
            Users {
                tickets: vec![Ticket::default(); n],
                held: vec![0; n],
                claimed: vec![],
            }
        }

        /// 领取所有已定价、可以领取的记录
        fn claim_ready(&mut self, h: &mut Harness) {
            self.claimed.resize(h.records.len(), (0, 0));
            for u in 0..self.tickets.len() {
                let t = self.tickets[u];
                if t.is_empty() || t.epoch >= h.s.epoch {
                    continue;
                }
                match h.claim(&mut self.tickets[u]) {
                    Ok((sh, us)) => {
                        self.held[u] += sh;
                        self.claimed[t.epoch as usize].0 += sh;
                        self.claimed[t.epoch as usize].1 += us;
                    }
                    Err(FundError::SliceNotSold) => assert!(t.redemption_shares > 0),
                    Err(e) => panic!("claim: {e:?}"),
                }
            }
        }

        /// 已发行份额 = 用户持有 + 还没定价的排队赎回份额（定价时已销毁的不算）
        fn assert_share_supply(&self, h: &Harness) {
            let queued: u64 = self
                .tickets
                .iter()
                .filter(|t| t.epoch == h.s.epoch)
                .map(|t| t.redemption_shares)
                .sum();
            assert_eq!(h.s.share_supply, self.held.iter().sum::<u64>() + queued);
        }

        /// 不再有新请求、流动性恢复、不脱锚：剩下的赎回资产都能卖完，每张记录都能领取。
        /// 之后每一期领取之和精确等于本期总额，没有尘埃。
        fn drain(&mut self, h: &mut Harness, day: i64, price: u64) {
            for d in day..day + 3 {
                match h.fix_with(d * DAY + ONE_AM, &at_fix(d, price)) {
                    Ok(_) | Err(FundError::TooManySlices) => {}
                    Err(e) => panic!("fix: {e:?}"),
                }
                let (_, why, _) = h.trade_until_done(d * DAY + ONE_AM, same(price), 0);
                assert_eq!(why, FundError::NothingToTrade);
                self.claim_ready(h);
            }
            assert_eq!(h.s.slice_count, 0, "赎回资产都应卖完");
            assert!(
                self.tickets.iter().all(|t| t.is_empty()),
                "每张记录都应领取"
            );
            assert_eq!(self.claimed.len(), h.records.len());
            for (r, &(sh, us)) in h.records.iter().zip(&self.claimed) {
                assert!(r.slice_sold_out);
                assert_eq!((sh, us), (r.minted_shares, r.payout_usdc));
                assert_eq!(
                    (
                        r.unclaimed_subscription_usdc,
                        r.unclaimed_shares,
                        r.unclaimed_redemption_shares,
                        r.unclaimed_payout_usdc
                    ),
                    (0, 0, 0, 0)
                );
            }
            self.assert_share_supply(h);
        }
    }

    /// 随机模拟：许多用户随机申购赎回，价格随机波动，每天多笔交易，部分成交失败。检查不变量：
    ///  - 已发行份额 == 用户持有 + 排队赎回
    ///  - 每期领取之和精确等于本期总额，没有尘埃
    ///  - 没有手续费和滑点时，资产总值 = 所有用户的价值之和（误差只来自向下取整）
    #[test]
    fn random_simulation_invariants() {
        for seed in 1..=20u64 {
            let mut rng = Rng(seed);
            let mut h = Harness::seeded(50_000 * USDC);
            h.c.trade_cap_usdc = 2_000 * USDC; // 用户的金额相对上限较大，每天多笔交易
            let mut users = Users::new(8);
            let mut tally = Tally::default();
            let mut price = 50_000 * USDC;
            let mut valuation = price;
            for day in 10..400i64 {
                for u in 0..users.tickets.len() {
                    if !users.tickets[u].is_empty() {
                        continue;
                    }
                    match rng.below(10) {
                        0 | 1 => {
                            let amt = (1 + rng.below(5_000)) * USDC;
                            h.subscribe(&mut users.tickets[u], amt, day * DAY);
                        }
                        2 if users.held[u] > 0 => {
                            let sh = 1 + rng.below(users.held[u]);
                            users.held[u] -= sh;
                            h.redeem(&mut users.tickets[u], sh, day * DAY);
                        }
                        _ => {}
                    }
                }
                users.assert_share_supply(&h);
                // 价格随机游走（±8%）；cbBTC 相对 BTC 偏离 -150..=150 bps，不触发脱锚
                price = rng.wiggle(price, 800).max(1_000 * USDC);
                valuation = rng.wiggle(price, 150);
                let fix_at = day * DAY + ONE_AM;
                match h.fix_with(fix_at, &at_fix2(day, price, valuation)) {
                    Ok(_) | Err(FundError::TooManySlices) => {}
                    Err(e) => panic!("fix: {e:?}"),
                }
                let p = MarketPrices {
                    signal: price,
                    valuation,
                };
                random_trades(
                    &mut h,
                    &mut rng,
                    fix_at,
                    fix_at + DAY - 2 * ONE_AM,
                    p,
                    0,
                    20,
                    &mut tally,
                );
                users.claim_ready(&mut h);
                users.assert_share_supply(&h);
            }
            users.drain(&mut h, 400, valuation);
            assert!(
                tally.buys > 0 && tally.slice_sells > 0 && tally.fund_sells > 0,
                "seed {seed}: {tally:?}"
            );
            assert!(tally.failed_fills > 0 && tally.too_soon > 0, "{tally:?}");
            assert!(tally.keeper_only > 0 && tally.public_plans > 0, "{tally:?}");
            // 价值守恒：基金总资产 ≈ 用户份额价值之和（误差为取整尘埃），按估值价格计
            let n = nav(&h.s, valuation).unwrap();
            let user_value: u128 = users.held.iter().map(|&x| x as u128 * n / NAV_SCALE).sum();
            let assets = total_assets(&h.s, valuation).unwrap() as u128;
            assert!(assets >= user_value);
            assert!(
                assets - user_value < 1_000 * USDC as u128 / 1000,
                "seed {seed}: 尘埃过大 {}",
                assets - user_value
            );
        }
    }

    /// 随机模拟：交易所收 0.25%（接近 0.3% 滑点上限），随机晚定价、随机脱锚、
    /// 随机交易时刻和失败成交、整天没有流动性、极小额赎回。基金永远不会卡住：
    /// 每次定价都成功（或因赎回资产太多暂时顺延），最后所有赎回资产都能卖完、所有记录都能领取。
    #[test]
    fn random_simulation_with_trading_costs_never_gets_stuck() {
        let mut tally = Tally::default();
        let (mut late, mut dust, mut carried, mut illiquid_days) = (0, 0, 0, 0);
        let (mut public_fixes, mut public_fixes_rejected) = (0, 0);
        for seed in 1..=20u64 {
            let mut rng = Rng(seed * 7919);
            let mut h = Harness::seeded(50_000 * USDC);
            h.c.trade_cap_usdc = 1_000 * USDC;
            let mut users = Users::new(6);
            let mut price = 50_000 * USDC;
            let mut valuation = price;
            let mut fail_pct = 30;
            for day in 10..300i64 {
                for u in 0..users.tickets.len() {
                    if !users.tickets[u].is_empty() {
                        continue;
                    }
                    let r = rng.below(10);
                    if r < 2 {
                        let amt = (1 + rng.below(3_000)) * USDC;
                        h.subscribe(&mut users.tickets[u], amt, day * DAY);
                    } else if r < 4 && users.held[u] > 0 {
                        let held = users.held[u];
                        let sh = if r == 2 {
                            held
                        } else if rng.below(4) == 0 {
                            dust += 1;
                            (1 + rng.below(1_000)).min(held) // 对应的 cbBTC 可能取整为 0
                        } else {
                            1 + rng.below(held)
                        };
                        users.held[u] -= sh;
                        h.redeem(&mut users.tickets[u], sh, day * DAY);
                    }
                }
                // 昨天的价格附近：定价之前，前几天没做完的交易继续
                let yesterday = MarketPrices {
                    signal: price,
                    valuation,
                };
                // 价格随机游走（±8%）；cbBTC 相对 BTC 偏离 -300..=300 bps，约三分之一超过脱锚阈值
                price = rng.wiggle(price, 800).max(1_000 * USDC);
                valuation = rng.wiggle(price, 300);
                // 当天任意时刻定价；定价价格在定价窗口内发布
                let fix_at = day * DAY + ONE_AM;
                let now = fix_at + rng.below((DAY - 2 * ONE_AM) as u64) as i64;
                // 昨天没有流动性时，一直持续到今天定价之后，让赎回资产跨过定价时刻
                random_trades(
                    &mut h,
                    &mut rng,
                    day * DAY,
                    now,
                    yesterday,
                    25,
                    fail_pct,
                    &mut tally,
                );
                fail_pct = if rng.below(20) == 0 {
                    illiquid_days += 1;
                    100
                } else {
                    30
                };
                if now > fix_at {
                    late += 1;
                    let t = fix_at + rng.below((now - fix_at) as u64) as i64;
                    assert_eq!(
                        request_subscription(&mut h.s, &mut Ticket::default(), USDC, t, &h.c),
                        Err(FundError::QueueClosed)
                    );
                }
                if h.s.slice_count > 0 {
                    carried += 1;
                }
                let publish =
                    |rng: &mut Rng| (fix_at + rng.below(FIX_WINDOW_SECS as u64) as i64).min(now);
                let p = FixPrices {
                    signal: PricePoint {
                        price,
                        publish_time: publish(&mut rng),
                    },
                    valuation: PricePoint {
                        price: valuation,
                        publish_time: publish(&mut rng),
                    },
                };
                // 随机调用人：清算窗口内其他人被拒绝，由清算地址定价
                let in_window = now < fix_at + h.c.keeper_window_secs;
                let caller = if rng.below(3) == 0 {
                    Caller::Public
                } else {
                    Caller::Keeper
                };
                let caller = if caller == Caller::Public && in_window {
                    assert_eq!(h.fix_as(now, &p, caller), Err(FundError::KeeperOnly));
                    public_fixes_rejected += 1;
                    Caller::Keeper
                } else {
                    caller
                };
                match h.fix_as(now, &p, caller) {
                    Ok(_) if caller == Caller::Public => public_fixes += 1,
                    Ok(_) | Err(FundError::TooManySlices) => {}
                    Err(e) => panic!("fix: {e:?}"),
                }
                let m = MarketPrices {
                    signal: price,
                    valuation,
                };
                random_trades(
                    &mut h,
                    &mut rng,
                    now,
                    (day + 1) * DAY,
                    m,
                    25,
                    fail_pct,
                    &mut tally,
                );
                users.claim_ready(&mut h);
                users.assert_share_supply(&h);
            }
            users.drain(&mut h, 300, price);
        }
        // 各种交易路径、晚定价、脱锚的两种情况、顺延和极小额赎回都应被覆盖到
        assert!(
            tally.buys > 0 && tally.slice_sells > 0 && tally.fund_sells > 0,
            "{tally:?}"
        );
        assert!(
            tally.depeg_blocked > 0 && tally.depeg_sells > 0 && tally.failed_fills > 0,
            "{tally:?}"
        );
        assert!(tally.keeper_only > 0 && tally.public_plans > 0, "{tally:?}");
        assert!(
            public_fixes > 0 && public_fixes_rejected > 0,
            "{public_fixes} {public_fixes_rejected}"
        );
        assert!(
            late > 0 && dust > 0 && carried > 0 && illiquid_days > 0,
            "{late} {dust} {carried} {illiquid_days}"
        );
    }

    #[test]
    fn depeg_threshold_is_relative_to_the_signal_price() {
        let bps = cfg().depeg_threshold_bps;
        let signal = 50_000 * USDC;
        let limit = signal / BPS as u64 * bps as u64; // 2% = 1,000 USDC
        assert!(!is_depegged(signal, signal, bps));
        assert!(!is_depegged(signal, signal - limit, bps));
        assert!(!is_depegged(signal, signal + limit, bps));
        assert!(is_depegged(signal, signal - limit - 1, bps));
        assert!(is_depegged(signal, signal + limit + 1, bps));
    }
}
