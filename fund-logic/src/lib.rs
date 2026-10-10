//! 均线基金的全部记账逻辑（纯 Rust，不依赖 Solana）。
//!
//! 单位约定：
//! - USDC：6 位小数，原始整数单位
//! - BTC（链上包装 BTC，如 cbBTC）：8 位小数，原始整数单位
//! - 份额：6 位小数，初始 1 份 = 1 USDC
//! - 价格：每 1 枚 BTC 值多少“微美元”（USD × 1e6），即 USDC 原始单位
//!
//! 规则：
//! - 申购、赎回随时可以排队，排队从不关闭。每个请求属于关闭它的定价时刻那一“期”，
//!   期号就是那个定价时刻的日期序号：01:00 之前提交的属于当天，之后提交的属于第二天。
//!   某天没有定价（跳过）时，这一期的请求顺延到下一次定价，一次定价可以关闭好几期。
//! - 请求在关闭它的定价时刻之前可以撤销，全额退回；定价时刻之后不能撤销。
//! - 基金从不按美元估值（ADR 0006）：赎回资产按份额比例从基金划出实物；申购带来多少
//!   基金资产（买到的 cbBTC，或目标为 USDC 时的 USDC），就按基金每份对应的资产发多少份额。
//! - 结算分多步（ADR 0001）：定价步骤（fix）记录价格、算出目标、
//!   销毁赎回份额并按比例划出赎回资产；之后是一系列交易（plan_trade + fill_trade），
//!   每笔不超过 trade_cap_usdc，间隔至少 min_trade_interval_secs。
//! - 交易优先级：先卖未卖完的赎回资产（最早的一期优先），再把基金调向目标，
//!   最后为待成交申购买入。基金调仓完成、只剩一种资产时才能按数量发份额。
//! - 赎回人拿到自己那份赎回资产实际卖出的 USDC（ADR 0002），整期卖完之后才能领取。
//! - 下一个定价时刻到来时没做完的交易延续下去：赎回资产继续优先卖出，
//!   基金调仓按新的目标重新计算，结算永远不会卡住。
//! - 信号和轧差用“定价时刻”（当天 UTC 0 点 + settle_offset）发布的价格，
//!   不用调用 fix 那一刻的价格：调用人不能挑一天里对自己最有利的时刻。
//! - 定价时刻到当天定价步骤之间提交的请求属于下一期：定价价格已经公开，
//!   这段时间的请求不能按它成交，也不能撤销已经排队的请求。
//! - 价格源（ADR 0005）：基金持有的是 cbBTC，有 cbBTC/USD 价格源时，均线、信号、轧差和交易的
//!   min_out 都用它，BTC/USD 只用来检查脱锚（ADR 0003）；没有 cbBTC/USD 时都用 BTC/USD，
//!   不检查脱锚，交易只靠滑点上限。用哪个由配置 signal_feed 决定。
//! - 定价时记录当天的信号价格，如果高于最近 window 个日历日信号价格的平均值，目标是持有 cbBTC；
//!   否则全部持有 USDC。价格按日期存在 MAX_WINDOW 个格子里：没有定价的一天可以由任何人
//!   用那天定价窗口内发布的价格回填；没回填的跳过，用更早的一天补足。
//!   最近 MAX_WINDOW 天里价格不足 window 个时持有 USDC。
//! - 两个价格源都有时，每笔交易前比较两个最新价格：相差超过 depeg_threshold_bps（脱锚）时不买入 cbBTC；
//!   卖出和定价照常进行，持续脱锚也不会让基金卡住。
//! - 资产用内部记账，不读取代币账户余额，别人直接转账进来不会影响份额计算
//!   （防止“捐款抬高份额价格”攻击）。赎回资产和待成交申购单独记账，不算基金资产。

pub const BTC_UNIT: u128 = 100_000_000; // 1e8，1 枚 BTC 的原始单位
pub const BPS: u128 = 10_000;
pub const SECONDS_PER_DAY: i64 = 86_400;
pub const MAX_WINDOW: usize = 64;
/// 同时未卖完的赎回资产最多几期。正常情况下当天就能卖完；
/// 满了之后定价步骤暂时失败，当期申购赎回顺延到下一次定价，卖出照常进行。
pub const MAX_SLICES: usize = 8;
/// 同时排队的待成交申购最多几期。满了之后新一期的申购全额退款（比如长时间脱锚、买不了
/// cbBTC），定价照常进行，赎回不受影响。
pub const MAX_PENDING: usize = 8;
/// 定价价格的发布时间必须落在 [定价时刻, 定价时刻 + FIX_WINDOW_SECS) 内。
/// 调用人只能在这几十秒内发布的价格里挑，可利用的价差可以忽略。
pub const FIX_WINDOW_SECS: i64 = 60;
/// 配置修改的时间锁：提议之后至少 48 小时才能生效，持有人有时间先赎回。
/// 写死在代码里，配置本身改不了它。
pub const TIMELOCK_SECS: i64 = 172_800;
/// 取款费率上限（ADR 0007），写死在代码里
pub const MAX_WITHDRAWAL_FEE_BPS: u16 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FundError {
    TooEarly,         // 还没到当天定价时刻
    AlreadySettled,   // 今天已经定价过
    BadPrice,         // 价格为 0
    SlippageExceeded, // 实际成交少于 min_out
    InvalidConfig,
    Overflow,
    NothingToClaim,
    EpochNotSettled,
    BelowMinSubscription,
    ZeroAmount,
    TooManyPrices,
    PriceNotAtFixTime,       // 定价价格的发布时间不在定价窗口内
    NothingToTrade,          // 没有需要交易的东西
    TradeTooSoon,            // 距离上一笔交易不到 min_trade_interval_secs
    DepegBlocksBuy,          // 需要买入 cbBTC，但 cbBTC 脱锚
    SliceNotSold,            // 这一期的赎回资产还没卖完
    WrongEpochRecord,        // 传入的期记录与交易计划不符
    TooManySlices,           // 未卖完的赎回资产已有 MAX_SLICES 期
    FixPending,              // 已过今天的定价时刻、还没定价，基金调仓等待定价
    KeeperOnly,              // 清算窗口内只有清算地址能驱动结算
    WrongEpoch,              // 排队记录不是这一刻开放的那一期
    TooLateToCancel,         // 已过关闭这一期的定价时刻，不能再撤销
    NothingToCancel,         // 排队记录是空的
    SubscriptionsPaused,     // 管理员暂停了申购（赎回永远不能暂停）
    TimelockNotElapsed,      // 配置提议还没过时间锁
    SubscriptionCapExceeded, // 排队和待成交的申购加上这一笔会超过申购上限
    ConfigFieldFixed,        // 定价时刻和均线窗口只能在创建时设定
    PriceAlreadyRecorded,    // 这一天已经有信号价格
    BackfillOutOfRange,      // 只能回填过去、还在存储范围内（最近 MAX_WINDOW 天）的一天
}

/// 信号（以及均线、轧差、min_out）用哪个价格源
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalFeed {
    CbBtc, // Pyth 有 cbBTC/USD：用它；BTC/USD 有的话只用来检查脱锚
    Btc,   // 没有 cbBTC/USD：都用 BTC/USD，不检查脱锚
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub window: u8,                   // 均线窗口（天），默认 50
    pub settle_offset_secs: i64,      // 定价时刻：每天 UTC 0 点之后多少秒，默认 3600（1 点）
    pub max_slippage_bps: u16,        // 交易相对估值价格的最大滑点
    pub subscription_fee_bps: u16,    // 申购费，留在基金里
    pub min_subscription: u64,        // 最低申购金额（USDC 原始单位）
    pub depeg_threshold_bps: u16,     // cbBTC/USD 与 BTC/USD 相差超过这个幅度就不买入 cbBTC
    pub trade_cap_usdc: u64,          // 单笔交易上限（按估值价格折算的 USDC 名义金额）
    pub min_trade_interval_secs: i64, // 两笔交易之间至少间隔多少秒
    pub keeper_window_secs: i64,      // 定价时刻之后多少秒内只有清算地址能驱动结算
    pub public_slippage_bps: u16,     // 窗口之后其他人交易的滑点上限，比 max_slippage_bps 更严
    pub withdrawal_fee_bps: u16,      // 取款费率（领取赎回款时欠管理人），最多 1%
    pub subscription_cap_usdc: u64,   // 申购上限：还没买完的申购（排队的加待成交的）最多这么多
    pub signal_feed: SignalFeed,      // 信号价格源，默认 BTC/USD；创建时设定，只能通过时间锁修改
}

impl Config {
    pub fn default_config() -> Self {
        Config {
            window: 50,
            settle_offset_secs: 3_600,
            // 0.075% 手续费 + 约 0.1% 滑点，再留出预言机与成交价的价差
            max_slippage_bps: 30,
            subscription_fee_bps: 0,
            min_subscription: 1_000_000, // 1 USDC
            depeg_threshold_bps: 200,
            trade_cap_usdc: 200_000_000_000, // 200,000 USDC
            min_trade_interval_secs: 60,
            keeper_window_secs: 1_800,
            public_slippage_bps: 10,
            withdrawal_fee_bps: 100,
            subscription_cap_usdc: 1_000_000_000_000, // 1,000,000 USDC
            signal_feed: SignalFeed::Btc,
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
        if self.withdrawal_fee_bps > MAX_WITHDRAWAL_FEE_BPS
            || self.subscription_cap_usdc < self.min_subscription
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

/// 一期申购中还没成交的 USDC（待成交申购），留在申购托管账户里，不算基金资产。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pending {
    pub epoch: u64,
    pub usdc: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FundState {
    pub usdc: u64,                     // 基金持有的 USDC（不含排队中的申购款、赎回资产）
    pub btc: u64,                      // 基金持有的 cbBTC（不含赎回资产）
    pub share_supply: u64,             // 已发行份额（含排队赎回、尚未销毁的份额）
    pub queued_subscription_usdc: u64, // 下一次定价要处理的排队申购款（可能包含好几期）
    pub queued_redemption_shares: u64, // 下一次定价要处理的排队赎回份额
    // 已过今天的定价时刻、今天还没定价时提交的请求（第 next_epoch 期）：今天的定价不处理它们。
    // 今天没有定价的话，它们并入上面的排队，由下一次定价一起处理。
    pub next_epoch: u64,
    pub next_subscription_usdc: u64,
    pub next_redemption_shares: u64,
    // 每日信号价格：第 day 天存在 day mod MAX_WINDOW 号格子里，price_days 记它是哪一天。
    // 价格为 0 表示空格子。
    pub prices: [u64; MAX_WINDOW],
    pub price_days: [i64; MAX_WINDOW],
    pub target_btc: bool, // 最近一次定价的目标：true 持有 cbBTC，false 持有 USDC
    pub last_fix_day: i64, // 最近一次定价的日期序号（unix 秒 / 86400），-1 表示从未定价
    pub last_trade_time: i64, // 最近一笔交易的时间（unix 秒）
    pub slices: [Slice; MAX_SLICES], // 未卖完的赎回资产，最早的在前
    pub slice_count: u8,
    pub pending: [Pending; MAX_PENDING], // 待成交申购，最早的在前
    pub pending_count: u8,
    pub subscriptions_paused: bool, // 管理员可以立即暂停、恢复申购；赎回永远不能暂停
    // 欠管理人的取款费：留在待领取账户里，由 collect_fees 付到收费地址。
    // 领取时不直接付，这样收费地址的代币账户出什么问题（关闭、换主人、被冻结）都挡不住赎回人领取。
    pub fees_owed: u64,
}

impl FundState {
    pub fn new() -> Self {
        FundState {
            usdc: 0,
            btc: 0,
            share_supply: 0,
            queued_subscription_usdc: 0,
            queued_redemption_shares: 0,
            next_epoch: 0,
            next_subscription_usdc: 0,
            next_redemption_shares: 0,
            prices: [0; MAX_WINDOW],
            price_days: [0; MAX_WINDOW],
            target_btc: false,
            last_fix_day: -1,
            last_trade_time: 0,
            slices: [Slice::default(); MAX_SLICES],
            slice_count: 0,
            pending: [Pending::default(); MAX_PENDING],
            pending_count: 0,
            subscriptions_paused: false,
            fees_owed: 0,
        }
    }

    /// 最早的一期未卖完的赎回资产（下一笔交易优先卖它）
    pub fn oldest_slice(&self) -> Option<&Slice> {
        self.slices[..self.slice_count as usize].first()
    }

    /// 最早的一期待成交申购
    pub fn oldest_pending(&self) -> Option<&Pending> {
        self.pending[..self.pending_count as usize].first()
    }

    /// 还没买完的申购款：排队的（含下一期）加上各期待成交的
    /// （u128 求和：最多 MAX_PENDING + 2 个 u64，不会溢出）
    fn outstanding_subscription_usdc(&self) -> Result<u64, FundError> {
        let pending: u128 = self.pending[..self.pending_count as usize]
            .iter()
            .map(|p| p.usdc as u128)
            .sum();
        to_u64(
            pending + self.queued_subscription_usdc as u128 + self.next_subscription_usdc as u128,
        )
    }

    /// 第 i 期待成交申购移出队列（全部成交或者所有人都撤销了），后面的往前挪
    fn remove_pending(&mut self, i: usize) {
        let n = self.pending_count as usize;
        self.pending.copy_within(i + 1..n, i);
        self.pending[n - 1] = Pending::default();
        self.pending_count -= 1;
    }
}

impl Default for FundState {
    fn default() -> Self {
        Self::new()
    }
}

/// 一次定价的结果，存到链上，用户据此领取份额或 USDC。
/// 它关闭 first_epoch..=epoch 这几期：epoch 是定价当天的日期序号，
/// 更早的是被跳过的定价顺延过来的。这几期的请求合在一起按比例领取。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpochRecord {
    pub epoch: u64,
    pub first_epoch: u64,
    pub signal_price: u64,         // 选定价格源的定价价格：记入均线，也用于轧差
    pub reference_price: u64,      // 脱锚检查用的 BTC/USD 定价价格，没有参照时为 0
    pub moving_average: u64,       // 价格不足 window 个时为 0
    pub subscription_usdc: u64,    // 本期申购总额（扣申购费之前），申购人按它的比例领取份额
    pub minted_shares: u64,        // 本期已发行的份额：随成交增加
    pub subscription_filled: bool, // 本期申购已全部成交，可以做最后一次领取
    pub netted_usdc: u64,          // 定价时轧差：申购款付给赎回人的 USDC
    pub netted_btc: u64,           // 定价时轧差：赎回资产换给申购人的 cbBTC
    pub refund_usdc: u64,          // 待成交申购已满、本期申购全额退款的 USDC
    pub redemption_shares: u64,    // 本期赎回（已销毁）的份额
    pub slice_usdc: u64,           // 定价时划给赎回人的 USDC
    pub slice_btc: u64,            // 定价时划给赎回人、需要卖出的 cbBTC
    pub payout_usdc: u64,          // 赎回资产全部卖完后的 USDC 总额（按份额比例分给赎回人）
    pub slice_sold_out: bool,      // 赎回资产已全部卖完，赎回人可以领取
    pub target_btc: bool,          // 本次定价的目标（信号）
    // 尚未领取的部分。每次领取后递减，最后一位领取人拿到精确的余数，
    // 不会留下取整“尘埃”。
    pub unclaimed_subscription_usdc: u64,
    pub unclaimed_shares: u64,
    pub unclaimed_redemption_shares: u64,
    pub unclaimed_payout_usdc: u64,
    pub unclaimed_refund_usdc: u64,
    // 撤销了待成交申购的人：他们的申购额，和撤销时一次领走的已成交份额。
    // 留下的人按 (minted_shares − cancelled_shares) × 本人申购额 ÷ (subscription_usdc − cancelled_subscription_usdc) 领取
    pub cancelled_subscription_usdc: u64,
    pub cancelled_shares: u64,
}

impl EpochRecord {
    /// 这次定价是否关闭了第 epoch 期
    pub fn covers(&self, epoch: u64) -> bool {
        (self.first_epoch..=self.epoch).contains(&epoch)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    BuyBtc,       // 用 USDC 买 cbBTC
    SellBtc,      // 卖 cbBTC 换 USDC
    WithoutTrade, // 不交易：目标为 USDC 时，申购款直接进入基金
}

/// 一个预言机价格及其发布时间（unix 秒）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PricePoint {
    pub price: u64,
    pub publish_time: i64,
}

/// 定价步骤用的价格：各价格源在定价窗口内发布的价格，没有这个价格源时为 None。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixPrices {
    pub cbbtc: Option<PricePoint>, // cbBTC/USD
    pub btc: Option<PricePoint>,   // BTC/USD
}

/// 按配置选出的价格：price 用于信号、均线、轧差和 min_out（即信号价格和估值价格）；
/// reference 是脱锚检查的参照价格（BTC/USD）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FeedChoice<T> {
    price: T,
    reference: Option<T>,
    checks_peg: bool, // 基金持有的 cbBTC 有自己的价格源：要和 BTC/USD 比较才能买入
}

/// signal_feed 为 CbBtc 时必须有 cbBTC/USD，BTC/USD 是脱锚检查的参照；为 Btc 时必须有
/// BTC/USD，cbBTC/USD 即使提供也不用，不检查脱锚。缺少选定的价格源时返回 BadPrice。
fn choose_feed<T: Copy>(
    cbbtc: Option<T>,
    btc: Option<T>,
    cfg: &Config,
) -> Result<FeedChoice<T>, FundError> {
    match cfg.signal_feed {
        SignalFeed::CbBtc => Ok(FeedChoice {
            price: cbbtc.ok_or(FundError::BadPrice)?,
            reference: btc,
            checks_peg: true,
        }),
        SignalFeed::Btc => Ok(FeedChoice {
            price: btc.ok_or(FundError::BadPrice)?,
            reference: None,
            checks_peg: false,
        }),
    }
}

impl FeedChoice<u64> {
    /// 能不能买入 cbBTC（轧差也算买入）：要检查脱锚时，参照价格缺失（没提供、过旧、不在定价窗口内）
    /// 等于无法确认没有脱锚，也不买入。这样少传参照价格绕不过脱锚检查；
    /// 而卖出和定价不需要参照价格，参照出问题时照常进行（不变量 6）。
    fn blocks_buys(&self, cfg: &Config) -> bool {
        self.checks_peg
            && self.reference.map_or(true, |btc| {
                is_depegged(btc, self.price, cfg.depeg_threshold_bps)
            })
    }
}

/// 定价步骤的结果：本期记录，以及外层要做的转账里记录没有直接给出的一项
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixOutcome {
    pub record: EpochRecord,
    /// 申购费：从申购托管转入基金主账户
    pub fee_usdc: u64,
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

/// 基金按数量计算的份额：asset_added × 已发行份额 ÷ 基金持有的同一种资产，向下取整
/// （对留下的人有利）。基金还没有份额（或这种资产为零）时，1 USDC 换 1 份。
fn shares_for(added: u64, holding: u64, supply: u64, usdc_paid: u64) -> Result<u64, FundError> {
    if supply == 0 || holding == 0 {
        return Ok(usdc_paid);
    }
    pro_rata(added, supply, holding)
}

/// 买入这么多 USDC 的 cbBTC 不到 2 sat：太少，留在基金里不买
fn is_dust_buy(usdc: u64, price: u64) -> Result<bool, FundError> {
    Ok(usdc_to_btc(usdc, price)? <= 1)
}

/// 基金的调仓是否已经完成（只剩目标资产，另一种只剩零头）。
/// 只有这时才能按数量给申购发份额，也才轧差。
fn fund_rebalanced(s: &FundState, price: u64) -> Result<bool, FundError> {
    if s.target_btc {
        is_dust_buy(s.usdc, price)
    } else {
        Ok(s.btc == 0)
    }
}

fn price_slot(day: i64) -> usize {
    day.rem_euclid(MAX_WINDOW as i64) as usize
}

/// 第 day 天的信号价格（没有记录或者格子已被更晚的一天占用时为 None）
fn price_on(s: &FundState, day: i64) -> Option<u64> {
    let i = price_slot(day);
    (s.prices[i] > 0 && s.price_days[i] == day).then_some(s.prices[i])
}

/// 记录第 day 天的信号价格，覆盖这个格子里 MAX_WINDOW 天之前的那一天
pub fn record_price(s: &mut FundState, day: i64, price: u64) {
    let i = price_slot(day);
    s.prices[i] = price;
    s.price_days[i] = day;
}

/// 第 day 天的均线：从这一天往前，取最近 window 个有价格的日历日求简单平均。
/// 没有价格的一天跳过；最多往回看 MAX_WINDOW 天（存储只有这么多），不足 window 个时返回 None。
pub fn moving_average(s: &FundState, day: i64, window: u8) -> Option<u64> {
    if window == 0 {
        return None;
    }
    let (mut sum, mut n): (u128, u8) = (0, 0);
    for d in (0..MAX_WINDOW as i64).map(|k| day.saturating_sub(k)) {
        if let Some(p) = price_on(s, d) {
            sum += p as u128;
            n += 1;
            if n == window {
                return Some((sum / window as u128) as u64);
            }
        }
    }
    None
}

/// 管理员在首次定价前写入历史价格，让均线从第一天就能计算。
/// prices 是到 last_day 为止（含）连续几天的价格，最早的在前。
/// 只能在从未定价过时调用（由外层检查）。
pub fn seed_prices(
    s: &mut FundState,
    prices: &[u64],
    last_day: i64,
    cfg: &Config,
) -> Result<(), FundError> {
    if prices.len() > cfg.window as usize {
        return Err(FundError::TooManyPrices);
    }
    if prices.contains(&0) {
        return Err(FundError::BadPrice);
    }
    // 不超过 window ≤ MAX_WINDOW 个，不会溢出
    let first_day = last_day - prices.len() as i64 + 1;
    for (k, &p) in prices.iter().enumerate() {
        record_price(s, first_day + k as i64, p);
    }
    Ok(())
}

/// 回填没有定价的一天的信号价格，任何人都可以调用。
/// 只接受过去的一天、还在存储范围内（最近 MAX_WINDOW 天）、还没有价格，
/// 而且价格发布在那一天的定价窗口内：回填的人只能在那几十秒里发布的价格中挑，
/// 和那天按时定价拿到的价格一样（谁先回填用谁的；换成 Pyth 后这几十秒里的价差可以忽略）。
/// 只影响之后定价时的均线，已经定价的结果不变。价格源按配置选择，和定价一样。
/// 返回记下的信号价格。
pub fn backfill_price(
    s: &mut FundState,
    day: i64,
    prices: &FixPrices,
    now_unix: i64,
    cfg: &Config,
) -> Result<u64, FundError> {
    let signal = &choose_feed(prices.cbbtc, prices.btc, cfg)?.price;
    let today = day_of(now_unix);
    if day >= today || day <= today.saturating_sub(MAX_WINDOW as i64) {
        return Err(FundError::BackfillOutOfRange);
    }
    if price_on(s, day).is_some() {
        return Err(FundError::PriceAlreadyRecorded);
    }
    if signal.price == 0 {
        return Err(FundError::BadPrice);
    }
    check_fix_window(signal.publish_time, fix_time(day, cfg))?;
    record_price(s, day, signal.price);
    Ok(signal.price)
}

/// cbBTC 是否脱锚：cbBTC/USD 与 BTC/USD 之差超过 BTC/USD 的 threshold_bps。
pub fn is_depegged(btc: u64, cbbtc: u64, threshold_bps: u16) -> bool {
    let gap = (cbbtc as u128).abs_diff(btc as u128);
    gap * BPS > (btc as u128) * threshold_bps as u128
}

pub fn day_of(unix: i64) -> i64 {
    unix.div_euclid(SECONDS_PER_DAY)
}

/// 价格的发布时间是否在定价窗口 [fix_at, fix_at + FIX_WINDOW_SECS) 内：
/// 定价和回填都只认这几十秒里发布的价格，调用人不能挑一天里对自己最有利的时刻
fn check_fix_window(publish_time: i64, fix_at: i64) -> Result<(), FundError> {
    if publish_time < fix_at || publish_time >= fix_at.saturating_add(FIX_WINDOW_SECS) {
        return Err(FundError::PriceNotAtFixTime);
    }
    Ok(())
}

/// 当天的定价时刻（unix 秒）
pub fn fix_time(day: i64, cfg: &Config) -> i64 {
    day * SECONDS_PER_DAY + cfg.settle_offset_secs
}

fn epoch_of_day(day: i64) -> Result<u64, FundError> {
    u64::try_from(day).map_err(|_| FundError::Overflow)
}

fn day_of_epoch(epoch: u64) -> Result<i64, FundError> {
    i64::try_from(epoch).map_err(|_| FundError::Overflow)
}

/// 这一刻提交的请求属于哪一期：关闭它的下一个定价时刻所在的日期序号。
/// 当天定价时刻之前提交的属于当天，之后提交的属于第二天，不管当天的定价步骤执行了没有。
pub fn request_epoch(now_unix: i64, cfg: &Config) -> Result<u64, FundError> {
    let day = day_of(now_unix);
    if now_unix < fix_time(day, cfg) {
        epoch_of_day(day)
    } else {
        epoch_of_day(day.checked_add(1).ok_or(FundError::Overflow)?)
    }
}

/// 下一次定价最早在哪一天：今天，或者今天已经定价过时的明天（last_fix_day ≥ -1，不会溢出）
fn next_fix_day(s: &FundState, now_unix: i64) -> i64 {
    day_of(now_unix).max(s.last_fix_day.saturating_add(1))
}

/// 第 epoch 期的请求是否由下一次定价处理（否则在 next_* 里，等再下一次）
fn joins_next_fix(s: &FundState, epoch: u64, now_unix: i64) -> Result<bool, FundError> {
    Ok(day_of_epoch(epoch)? <= next_fix_day(s, now_unix))
}

/// next_* 那一期的定价时刻已过、那天却没有定价（跳过）时，并入下一次定价要处理的排队。
fn roll_queue(s: &mut FundState, now_unix: i64) -> Result<(), FundError> {
    let empty = s.next_subscription_usdc == 0 && s.next_redemption_shares == 0;
    if empty || !joins_next_fix(s, s.next_epoch, now_unix)? {
        return Ok(());
    }
    s.queued_subscription_usdc = s
        .queued_subscription_usdc
        .checked_add(s.next_subscription_usdc)
        .ok_or(FundError::Overflow)?;
    s.queued_redemption_shares = s
        .queued_redemption_shares
        .checked_add(s.next_redemption_shares)
        .ok_or(FundError::Overflow)?;
    s.next_epoch = 0;
    s.next_subscription_usdc = 0;
    s.next_redemption_shares = 0;
    Ok(())
}

/// 第 epoch 期请求所在的排队（申购款, 赎回份额）。调用前先 roll_queue。
fn queue_of(
    s: &mut FundState,
    epoch: u64,
    now_unix: i64,
) -> Result<(&mut u64, &mut u64), FundError> {
    if joins_next_fix(s, epoch, now_unix)? {
        return Ok((
            &mut s.queued_subscription_usdc,
            &mut s.queued_redemption_shares,
        ));
    }
    // 不由下一次定价处理的只能是明天这一期（roll_queue 已经并入更早的）
    s.next_epoch = epoch;
    Ok((&mut s.next_subscription_usdc, &mut s.next_redemption_shares))
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
// 治理：配置修改走时间锁
// ---------------------------------------------------------------------------

/// 提议一份完整的新配置，替换 current：先校验，返回最早可以生效的时刻。
/// 新的提议取代还没生效的旧提议，重新计时。
///
/// 定价时刻和均线窗口不能改：
/// - 期号按定价时刻划分。当天定价之后把定价时刻往后挪，这一刻的请求会被算进
///   已经定价的那一期，冒领那一期别人的份额，别人的领取从此失败。
/// - 均线窗口变长时，已记录的价格不够算均线，基金会一连几天持有 USDC：这是策略改动。
pub fn propose_config(current: &Config, new: &Config, now_unix: i64) -> Result<i64, FundError> {
    new.validate()?;
    if new.settle_offset_secs != current.settle_offset_secs || new.window != current.window {
        return Err(FundError::ConfigFieldFixed);
    }
    now_unix
        .checked_add(TIMELOCK_SECS)
        .ok_or(FundError::Overflow)
}

/// 提议的配置到了 effective_at 才能生效
pub fn check_timelock(effective_at: i64, now_unix: i64) -> Result<(), FundError> {
    if now_unix < effective_at {
        return Err(FundError::TimelockNotElapsed);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 申购、赎回排队
// ---------------------------------------------------------------------------

/// 用户在某一期的排队记录。每个用户每一期一张，所以可以同时有好几期没领取。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ticket {
    pub epoch: u64,
    pub subscription_usdc: u64,
    pub redemption_shares: u64,
    pub shares_claimed: u64, // 本期申购已经领取的份额（申购可以分几次领取）
}

impl Ticket {
    pub fn is_empty(&self) -> bool {
        self.subscription_usdc == 0 && self.redemption_shares == 0
    }
}

/// 把请求加进它这一期的排队记录和排队。t 必须是空的，或者正是这一刻开放的那一期
/// （外层按 request_epoch 推出排队记录的地址）。
fn enqueue(
    s: &mut FundState,
    t: &mut Ticket,
    usdc: u64,
    shares: u64,
    now_unix: i64,
    cfg: &Config,
) -> Result<(), FundError> {
    let epoch = request_epoch(now_unix, cfg)?;
    if !t.is_empty() && t.epoch != epoch {
        return Err(FundError::WrongEpoch);
    }
    let mut n = s.clone();
    roll_queue(&mut n, now_unix)?;
    let (queued_usdc, queued_shares) = queue_of(&mut n, epoch, now_unix)?;
    *queued_usdc = queued_usdc.checked_add(usdc).ok_or(FundError::Overflow)?;
    *queued_shares = queued_shares
        .checked_add(shares)
        .ok_or(FundError::Overflow)?;
    let ticket = Ticket {
        epoch,
        subscription_usdc: t
            .subscription_usdc
            .checked_add(usdc)
            .ok_or(FundError::Overflow)?,
        redemption_shares: t
            .redemption_shares
            .checked_add(shares)
            .ok_or(FundError::Overflow)?,
        shares_claimed: t.shares_claimed,
    };
    *s = n;
    *t = ticket;
    Ok(())
}

pub fn request_subscription(
    s: &mut FundState,
    t: &mut Ticket,
    amount: u64,
    now_unix: i64,
    cfg: &Config,
) -> Result<(), FundError> {
    if amount == 0 {
        return Err(FundError::ZeroAmount);
    }
    if amount < cfg.min_subscription {
        return Err(FundError::BelowMinSubscription);
    }
    // 暂停只挡新的申购：已经排队和待成交的照常定价、成交、领取
    if s.subscriptions_paused {
        return Err(FundError::SubscriptionsPaused);
    }
    // 上限算上之前几期留下的待成交申购：每天要买的 cbBTC 有上限。赎回不设上限。
    let after = (s.outstanding_subscription_usdc()? as u128) + amount as u128;
    if after > cfg.subscription_cap_usdc as u128 {
        return Err(FundError::SubscriptionCapExceeded);
    }
    enqueue(s, t, amount, 0, now_unix, cfg)
}

pub fn request_redemption(
    s: &mut FundState,
    t: &mut Ticket,
    shares: u64,
    now_unix: i64,
    cfg: &Config,
) -> Result<(), FundError> {
    if shares == 0 {
        return Err(FundError::ZeroAmount);
    }
    enqueue(s, t, 0, shares, now_unix, cfg)
}

/// 第 epoch 期的请求最晚在什么时刻之前可以撤销：关闭它的定价时刻，
/// 即这一期自己的定价时刻，或者它被跳过时今天的定价时刻
pub fn cancel_deadline(epoch: u64, now_unix: i64, cfg: &Config) -> Result<i64, FundError> {
    Ok(fix_time(day_of_epoch(epoch)?.max(day_of(now_unix)), cfg))
}

/// 撤销的结果：外层从申购托管退回 usdc，从份额托管退回 shares。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CancelOutcome {
    pub usdc: u64,
    pub shares: u64,
}

/// 撤销一张还没定价的排队记录，申购款和赎回份额全额退回，记录清空。
/// 只能在关闭这一期的定价时刻之前：今天的定价时刻已过、今天还没定价时，定价价格已经公开，
/// 这时撤销等于看到价格再决定要不要成交，所以拒绝。被跳过的定价顺延到下一次，
/// 在下一个定价时刻之前又可以撤销。
pub fn cancel(
    s: &mut FundState,
    t: &mut Ticket,
    now_unix: i64,
    cfg: &Config,
) -> Result<CancelOutcome, FundError> {
    if t.is_empty() {
        return Err(FundError::NothingToCancel);
    }
    if day_of_epoch(t.epoch)? <= s.last_fix_day
        || now_unix >= cancel_deadline(t.epoch, now_unix, cfg)?
    {
        return Err(FundError::TooLateToCancel);
    }
    let mut n = s.clone();
    roll_queue(&mut n, now_unix)?;
    let (queued_usdc, queued_shares) = queue_of(&mut n, t.epoch, now_unix)?;
    *queued_usdc = queued_usdc
        .checked_sub(t.subscription_usdc)
        .ok_or(FundError::Overflow)?;
    *queued_shares = queued_shares
        .checked_sub(t.redemption_shares)
        .ok_or(FundError::Overflow)?;
    let out = CancelOutcome {
        usdc: t.subscription_usdc,
        shares: t.redemption_shares,
    };
    *s = n;
    *t = Ticket::default();
    Ok(out)
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

/// 一次领取的结果。外层把份额从份额托管、赎回款从待领取账户、退款从申购托管转给用户。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimOutcome {
    pub shares: u64,
    pub redeemer_usdc: u64, // 赎回人实收：他那份赎回资产卖出所得减去取款费
    pub fee_usdc: u64,      // 取款费：记入 FundState.fees_owed，之后付给收费地址
    pub refund_usdc: u64,   // 申购退款（不收取款费）
}

/// 定价之后领取。
/// - 申购可以分几次领取已成交的部分：累计领到 已发行份额 × 本人申购额 ÷ 本期申购总额（向下取整）。
///   全部成交后最后一次领取，最后一位拿到本期剩下的全部份额。
/// - 申购退款（待成交申购已满时）在最后一次领取时按申购额比例返还，最后一位拿余数。
/// - 赎回要等这一期的赎回资产全部卖完，按份额比例分实际卖出的 USDC，最后一位拿余数。
///   其中 withdrawal_fee_bps 作为取款费欠给管理人（ADR 0007，记入 s.fees_owed），其余给赎回人；
///   份额和申购退款不收。费率按领取时的配置（只能通过时间锁修改，最多 1%）。
/// 领取之和（赎回人实收 + 取款费）精确等于本期总额，不留尘埃。两部分都领完后记录清空。
/// 没有新的可领取内容时返回 SliceNotSold（赎回资产还没卖完）或 NothingToClaim。出错时什么都不改。
pub fn claim(
    s: &mut FundState,
    t: &mut Ticket,
    r: &mut EpochRecord,
    cfg: &Config,
) -> Result<ClaimOutcome, FundError> {
    let (mut t2, mut r2) = (*t, *r);
    let out = claim_parts(&mut t2, &mut r2, cfg)?;
    s.fees_owed = s
        .fees_owed
        .checked_add(out.fee_usdc)
        .ok_or(FundError::Overflow)?;
    *t = t2;
    *r = r2;
    Ok(out)
}

/// 留下的申购人共有的 (已发行份额, 申购额)：去掉撤销了待成交申购的人
fn remaining_subscription_pool(r: &EpochRecord) -> Result<(u64, u64), FundError> {
    Ok((
        r.minted_shares
            .checked_sub(r.cancelled_shares)
            .ok_or(FundError::Overflow)?,
        r.subscription_usdc
            .checked_sub(r.cancelled_subscription_usdc)
            .ok_or(FundError::Overflow)?,
    ))
}

/// 撤销待成交申购的结果：外层从份额托管转出 shares，从申购托管退回 refund_usdc。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingCancel {
    pub shares: u64,      // 已成交、还没领取的份额
    pub refund_usdc: u64, // 没成交的申购款
}

/// 撤销已定价、还没成交完的申购，任何时候都可以（比如长时间脱锚买不了 cbBTC）。
/// 这个人离开这一期的申购：已成交的份额按比例一次领走，没成交的 USDC 按比例退回，
/// 都向下取整；最后一个撤销的人拿走剩下的全部，不留尘埃。
/// 留下的人每份申购额对应的份额和待成交 USDC 不减少（取整都对他们有利），之后的成交只归他们。
/// 待成交的 USDC 已经扣过申购费：申购费在定价时进了基金，撤销不退。
/// 同一张排队记录里的赎回不受影响。还没定价的排队记录用 cancel。
pub fn cancel_pending(
    s: &mut FundState,
    t: &mut Ticket,
    r: &mut EpochRecord,
) -> Result<PendingCancel, FundError> {
    if !r.covers(t.epoch) {
        return Err(FundError::EpochNotSettled);
    }
    if t.subscription_usdc == 0 || r.subscription_filled {
        return Err(FundError::NothingToCancel);
    }
    let n = s.pending_count as usize;
    let i = s.pending[..n]
        .iter()
        .position(|p| p.epoch == r.epoch)
        .ok_or(FundError::WrongEpochRecord)?;
    let part = t.subscription_usdc;
    let (minted, basis) = remaining_subscription_pool(r)?;
    let unfilled = s.pending[i].usdc;
    let last = part == basis;
    let (entitled, refund_usdc) = if last {
        (minted, unfilled)
    } else {
        (
            pro_rata(minted, part, basis)?,
            pro_rata(unfilled, part, basis)?,
        )
    };
    let shares = entitled
        .checked_sub(t.shares_claimed)
        .ok_or(FundError::Overflow)?;

    let mut rec = *r;
    rec.cancelled_subscription_usdc = rec
        .cancelled_subscription_usdc
        .checked_add(part)
        .ok_or(FundError::Overflow)?;
    rec.cancelled_shares = rec
        .cancelled_shares
        .checked_add(entitled)
        .ok_or(FundError::Overflow)?;
    rec.unclaimed_shares = rec
        .unclaimed_shares
        .checked_sub(shares)
        .ok_or(FundError::Overflow)?;
    rec.unclaimed_subscription_usdc = rec
        .unclaimed_subscription_usdc
        .checked_sub(part)
        .ok_or(FundError::Overflow)?;
    if last {
        // 没有人留下：这一期的申购结束，待成交的这一期移出队列
        rec.subscription_filled = true;
        s.remove_pending(i);
    } else {
        // 留下的人还有 unfilled − refund > 0（refund ≤ unfilled × part ÷ basis < unfilled）
        s.pending[i].usdc = unfilled
            .checked_sub(refund_usdc)
            .ok_or(FundError::Overflow)?;
    }
    *r = rec;
    t.subscription_usdc = 0;
    t.shares_claimed = 0;
    if t.is_empty() {
        *t = Ticket::default();
    }
    Ok(PendingCancel {
        shares,
        refund_usdc,
    })
}

/// 欠管理人的取款费全部结清：返回要从待领取账户付到收费地址的 USDC
pub fn collect_fees(s: &mut FundState) -> u64 {
    std::mem::take(&mut s.fees_owed)
}

fn claim_parts(
    t: &mut Ticket,
    r: &mut EpochRecord,
    cfg: &Config,
) -> Result<ClaimOutcome, FundError> {
    if t.is_empty() {
        return Err(FundError::NothingToClaim);
    }
    if !r.covers(t.epoch) {
        return Err(FundError::EpochNotSettled);
    }
    // 先算出两部分，再修改：没有进展时什么都不改
    let part = t.subscription_usdc;
    let sub_final = part > 0 && r.subscription_filled;
    let shares = if part == 0 {
        0
    } else if sub_final && part == r.unclaimed_subscription_usdc {
        r.unclaimed_shares // 最后一位
    } else {
        let (minted, basis) = remaining_subscription_pool(r)?;
        pro_rata(minted, part, basis)?
            .checked_sub(t.shares_claimed)
            .ok_or(FundError::Overflow)?
    };
    if shares > r.unclaimed_shares || part > r.unclaimed_subscription_usdc {
        return Err(FundError::Overflow);
    }
    let redeem_final = t.redemption_shares > 0 && r.slice_sold_out;
    if shares == 0 && !sub_final && !redeem_final {
        return Err(if t.redemption_shares > 0 {
            FundError::SliceNotSold
        } else {
            FundError::NothingToClaim
        });
    }

    r.unclaimed_shares -= shares; // 上面已检查 shares ≤ unclaimed_shares
    t.shares_claimed = t
        .shares_claimed
        .checked_add(shares)
        .ok_or(FundError::Overflow)?;
    let refund_usdc = if sub_final {
        t.subscription_usdc = 0;
        take_pro_rata(
            part,
            &mut r.unclaimed_subscription_usdc,
            &mut r.unclaimed_refund_usdc,
        )?
    } else {
        0
    };
    let gross_usdc = if redeem_final {
        let usdc = take_pro_rata(
            t.redemption_shares,
            &mut r.unclaimed_redemption_shares,
            &mut r.unclaimed_payout_usdc,
        )?;
        t.redemption_shares = 0;
        usdc
    } else {
        0
    };
    // 取款费从这个人那一份里扣，按规定向下取整。它不是基金资产，取整只在赎回人和管理人之间，
    // 留下的持有人不受影响
    let fee_usdc = pro_rata(gross_usdc, cfg.withdrawal_fee_bps as u64, BPS as u64)?;
    let redeemer_usdc = gross_usdc
        .checked_sub(fee_usdc)
        .ok_or(FundError::Overflow)?;
    if t.subscription_usdc == 0 && t.redemption_shares == 0 {
        *t = Ticket::default();
    }
    Ok(ClaimOutcome {
        shares,
        redeemer_usdc,
        fee_usdc,
        refund_usdc,
    })
}

// ---------------------------------------------------------------------------
// 定价步骤
// ---------------------------------------------------------------------------

/// 定价步骤：在定价时刻之后调用一次，开启当天的结算。
/// 关闭所有由这次定价处理的期：当天这一期，以及被跳过的定价顺延过来的更早几期；
/// 定价时刻之后才提交的请求属于下一期，留在排队里。
/// 外层据此转账：销毁赎回份额（record.redemption_shares）；基金主账户的 slice_usdc 和
/// 申购托管的 netted_usdc 转入待领取账户；申购费 fee_usdc 从申购托管转入主账户；
/// 为轧差发行 record.minted_shares 份。赎回资产的 cbBTC、待成交申购和退款的 USDC 留在原处。
///
/// 顺序：
///   1. 检查调用人和时间：清算窗口内只有清算地址；今天还没定价，且已过定价时刻；
///      两个定价价格都在定价窗口内发布
///   2. 记录信号价格，计算均线和目标
///   3. 销毁赎回份额，按份额比例从基金划出实物赎回资产（都向下取整，对留下的人有利）
///   4. 轧差：基金调仓已完成、目标是 cbBTC、没有脱锚时，本期申购款按估值价格换走
///      本期赎回资产里的 cbBTC，换到的 cbBTC 立即按数量发份额
///   5. 其余申购款成为待成交申购，排在更早的之后；待成交申购已满时全额退款
///
/// 无论什么时候调用，信号和轧差都只取决于定价价格。
/// 未卖完的赎回资产、基金调仓和待成交申购在之后的交易里继续。
pub fn fix(
    s: &mut FundState,
    now_unix: i64,
    prices: &FixPrices,
    caller: Caller,
    cfg: &Config,
) -> Result<FixOutcome, FundError> {
    cfg.validate()?;
    check_keeper_window(now_unix, caller, cfg)?;
    let chosen = choose_feed(prices.cbbtc, prices.btc, cfg)?;
    if chosen.price.price == 0 {
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
    let at_fix_time = |p: &PricePoint| {
        check_fix_window(p.publish_time, fix_at).is_ok() && p.publish_time <= now_unix
    };
    if !at_fix_time(&chosen.price) {
        return Err(FundError::PriceNotAtFixTime);
    }
    // 参照价格不是定价时刻发布的就不用：只是不轧差，定价照常
    let feeds = FeedChoice {
        price: chosen.price.price,
        reference: chosen
            .reference
            .filter(|p| p.price > 0 && at_fix_time(p))
            .map(|p| p.price),
        checks_peg: chosen.checks_peg,
    };

    // 在副本上计算，最后一次性写回：出错时状态不变
    let mut n = s.clone();
    roll_queue(&mut n, now_unix)?;
    let epoch = epoch_of_day(day)?;
    let price = feeds.price;

    // 2. 信号价格进均线，决定目标（信号价格就是选定价格源的定价价格）
    let signal = price;
    record_price(&mut n, day, signal);
    let ma = moving_average(&n, day, cfg.window);
    n.target_btc = matches!(ma, Some(m) if signal > m);

    // 3. 赎回资产：按份额比例划出实物，不需要价格
    let redemption_shares = n.queued_redemption_shares;
    let (slice_usdc, slice_btc) = if redemption_shares == 0 {
        (0, 0)
    } else {
        (
            pro_rata(n.usdc, redemption_shares, n.share_supply)?,
            pro_rata(n.btc, redemption_shares, n.share_supply)?,
        )
    };
    n.usdc = n.usdc.checked_sub(slice_usdc).ok_or(FundError::Overflow)?;
    n.btc = n.btc.checked_sub(slice_btc).ok_or(FundError::Overflow)?;
    n.share_supply = n
        .share_supply
        .checked_sub(redemption_shares)
        .ok_or(FundError::Overflow)?;
    n.queued_redemption_shares = 0;

    // 4. 轧差：申购人用估值价格换走赎回资产里的 cbBTC，这部分双方都不付交易成本。
    //    只在基金已经只有 cbBTC 时做，换来的 cbBTC 才能按数量发份额。
    let subscription_usdc = n.queued_subscription_usdc;
    // 待成交申购已满：本期申购不排队，全额退款（不收申购费），定价照常
    let refund = subscription_usdc > 0 && n.pending_count as usize >= MAX_PENDING;
    let net_subscription = if refund {
        0
    } else {
        apply_bps_down(subscription_usdc, cfg.subscription_fee_bps)?
    };
    let refund_usdc = if refund { subscription_usdc } else { 0 };
    let fee_usdc = subscription_usdc
        .checked_sub(net_subscription)
        .and_then(|x| x.checked_sub(refund_usdc))
        .ok_or(FundError::Overflow)?; // 申购费留在基金里
                                      // 还有更早的待成交申购时不轧差：它们先成交
    let can_net = n.target_btc
        && n.pending_count == 0
        && fund_rebalanced(&n, price)?
        && !feeds.blocks_buys(cfg);
    let (netted_usdc, netted_btc) = if can_net {
        let usdc = net_subscription.min(btc_value(slice_btc, price)?);
        // cbBTC 向下取整：usdc ≤ slice_btc 的价值，所以 netted_btc ≤ slice_btc
        match usdc_to_btc(usdc, price)? {
            0 => (0, 0),
            btc => (usdc, btc),
        }
    } else {
        (0, 0)
    };
    let netted_shares = shares_for(netted_btc, n.btc, n.share_supply, netted_usdc)?;
    n.btc = n.btc.checked_add(netted_btc).ok_or(FundError::Overflow)?;
    n.usdc = n.usdc.checked_add(fee_usdc).ok_or(FundError::Overflow)?;
    n.share_supply = n
        .share_supply
        .checked_add(netted_shares)
        .ok_or(FundError::Overflow)?;
    n.queued_subscription_usdc = 0;

    // 剩下要卖的赎回资产和待成交申购各自排队
    let slice = Slice {
        epoch,
        btc: slice_btc
            .checked_sub(netted_btc)
            .ok_or(FundError::Overflow)?,
        usdc: slice_usdc
            .checked_add(netted_usdc)
            .ok_or(FundError::Overflow)?,
    };
    let slice_sold_out = slice.btc == 0;
    if !slice_sold_out {
        if n.slice_count as usize >= MAX_SLICES {
            return Err(FundError::TooManySlices);
        }
        n.slices[n.slice_count as usize] = slice;
        n.slice_count += 1;
    }
    let pending_usdc = net_subscription
        .checked_sub(netted_usdc)
        .ok_or(FundError::Overflow)?;
    if pending_usdc > 0 {
        // 退款时 net_subscription 为 0，所以这里一定还有空位
        n.pending[n.pending_count as usize] = Pending {
            epoch,
            usdc: pending_usdc,
        };
        n.pending_count += 1;
    }
    let payout = if slice_sold_out { slice.usdc } else { 0 };

    let record = EpochRecord {
        epoch,
        first_epoch: epoch_of_day(s.last_fix_day.saturating_add(1))?,
        signal_price: signal,
        reference_price: feeds.reference.unwrap_or(0),
        moving_average: ma.unwrap_or(0),
        subscription_usdc,
        minted_shares: netted_shares,
        subscription_filled: pending_usdc == 0,
        netted_usdc,
        netted_btc,
        refund_usdc,
        redemption_shares,
        slice_usdc,
        slice_btc,
        payout_usdc: payout,
        slice_sold_out,
        target_btc: n.target_btc,
        unclaimed_subscription_usdc: subscription_usdc,
        unclaimed_shares: netted_shares,
        unclaimed_redemption_shares: redemption_shares,
        unclaimed_payout_usdc: payout,
        unclaimed_refund_usdc: refund_usdc,
        cancelled_subscription_usdc: 0,
        cancelled_shares: 0,
    };
    n.last_fix_day = day;
    // 定价时刻之后提交的请求（下一期）由下一次定价处理
    n.queued_redemption_shares = n.next_redemption_shares;
    n.queued_subscription_usdc = n.next_subscription_usdc;
    n.next_epoch = 0;
    n.next_redemption_shares = 0;
    n.next_subscription_usdc = 0;
    *s = n;
    Ok(FixOutcome { record, fee_usdc })
}

// ---------------------------------------------------------------------------
// 交易
// ---------------------------------------------------------------------------

/// 调用时各价格源的最新价格，没有这个价格源时为 None
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketPrices {
    pub cbbtc: Option<u64>, // cbBTC/USD
    pub btc: Option<u64>,   // BTC/USD
}

/// 一笔交易服务的对象
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeBucket {
    Slice { epoch: u64 },        // 卖出第 epoch 期的赎回资产，所得归这一期的赎回人
    Fund,                        // 把基金调向目标
    Subscription { epoch: u64 }, // 为第 epoch 期的待成交申购买入（或不交易直接成交）
}

impl TradeBucket {
    /// 这笔交易要更新哪一期的记录；基金调仓为 None
    pub fn record_epoch(&self) -> Option<u64> {
        match *self {
            TradeBucket::Slice { epoch } | TradeBucket::Subscription { epoch } => Some(epoch),
            TradeBucket::Fund => None,
        }
    }
}

/// 一笔交易。min_out 由执行时的估值价格和滑点上限算出，
/// 外部兑换（模拟交易所或 Jupiter）实际成交少于 min_out 必须失败。
/// side 为 WithoutTrade 时不兑换：amount_in 的申购款直接进入基金。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradePlan {
    pub side: Side,
    pub amount_in: u64,
    pub min_out: u64,
    pub bucket: TradeBucket,
    pub price: u64, // 算 min_out 用的估值价格（选定价格源的最新价格）；模拟交易所按它成交
}

/// 规划下一步。清算窗口内其他调用人得到 KeeperOnly。没有可做的时返回原因：
/// - FixPending：已过今天的定价时刻、还没定价，基金调仓和申购要等定价
/// - NothingToTrade：赎回资产都卖完了，基金也已经是目标持仓，申购都已成交
/// - DepegBlocksBuy：下一笔是买入 cbBTC，但 cbBTC 脱锚
/// - TradeTooSoon：距离上一笔交易不到最小间隔
///
/// 前三个原因先于 TradeTooSoon 判断：它们表示等多久都没有交易（直到下一次定价或恢复锚定），
/// 驱动交易的脚本据此停下；TradeTooSoon 表示稍后重试。
///
/// 优先级：最早一期未卖完的赎回资产，然后是基金调仓，最后是最早一期待成交申购。
/// 基金调仓完成后才处理申购：那时基金只有一种资产，才能按数量发份额。
/// 每笔按当前估值价格折算不超过 trade_cap_usdc。不交易的成交不受交易间隔限制。
pub fn plan_trade(
    s: &FundState,
    now_unix: i64,
    prices: &MarketPrices,
    caller: Caller,
    cfg: &Config,
) -> Result<TradePlan, FundError> {
    cfg.validate()?;
    check_keeper_window(now_unix, caller, cfg)?;
    let mut chosen = choose_feed(prices.cbbtc, prices.btc, cfg)?;
    if chosen.price == 0 {
        return Err(FundError::BadPrice);
    }
    chosen.reference = chosen.reference.filter(|&p| p > 0);
    let price = chosen.price;
    // 窗口之后的其他调用人用更严的滑点上限：能从一笔交易里挤出的价值更少
    let slip = match caller {
        Caller::Keeper => cfg.max_slippage_bps,
        Caller::Public => cfg.public_slippage_bps,
    };
    let depegged = chosen.blocks_buys(cfg);
    // 上限折算成 cbBTC 向下取整；至少 1 sat，保证价格再高也能卖完
    let cap_btc = usdc_to_btc(cfg.trade_cap_usdc, price)?.max(1);
    let sell = |amount: u64, bucket| -> Result<TradePlan, FundError> {
        Ok(TradePlan {
            side: Side::SellBtc,
            amount_in: amount,
            min_out: apply_bps_down(btc_value(amount, price)?, slip)?,
            bucket,
            price,
        })
    };
    let buy = |amount: u64, bucket| -> Result<TradePlan, FundError> {
        Ok(TradePlan {
            side: Side::BuyBtc,
            amount_in: amount,
            min_out: apply_bps_down(usdc_to_btc(amount, price)?, slip)?,
            bucket,
            price,
        })
    };
    let plan = if let Some(slice) = s.oldest_slice() {
        // 赎回资产无论多小都要卖完：min_out 为 0 时成交 0 也接受，赎回人承担这点尘埃
        sell(
            slice.btc.min(cap_btc),
            TradeBucket::Slice { epoch: slice.epoch },
        )?
    } else if now_unix >= fix_time(day_of(now_unix), cfg) && s.last_fix_day < day_of(now_unix) {
        // 新的定价时刻已到，目标可能会变：基金调仓和申购等定价之后按新目标进行
        // （赎回资产不算基金资产，照常卖出）
        return Err(FundError::FixPending);
    } else if !fund_rebalanced(s, price)? {
        if s.target_btc {
            if depegged {
                return Err(FundError::DepegBlocksBuy);
            }
            buy(s.usdc.min(cfg.trade_cap_usdc), TradeBucket::Fund)?
        } else {
            sell(s.btc.min(cap_btc), TradeBucket::Fund)?
        }
    } else if let Some(p) = s.oldest_pending() {
        let bucket = TradeBucket::Subscription { epoch: p.epoch };
        if !s.target_btc {
            // 目标是 USDC：申购款直接进入基金，不交易
            return Ok(TradePlan {
                side: Side::WithoutTrade,
                amount_in: p.usdc,
                min_out: 0,
                bucket,
                price,
            });
        }
        if depegged {
            return Err(FundError::DepegBlocksBuy);
        }
        // 申购款无论多少都要买完：min_out 为 0 时成交 0 也接受，申购人承担这点尘埃
        buy(p.usdc.min(cfg.trade_cap_usdc), bucket)?
    } else {
        return Err(FundError::NothingToTrade);
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
        Side::WithoutTrade => return Ok(0),
    };
    apply_bps_down(raw, fee_bps)
}

/// 记录一笔交易的实际成交 actual_out，返回新发行的份额（只有申购会发）。
/// 成交少于 min_out 返回 SlippageExceeded，状态不变（外层整笔交易回滚）。
///
/// 卖出赎回资产或为申购买入时，必须传入那一期的记录：
/// - 赎回资产卖完的那一笔把全部 USDC 写入记录的应付款，赎回人从此可以领取。
///   所得 USDC 由外层转入待领取账户，不进入基金。
/// - 申购买到的 cbBTC（目标为 USDC 时是申购款本身）进入基金，按数量发份额，写入记录；
///   全部成交后记录标记为已成交。外层把新份额发到份额托管账户。
pub fn fill_trade(
    s: &mut FundState,
    plan: &TradePlan,
    actual_out: u64,
    now_unix: i64,
    record: Option<&mut EpochRecord>,
) -> Result<u64, FundError> {
    if actual_out < plan.min_out {
        return Err(FundError::SlippageExceeded);
    }
    let mut minted = 0;
    match plan.bucket {
        TradeBucket::Slice { epoch } => {
            let r = record.ok_or(FundError::WrongEpochRecord)?;
            let slice = match s.oldest_slice().copied() {
                Some(sl) if sl.epoch == epoch && r.epoch == epoch => sl,
                _ => return Err(FundError::WrongEpochRecord),
            };
            if plan.side != Side::SellBtc {
                return Err(FundError::WrongEpochRecord);
            }
            let usdc = slice
                .usdc
                .checked_add(actual_out)
                .ok_or(FundError::Overflow)?;
            let btc = slice
                .btc
                .checked_sub(plan.amount_in)
                .ok_or(FundError::WrongEpochRecord)?;
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
            Side::WithoutTrade => return Err(FundError::WrongEpochRecord),
        },
        TradeBucket::Subscription { epoch } => {
            let r = record.ok_or(FundError::WrongEpochRecord)?;
            let p = match s.oldest_pending().copied() {
                Some(p) if p.epoch == epoch && r.epoch == epoch => p,
                _ => return Err(FundError::WrongEpochRecord),
            };
            let usdc_left = p
                .usdc
                .checked_sub(plan.amount_in)
                .ok_or(FundError::WrongEpochRecord)?;
            // 发份额用加入之前的基金资产：新份额 = 带来的资产 × 已发行份额 ÷ 基金同种资产
            match plan.side {
                Side::BuyBtc => {
                    minted = shares_for(actual_out, s.btc, s.share_supply, plan.amount_in)?;
                    s.btc = s.btc.checked_add(actual_out).ok_or(FundError::Overflow)?;
                }
                Side::WithoutTrade => {
                    minted = shares_for(plan.amount_in, s.usdc, s.share_supply, plan.amount_in)?;
                    s.usdc = s
                        .usdc
                        .checked_add(plan.amount_in)
                        .ok_or(FundError::Overflow)?;
                }
                Side::SellBtc => return Err(FundError::WrongEpochRecord),
            }
            s.share_supply = s
                .share_supply
                .checked_add(minted)
                .ok_or(FundError::Overflow)?;
            r.minted_shares = r
                .minted_shares
                .checked_add(minted)
                .ok_or(FundError::Overflow)?;
            r.unclaimed_shares = r
                .unclaimed_shares
                .checked_add(minted)
                .ok_or(FundError::Overflow)?;
            if usdc_left > 0 {
                s.pending[0].usdc = usdc_left;
            } else {
                r.subscription_filled = true;
                s.remove_pending(0); // 有最早的一期
            }
        }
    }
    // 不交易的成交不算一笔交易，不占用交易间隔
    if plan.side != Side::WithoutTrade {
        s.last_trade_time = now_unix;
    }
    Ok(minted)
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

    /// 两个价格源都在定价时刻发布：BTC/USD 和 cbBTC/USD
    fn at_fix2(day: i64, btc: u64, cbbtc: u64) -> FixPrices {
        let t = day * DAY + ONE_AM;
        FixPrices {
            btc: Some(PricePoint {
                price: btc,
                publish_time: t,
            }),
            cbbtc: Some(PricePoint {
                price: cbbtc,
                publish_time: t,
            }),
        }
    }

    /// 两个价格源在同一时刻发布同一个价格
    fn both(p: PricePoint) -> FixPrices {
        FixPrices {
            btc: Some(p),
            cbbtc: Some(p),
        }
    }

    /// 两个价格源的最新价格：BTC/USD 和 cbBTC/USD
    fn mp(btc: u64, cbbtc: u64) -> MarketPrices {
        MarketPrices {
            btc: Some(btc),
            cbbtc: Some(cbbtc),
        }
    }

    fn at_fix(day: i64, price: u64) -> FixPrices {
        at_fix2(day, price, price)
    }

    /// 基金状态加上每次定价的记录（按期号递增），模拟链上的账户。
    /// usdc_tokens / btc_tokens 按程序的转账记录基金名下的代币（托管、主账户、待领取账户），
    /// 用来核对内部记账：不能凭空多出或少掉资产。
    #[derive(Clone)]
    struct Harness {
        s: FundState,
        records: Vec<EpochRecord>,
        c: Config,
        usdc_tokens: u64,
        btc_tokens: u64,
        fees: u64, // 付给管理人的取款费
    }

    impl Harness {
        fn new() -> Self {
            Harness {
                s: FundState::new(),
                records: vec![],
                c: cfg(),
                usdc_tokens: 0,
                btc_tokens: 0,
                fees: 0,
            }
        }

        fn seeded(price: u64) -> Self {
            let mut h = Self::new();
            // 第 -40 到第 8 天
            seed_prices(&mut h.s, &vec![price; 49], 8, &h.c).unwrap();
            h
        }

        fn subscribe(&mut self, t: &mut Ticket, amount: u64, now: i64) {
            request_subscription(&mut self.s, t, amount, now, &self.c).unwrap();
            self.usdc_tokens += amount;
        }

        fn redeem(&mut self, t: &mut Ticket, shares: u64, now: i64) {
            request_redemption(&mut self.s, t, shares, now, &self.c).unwrap();
        }

        /// 撤销排队记录，退回的申购款离开基金名下
        fn cancel(&mut self, t: &mut Ticket, now: i64) -> Result<CancelOutcome, FundError> {
            let out = cancel(&mut self.s, t, now, &self.c)?;
            self.usdc_tokens -= out.usdc;
            self.assert_ledger();
            Ok(out)
        }

        /// 第 epoch 期的记录
        fn rec(&self, epoch: u64) -> EpochRecord {
            *self.records.iter().find(|r| r.epoch == epoch).unwrap()
        }

        /// 关闭第 epoch 期的那次定价的记录（期号是排队记录的期号）
        fn covering(&mut self, epoch: u64) -> Option<&mut EpochRecord> {
            self.records.iter_mut().find(|r| r.covers(epoch))
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
            let before = self.s.clone();
            let out = fix(&mut self.s, now, p, caller, &self.c)?;
            let r = out.record;
            // 每次定价接着上一次，关闭中间所有的期
            let first = self.records.last().map_or(0, |p| p.epoch + 1);
            assert_eq!(r.first_epoch, first);
            assert!(r.first_epoch <= r.epoch);
            self.records.push(r);
            // 赎回资产按比例向下取整，留下的人每份对应的两种资产都不减少；
            // 轧差发的份额向下取整，每份 cbBTC 不减少（USDC 只剩零头，允许被摊薄）。
            // 所有份额都赎回时没有留下的人，不用比较。
            if r.redemption_shares < before.share_supply {
                assert_asset_per_share_kept(&before, &self.s, true);
                if r.minted_shares == 0 {
                    assert_asset_per_share_kept(&before, &self.s, false);
                }
            }
            self.assert_ledger();
            Ok(r)
        }

        /// 基金名下的代币 = 基金资产 + 排队和待成交的申购款 + 未卖完的赎回资产
        /// + 待领取的赎回款和申购退款
        fn assert_ledger(&self) {
            let slices = &self.s.slices[..self.s.slice_count as usize];
            let payouts: u64 = self
                .records
                .iter()
                .filter(|r| r.slice_sold_out)
                .map(|r| r.unclaimed_payout_usdc + r.unclaimed_refund_usdc)
                .sum();
            let refunds_unsold: u64 = self
                .records
                .iter()
                .filter(|r| !r.slice_sold_out)
                .map(|r| r.unclaimed_refund_usdc)
                .sum();
            let pending = &self.s.pending[..self.s.pending_count as usize];
            assert_eq!(
                self.usdc_tokens,
                self.s.usdc
                    + self.s.queued_subscription_usdc
                    + self.s.next_subscription_usdc
                    + pending.iter().map(|x| x.usdc).sum::<u64>()
                    + slices.iter().map(|x| x.usdc).sum::<u64>()
                    + payouts
                    + refunds_unsold
                    + self.s.fees_owed
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

        /// 领取，返回 (份额, USDC)。USDC 是领取人那一份的总额：赎回款含取款费，加上申购退款。
        /// 取款费单独累计在 fees 里，并检查它正好是赎回款的 withdrawal_fee_bps（向下取整）。
        fn claim(&mut self, t: &mut Ticket) -> Result<(u64, u64), FundError> {
            self.claim_split(t)
                .map(|o| (o.shares, o.redeemer_usdc + o.fee_usdc + o.refund_usdc))
        }

        /// 撤销已定价、还没成交完的申购：退回没成交的 USDC，领走已成交的份额
        fn cancel_pending(&mut self, t: &mut Ticket) -> Result<PendingCancel, FundError> {
            let mut s = self.s.clone();
            let r = self.covering(t.epoch).ok_or(FundError::EpochNotSettled)?;
            let out = cancel_pending(&mut s, t, r)?;
            self.s = s;
            self.usdc_tokens -= out.refund_usdc;
            self.assert_ledger();
            Ok(out)
        }

        /// 结清取款费：从待领取账户付到收费地址
        fn collect_fees(&mut self) -> u64 {
            let fee = collect_fees(&mut self.s);
            self.usdc_tokens -= fee;
            self.assert_ledger();
            fee
        }

        /// 领取，返回完整结果（赎回人实收、取款费、退款分开）
        fn claim_split(&mut self, t: &mut Ticket) -> Result<ClaimOutcome, FundError> {
            let c = self.c;
            let mut s = self.s.clone();
            let r = self.covering(t.epoch).ok_or(FundError::EpochNotSettled)?;
            let out = claim(&mut s, t, r, &c)?;
            let gross = out.redeemer_usdc + out.fee_usdc;
            assert_eq!(
                out.fee_usdc as u128,
                gross as u128 * c.withdrawal_fee_bps as u128 / BPS
            );
            assert_eq!(s.fees_owed, self.s.fees_owed + out.fee_usdc);
            self.s = s;
            self.fees += out.fee_usdc;
            // 取款费留在待领取账户里，等 collect_fees
            self.usdc_tokens -= out.redeemer_usdc + out.refund_usdc;
            self.assert_ledger();
            Ok(out)
        }

        /// 成交一笔计划好的交易；卖出赎回资产或为申购买入时传入那一期的记录
        fn fill(&mut self, now: i64, plan: &TradePlan, got: u64) -> Result<(), FundError> {
            let r = match plan.bucket.record_epoch() {
                Some(e) => self.records.iter_mut().find(|r| r.epoch == e),
                None => None,
            };
            let before = self.s.clone();
            fill_trade(&mut self.s, plan, got, now, r)?;
            // 申购发份额按数量向下取整：已有持有人每份对应的基金资产不减少
            if let TradeBucket::Subscription { .. } = plan.bucket {
                assert_asset_per_share_kept(&before, &self.s, plan.side == Side::BuyBtc);
            }
            match plan.side {
                Side::BuyBtc => {
                    self.usdc_tokens -= plan.amount_in;
                    self.btc_tokens += got;
                }
                Side::SellBtc => {
                    self.btc_tokens -= plan.amount_in;
                    self.usdc_tokens += got;
                }
                Side::WithoutTrade => {} // 申购托管 → 基金主账户
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
            // 交易所按算 min_out 的那个价格成交
            self.fill(now, &plan, exec(&plan, plan.price, fee_bps))?;
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
            self.rec(r.epoch)
        }
    }

    fn same(price: u64) -> MarketPrices {
        mp(price, price)
    }

    /// 基金资产按价格折算的总值（只在测试里用来检查，基金本身从不按美元估值）
    fn fund_value(s: &FundState, price: u64) -> u128 {
        s.usdc as u128 + btc_value(s.btc, price).unwrap() as u128
    }

    /// 别人的申购赎回不减少已有持有人每份对应的基金资产（btc 为 true 时看 cbBTC，否则看 USDC）。
    /// 按数量比较，与价格无关：价格不变时，留下的人的价值只随基金自己的调仓变化。
    fn assert_asset_per_share_kept(before: &FundState, after: &FundState, btc: bool) {
        let (s0, s1) = (before.share_supply as u128, after.share_supply as u128);
        if s0 == 0 || s1 == 0 {
            return;
        }
        let (h0, h1) = if btc {
            (before.btc as u128, after.btc as u128)
        } else {
            (before.usdc as u128, after.usdc as u128)
        };
        assert!(h1 * s0 >= h0 * s1, "每份资产 {h0}/{s0} -> {h1}/{s1}");
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
        let r = h.settle_day(100, 60_000 * USDC, 0);
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
        h.settle_day(10, 50_000 * USDC, 0);
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
        let r = h.settle_day(10, 50_000 * USDC, 0);
        // 第一个人：1% 申购费留在基金里，990 USDC 换 990 份，每份对应的 USDC 多于 1
        assert_eq!(r.minted_shares, 990 * USDC);
        assert_eq!(h.s.usdc, 1_000 * USDC);

        // 赎回资产按份额比例划出，不再扣赎回费：赎回一半份额，拿到一半资产
        h.claim(&mut ta).unwrap();
        h.redeem(&mut ta, 495 * USDC, 11 * DAY);
        let r = h.fix_day(11, 50_000 * USDC);
        assert_eq!(r.slice_usdc, 500 * USDC);
        assert_eq!(h.s.usdc, 500 * USDC);
    }

    #[test]
    fn redeemers_pay_the_withdrawal_fee_on_gains_and_losses() {
        // A 在 51,000 买入；第 11 天涨到 60,000 时赎回一半，第 12 天跌到 45,000 时赎回剩下的
        let mut h = Harness::seeded(50_000 * USDC);
        let mut ta = Ticket::default();
        h.subscribe(&mut ta, 1_000 * USDC, 10 * DAY);
        h.settle_day(10, 51_000 * USDC, 0); // 1,960,784 sat
        let (a, _) = h.claim(&mut ta).unwrap();

        h.redeem(&mut ta, a / 2, 11 * DAY);
        h.settle_day(11, 60_000 * USDC, 0);
        // 980,392 sat × 60,000 = 588.2352 USDC；1% 取款费 5.882352，向下取整
        let out = h.claim_split(&mut ta).unwrap();
        assert_eq!((out.redeemer_usdc, out.fee_usdc), (582_352_848, 5_882_352));

        h.redeem(&mut ta, a - a / 2, 12 * DAY);
        let r = h.settle_day(12, 45_000 * USDC, 0);
        // 亏损也收：980,392 sat × 45,000 = 441.1764 USDC
        assert_eq!(r.payout_usdc, 441_176_400);
        let out = h.claim_split(&mut ta).unwrap();
        assert_eq!((out.redeemer_usdc, out.fee_usdc), (436_764_636, 4_411_764));
        assert_eq!(h.fees, 5_882_352 + 4_411_764);
        // 取款费攒在基金名下，结清时一次付给收费地址
        assert_eq!(h.s.fees_owed, h.fees);
        assert_eq!(h.collect_fees(), 5_882_352 + 4_411_764);
        assert_eq!((h.s.fees_owed, h.collect_fees()), (0, 0));
    }

    #[test]
    fn share_claims_and_refunds_pay_no_withdrawal_fee() {
        let mut h = Harness::seeded(50_000 * USDC);
        h.c.signal_feed = SignalFeed::CbBtc; // 两个价格源：cbBTC/USD 作信号，BTC/USD 检查脱锚
        let mut ta = Ticket::default();
        h.subscribe(&mut ta, 1_000 * USDC, 10 * DAY);
        h.settle_day(10, 51_000 * USDC, 0);
        let out = h.claim_split(&mut ta).unwrap();
        assert_eq!((out.shares, out.fee_usdc), (1_000 * USDC, 0));

        // 待成交申购满了之后的申购全额退款，也不收取款费
        let depeg = |day: i64| at_fix2(day, 53_000 * USDC, 51_000 * USDC);
        for day in 11..11 + MAX_PENDING as i64 {
            h.subscribe(&mut Ticket::default(), 100 * USDC, day * DAY);
            h.fix_with(day * DAY + ONE_AM, &depeg(day)).unwrap();
        }
        let day = 11 + MAX_PENDING as i64;
        let mut late = Ticket::default();
        h.subscribe(&mut late, 100 * USDC, day * DAY);
        h.fix_with(day * DAY + ONE_AM, &depeg(day)).unwrap();
        let out = h.claim_split(&mut late).unwrap();
        assert_eq!((out.refund_usdc, out.fee_usdc), (100 * USDC, 0));
        assert_eq!(h.fees, 0);
    }

    #[test]
    fn redeemer_payouts_plus_fees_equal_the_epoch_payout_exactly() {
        // 三个人的赎回份额除不尽：取款费各自向下取整，最后一位拿余数，加起来一分不差
        let mut h = Harness::seeded(50_000 * USDC);
        let mut t = [Ticket::default(); 3];
        for (i, amt) in [1_001, 2_003, 3_007].into_iter().enumerate() {
            h.subscribe(&mut t[i], amt * USDC + 7, 10 * DAY);
        }
        h.settle_day(10, 51_000 * USDC, 0);
        let held: Vec<u64> = (0..3).map(|i| h.claim(&mut t[i]).unwrap().0).collect();
        for i in 0..3 {
            h.redeem(&mut t[i], held[i] / 3 + 1, 11 * DAY);
        }
        let r = h.settle_day(11, 53_333 * USDC, 25);
        let outs: Vec<ClaimOutcome> = (0..3).map(|i| h.claim_split(&mut t[i]).unwrap()).collect();
        let users: u64 = outs.iter().map(|o| o.redeemer_usdc).sum();
        let fees: u64 = outs.iter().map(|o| o.fee_usdc).sum();
        assert_eq!(users + fees, r.payout_usdc);
        assert!(fees > 0 && fees <= r.payout_usdc / 100);
        assert_eq!(h.rec(11).unclaimed_payout_usdc, 0);
    }

    #[test]
    fn no_withdrawal_fee_when_the_rate_is_zero() {
        let mut h = Harness::new();
        h.c.withdrawal_fee_bps = 0;
        let mut ta = Ticket::default();
        h.subscribe(&mut ta, 1_000 * USDC, 10 * DAY);
        h.settle_day(10, 50_000 * USDC, 0);
        h.claim(&mut ta).unwrap();
        h.redeem(&mut ta, 400 * USDC, 11 * DAY);
        h.fix_day(11, 50_000 * USDC);
        let out = h.claim_split(&mut ta).unwrap();
        assert_eq!((out.redeemer_usdc, out.fee_usdc), (400 * USDC, 0));
    }

    #[test]
    fn claim_requires_settled_epoch_and_only_once() {
        let mut h = Harness::new();
        let mut t = Ticket::default();
        h.subscribe(&mut t, 5 * USDC, 1 * DAY);
        let r = h.settle_day(1, 1 * USDC, 0);
        // 记录必须关闭这张排队记录的那一期
        for (first, last) in [(t.epoch + 1, t.epoch + 3), (0, t.epoch - 1)] {
            let mut wrong = r;
            (wrong.first_epoch, wrong.epoch) = (first, last);
            assert_eq!(
                claim(&mut h.s.clone(), &mut t.clone(), &mut wrong, &h.c),
                Err(FundError::EpochNotSettled)
            );
        }
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
    fn moving_average_uses_the_latest_window_days() {
        let mut s = FundState::new();
        for (day, p) in [(1, 10), (2, 20), (3, 30), (4, 40)] {
            record_price(&mut s, day, p);
        }
        assert_eq!(moving_average(&s, 4, 3), Some(30)); // (20+30+40)/3
        assert_eq!(moving_average(&s, 3, 3), Some(20)); // 只看这一天及之前
        assert_eq!(moving_average(&FundState::new(), 4, 3), None);

        // 没有价格的一天跳过，用更早的一天补足 window 个
        let mut s = FundState::new();
        for (day, p) in [(1, 10), (2, 20), (4, 40)] {
            record_price(&mut s, day, p);
        }
        // (10+20+40)/3
        assert_eq!(moving_average(&s, 4, 3), Some(23));
        // 最多往回看 MAX_WINDOW 天：更早的价格已经不在存储里
        assert_eq!(moving_average(&s, 4 + MAX_WINDOW as i64, 3), None);
        assert_eq!(moving_average(&s, 4 + MAX_WINDOW as i64 - 1, 1), Some(40));
        // 一个格子只存一天：64 天之后同一个格子被新的一天覆盖
        record_price(&mut s, 4 + MAX_WINDOW as i64, 99);
        assert_eq!(moving_average(&s, 4 + MAX_WINDOW as i64, 1), Some(99));
        assert_eq!(moving_average(&s, 4, 1), Some(20));
    }

    #[test]
    fn a_gap_then_backfill_restores_the_50_day_average() {
        // 种子价格是第 -40 到第 8 天的 49 个 50,000；第 9 天定价，第 10 天没人定价
        let mut h = Harness::seeded(50_000 * USDC);
        h.fix_day(9, 50_000 * USDC);
        let fix11 = 11 * DAY + ONE_AM;
        let p11 = at_fix(11, 52_000 * USDC);

        // 不回填：第 10 天跳过，均线用更早的一天补足 50 个
        let r = fix(&mut h.s.clone(), fix11, &p11, Caller::Keeper, &h.c).unwrap();
        assert_eq!(r.record.moving_average, (49 * 50_000 + 52_000) * USDC / 50);

        // 任何人用第 10 天定价窗口内发布的价格回填，均线恢复成最近 50 个日历日
        let p10 = PricePoint {
            price: 60_000 * USDC,
            publish_time: 10 * DAY + ONE_AM + 5,
        };
        backfill_price(&mut h.s, 10, &both(p10), 11 * DAY, &h.c).unwrap();
        let r = h.fix_with(fix11, &p11).unwrap();
        assert_eq!(
            r.moving_average,
            (48 * 50_000 + 60_000 + 52_000) * USDC / 50
        );
    }

    #[test]
    fn backfill_only_fills_a_missing_day_with_its_fixing_window_price() {
        let mut h = Harness::seeded(50_000 * USDC);
        h.fix_day(9, 50_000 * USDC);
        let now = 12 * DAY + 5 * ONE_AM;
        let at = |day: i64, t: i64| PricePoint {
            price: 60_000 * USDC,
            publish_time: day * DAY + ONE_AM + t,
        };
        let s0 = h.s.clone();
        let run =
            |day: i64, p: PricePoint| backfill_price(&mut s0.clone(), day, &both(p), now, &h.c);
        // 已经有价格的一天（定价过的、种子价格）不能改
        assert_eq!(run(9, at(9, 0)), Err(FundError::PriceAlreadyRecorded));
        assert_eq!(run(8, at(8, 0)), Err(FundError::PriceAlreadyRecorded));
        // 只能回填过去的一天，而且还在存储范围内（最近 MAX_WINDOW 天）
        assert_eq!(run(12, at(12, 0)), Err(FundError::BackfillOutOfRange));
        assert_eq!(run(13, at(13, 0)), Err(FundError::BackfillOutOfRange));
        let oldest = 12 - MAX_WINDOW as i64 + 1;
        assert_eq!(
            run(oldest - 1, at(oldest - 1, 0)),
            Err(FundError::BackfillOutOfRange)
        );
        // 发布时间必须在那一天的定价窗口内
        let bad = Err(FundError::PriceNotAtFixTime);
        assert_eq!(run(10, at(10, -1)), bad);
        assert_eq!(run(10, at(10, FIX_WINDOW_SECS)), bad);
        assert_eq!(run(10, at(11, 0)), bad);
        assert_eq!(
            run(
                10,
                PricePoint {
                    price: 0,
                    publish_time: 10 * DAY + ONE_AM
                }
            ),
            Err(FundError::BadPrice)
        );
        // 失败时状态不变；成功只写这一天，之后不能再改
        assert_eq!(h.s, s0);
        backfill_price(&mut h.s, 10, &both(at(10, FIX_WINDOW_SECS - 1)), now, &h.c).unwrap();
        assert_eq!(
            backfill_price(&mut h.s, 10, &both(at(10, 0)), now, &h.c),
            Err(FundError::PriceAlreadyRecorded)
        );
        backfill_price(&mut h.s, oldest, &both(at(oldest, 0)), now, &h.c).unwrap();
        assert_eq!(moving_average(&h.s, 10, 1), Some(60_000 * USDC));
    }

    #[test]
    fn fix_price_must_be_published_in_fix_window() {
        let s = FundState::new();
        // 两个价格源：cbBTC/USD 作信号，BTC/USD 是参照
        let c = Config {
            signal_feed: SignalFeed::CbBtc,
            ..cfg()
        };
        let fix_at = 10 * DAY + ONE_AM;
        let now = fix_at + 5 * 3600;
        // 两个价格的发布时间分别设置
        let at = |ts: i64, tv: i64| FixPrices {
            btc: Some(PricePoint {
                price: USDC,
                publish_time: ts,
            }),
            cbbtc: Some(PricePoint {
                price: USDC,
                publish_time: tv,
            }),
        };
        let run_both = |now: i64, ts: i64, tv: i64| {
            fix(&mut s.clone(), now, &at(ts, tv), Caller::Keeper, &c).map(|_| ())
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
        // 选定的 cbBTC/USD 必须在窗口内；BTC/USD 参照不在窗口内时不用它（只是不轧差），定价照常
        assert_eq!(run_both(now, fix_at, now), bad);
        let r = fix(&mut s.clone(), now, &at(now, fix_at), Caller::Keeper, &c).unwrap();
        assert_eq!(r.record.reference_price, 0);
    }

    #[test]
    fn a_request_after_the_fixing_time_joins_the_next_epoch() {
        let mut h = Harness::seeded(50_000 * USDC);
        let fix_at = 10 * DAY + ONE_AM;
        let (mut ta, mut tb, mut tc) = (Ticket::default(), Ticket::default(), Ticket::default());
        h.subscribe(&mut ta, 100 * USDC, fix_at - 1); // 00:59:59：第 10 期
                                                      // 定价价格已经公开、当天还没定价：排队照常开放，进入第 11 期
        h.subscribe(&mut tb, 200 * USDC, fix_at);
        assert_eq!((ta.epoch, tb.epoch), (10, 11));
        let r = h.settle_day(10, 50_000 * USDC, 0);
        assert_eq!(
            (r.first_epoch, r.epoch, r.subscription_usdc),
            (0, 10, 100 * USDC)
        );
        assert_eq!(h.claim(&mut tb), Err(FundError::EpochNotSettled));
        // 定价之后提交的也属于第 11 期；还没领取的第 10 期记录不能再追加
        h.subscribe(&mut tc, 300 * USDC, fix_at + 3_600);
        assert_eq!(tc.epoch, 11);
        assert_eq!(
            request_subscription(&mut h.s, &mut ta.clone(), USDC, fix_at + 3_600, &h.c),
            Err(FundError::WrongEpoch)
        );
        h.claim(&mut ta).unwrap();

        let r = h.settle_day(11, 50_000 * USDC, 0);
        assert_eq!(
            (r.first_epoch, r.epoch, r.subscription_usdc),
            (11, 11, 500 * USDC)
        );
        assert_eq!(h.claim(&mut tb).unwrap(), (200 * USDC, 0));
        assert_eq!(h.claim(&mut tc).unwrap(), (300 * USDC, 0));
    }

    #[test]
    fn requests_of_skipped_fixings_roll_into_the_next_one() {
        let mut h = Harness::seeded(50_000 * USDC);
        h.settle_day(9, 50_000 * USDC, 0);
        let mut t = [Ticket::default(); 4];
        h.subscribe(&mut t[0], 100 * USDC, 10 * DAY); // 第 10 期
        h.subscribe(&mut t[1], 200 * USDC, 10 * DAY + 2 * ONE_AM); // 第 11 期，第 10 天没人定价
        h.subscribe(&mut t[2], 300 * USDC, 11 * DAY + 5 * ONE_AM); // 第 12 期，第 11 天也没人定价
        h.subscribe(&mut t[3], 400 * USDC, 12 * DAY + 2 * ONE_AM); // 第 13 期：定价时刻之后
        assert_eq!(t.map(|x| x.epoch), [10, 11, 12, 13]);

        // 第 12 天 3 点才定价：关闭第 10–12 期，合在一起处理；第 13 期等下一次
        let fix_at = 12 * DAY + 3 * ONE_AM;
        let r = h.fix_with(fix_at, &at_fix(12, 50_000 * USDC)).unwrap();
        assert_eq!(
            (r.first_epoch, r.epoch, r.subscription_usdc),
            (10, 12, 600 * USDC)
        );
        let (_, why, _) = h.trade_until_done(fix_at, same(50_000 * USDC), 0);
        assert_eq!(why, FundError::NothingToTrade);
        assert_eq!(h.claim(&mut t[0]).unwrap(), (100 * USDC, 0));
        assert_eq!(h.claim(&mut t[1]).unwrap(), (200 * USDC, 0));
        assert_eq!(h.claim(&mut t[2]).unwrap(), (300 * USDC, 0));
        assert_eq!(h.claim(&mut t[3]), Err(FundError::EpochNotSettled));
        assert_eq!(h.rec(12).unclaimed_shares, 0);

        let r = h.settle_day(13, 50_000 * USDC, 0);
        assert_eq!((r.first_epoch, r.subscription_usdc), (13, 400 * USDC));
        assert_eq!(h.claim(&mut t[3]).unwrap(), (400 * USDC, 0));
    }

    #[test]
    fn a_user_can_have_requests_in_several_epochs() {
        let mut h = Harness::seeded(50_000 * USDC);
        let mut t10 = Ticket::default();
        h.subscribe(&mut t10, 1_000 * USDC, 10 * DAY);
        h.settle_day(10, 50_000 * USDC, 0);
        h.claim(&mut t10).unwrap();

        // 第 10 天定价之后：赎回 400 份、申购 100，都进第 11 期这张记录
        let (mut t11, mut t12) = (Ticket::default(), Ticket::default());
        h.redeem(&mut t11, 400 * USDC, 10 * DAY + 2 * ONE_AM);
        h.subscribe(&mut t11, 100 * USDC, 10 * DAY + 3 * ONE_AM);
        // 第 11 天定价时刻之后、定价之前：同一个用户再开第 12 期
        let late = 11 * DAY + 2 * ONE_AM;
        assert_eq!(
            request_subscription(&mut h.s, &mut t11.clone(), 200 * USDC, late, &h.c),
            Err(FundError::WrongEpoch)
        );
        h.subscribe(&mut t12, 200 * USDC, late);
        assert_eq!((t11.epoch, t12.epoch), (11, 12));

        // 第 11 天的定价只处理第 11 期
        h.fix_with(11 * DAY + 3 * ONE_AM, &at_fix(11, 50_000 * USDC))
            .unwrap();
        let (_, why, _) = h.trade_until_done(11 * DAY + 3 * ONE_AM, same(50_000 * USDC), 0);
        assert_eq!(why, FundError::NothingToTrade);
        assert_eq!(h.claim(&mut t11).unwrap(), (100 * USDC, 400 * USDC));
        assert_eq!(h.claim(&mut t12), Err(FundError::EpochNotSettled));
        h.settle_day(12, 50_000 * USDC, 0);
        assert_eq!(h.claim(&mut t12).unwrap(), (200 * USDC, 0));
        assert_eq!(h.s.share_supply, 900 * USDC);
    }

    #[test]
    fn a_request_can_be_cancelled_until_its_fixing_time() {
        let mut h = Harness::seeded(50_000 * USDC);
        let mut holder = Ticket::default();
        h.subscribe(&mut holder, 1_000 * USDC, 9 * DAY);
        h.settle_day(9, 50_000 * USDC, 0);
        h.claim(&mut holder).unwrap();
        let fix_at = 10 * DAY + ONE_AM;

        // 定价时刻之前撤销：申购款和赎回份额全额退回
        let mut ta = Ticket::default();
        h.subscribe(&mut ta, 300 * USDC, 10 * DAY);
        h.redeem(&mut ta, 200 * USDC, 10 * DAY);
        assert_eq!(
            h.cancel(&mut ta, fix_at - 1),
            Ok(CancelOutcome {
                usdc: 300 * USDC,
                shares: 200 * USDC
            })
        );
        assert!(ta.is_empty());
        assert_eq!(
            h.cancel(&mut ta, fix_at - 1),
            Err(FundError::NothingToCancel)
        );

        // 定价时刻之后不能撤销，即使当天还没定价：定价价格已经公开
        let mut tb = Ticket::default();
        h.subscribe(&mut tb, 50 * USDC, 10 * DAY);
        let before = h.s.clone();
        assert_eq!(h.cancel(&mut tb, fix_at), Err(FundError::TooLateToCancel));
        assert_eq!(h.s, before);
        // 定价时刻之后提交的属于下一期，在下一期的定价时刻之前可以撤销
        let (mut tc, mut td) = (Ticket::default(), Ticket::default());
        h.subscribe(&mut tc, 500 * USDC, fix_at + 60);
        h.subscribe(&mut td, 70 * USDC, fix_at + 60);
        assert_eq!(h.cancel(&mut tc, fix_at + 120).unwrap().usdc, 500 * USDC);

        // 第 10 天没人定价：tb 顺延，第 11 天的定价时刻之前又可以撤销
        let fix11 = 11 * DAY + ONE_AM;
        assert_eq!(h.cancel(&mut tb, fix11 - 1).unwrap().usdc, 50 * USDC);
        assert_eq!(h.cancel(&mut td, fix11), Err(FundError::TooLateToCancel));
        let r = h.fix_day(11, 50_000 * USDC);
        assert_eq!(
            (r.first_epoch, r.subscription_usdc, r.redemption_shares),
            (10, 70 * USDC, 0)
        );
        // 定价之后更不能撤销
        assert_eq!(h.cancel(&mut td, 12 * DAY), Err(FundError::TooLateToCancel));
        let (_, why, _) = h.trade_until_done(fix11, same(50_000 * USDC), 0);
        assert_eq!(why, FundError::NothingToTrade);
        assert_eq!(h.claim(&mut td).unwrap(), (70 * USDC, 0));
        assert_eq!(h.s.share_supply, 1_070 * USDC);
    }

    #[test]
    fn config_changes_wait_for_the_timelock() {
        let now = 10 * DAY;
        // 提议时就校验：取款费率最多 1%
        let bad = Config {
            withdrawal_fee_bps: 101,
            ..cfg()
        };
        assert_eq!(
            propose_config(&cfg(), &bad, now),
            Err(FundError::InvalidConfig)
        );
        let new = Config {
            trade_cap_usdc: 50_000 * USDC,
            keeper_window_secs: 600,
            ..cfg()
        };
        let at = propose_config(&cfg(), &new, now).unwrap();
        assert_eq!(at, now + 48 * 3_600);
        assert_eq!(check_timelock(at, now), Err(FundError::TimelockNotElapsed));
        assert_eq!(
            check_timelock(at, at - 1),
            Err(FundError::TimelockNotElapsed)
        );
        assert_eq!(check_timelock(at, at), Ok(()));
    }

    #[test]
    fn the_fixing_time_and_the_moving_average_window_cannot_be_changed() {
        // 改定价时刻会让已经定价过的一期重新开放（新请求进到已经定价的期里，冒领别人的份额）；
        // 改均线窗口会让均线暂时算不出来、基金悄悄换成 USDC。两者都不能通过治理修改。
        let fixed = [
            Config {
                settle_offset_secs: 2 * 3_600,
                ..cfg()
            },
            Config {
                window: 60,
                ..cfg()
            },
        ];
        for new in fixed {
            assert_eq!(
                propose_config(&cfg(), &new, 10 * DAY),
                Err(FundError::ConfigFieldFixed),
                "{new:?}"
            );
        }
    }

    #[test]
    fn subscriptions_are_capped_per_epoch_including_pending() {
        let mut h = Harness::seeded(50_000 * USDC);
        h.c.signal_feed = SignalFeed::CbBtc; // 两个价格源：cbBTC/USD 作信号，BTC/USD 检查脱锚
        assert_eq!(h.c.subscription_cap_usdc, 1_000_000 * USDC);
        let (mut ta, mut tb, mut holder) =
            (Ticket::default(), Ticket::default(), Ticket::default());
        h.subscribe(&mut holder, 1_000 * USDC, 9 * DAY);
        h.settle_day(9, 51_000 * USDC, 0);
        h.claim(&mut holder).unwrap();

        // 一期最多 100 万 USDC：正好到上限可以，再多 1 USDC 被拒绝，状态不变
        h.subscribe(&mut ta, 600_000 * USDC, 10 * DAY);
        h.subscribe(&mut tb, 400_000 * USDC, 10 * DAY);
        let before = h.s.clone();
        let capped = Err(FundError::SubscriptionCapExceeded);
        let mut tc = Ticket::default();
        assert_eq!(
            request_subscription(&mut h.s, &mut tc, USDC, 10 * DAY, &h.c),
            capped
        );
        assert_eq!((h.s.clone(), tc), (before, Ticket::default()));
        // 赎回不设上限
        h.redeem(&mut holder, 100 * USDC, 10 * DAY);

        // 脱锚，一笔都没买：这 100 万成为待成交申购，算进下一期的上限
        let depeg = |day: i64| at_fix2(day, 53_000 * USDC, 51_000 * USDC);
        h.fix_with(10 * DAY + ONE_AM, &depeg(10)).unwrap();
        assert_eq!(
            request_subscription(&mut h.s, &mut tc, USDC, 10 * DAY + 2 * ONE_AM, &h.c),
            capped
        );
        // 撤销待成交申购之后腾出额度
        assert_eq!(
            h.cancel_pending(&mut tb).unwrap().refund_usdc,
            400_000 * USDC
        );
        h.subscribe(&mut tc, 400_000 * USDC, 10 * DAY + 2 * ONE_AM);
        assert_eq!(
            request_subscription(&mut h.s, &mut tc, USDC, 10 * DAY + 2 * ONE_AM, &h.c),
            capped
        );
    }

    #[test]
    fn the_cap_also_counts_requests_waiting_for_the_next_epoch() {
        // 定价时刻之后、当天定价之前提交的属于下一期，但和这一期一起算上限
        let mut h = Harness::seeded(50_000 * USDC);
        h.c.subscription_cap_usdc = 1_000 * USDC;
        h.subscribe(&mut Ticket::default(), 700 * USDC, 10 * DAY);
        let late = 10 * DAY + 2 * ONE_AM;
        let mut tb = Ticket::default();
        h.subscribe(&mut tb, 300 * USDC, late);
        assert_eq!(tb.epoch, 11);
        assert_eq!(
            request_subscription(&mut h.s, &mut Ticket::default(), USDC, late, &h.c),
            Err(FundError::SubscriptionCapExceeded)
        );
        // 当天定价、全部成交（目标 USDC，不交易）之后，额度只剩第 11 期的 300
        h.settle_day(10, 50_000 * USDC, 0);
        h.subscribe(&mut Ticket::default(), 700 * USDC, late + 60);
    }

    #[test]
    fn a_pending_cancel_refunds_the_unfilled_usdc_after_the_subscription_fee() {
        let mut h = Harness::seeded(50_000 * USDC);
        h.c.signal_feed = SignalFeed::CbBtc; // 两个价格源：cbBTC/USD 作信号，BTC/USD 检查脱锚
        h.c.subscription_fee_bps = 100;
        let mut ta = Ticket::default();
        h.subscribe(&mut ta, 1_000 * USDC, 10 * DAY);
        // 脱锚：一笔都买不了。申购费 10 USDC 在定价时进了基金，待成交 990
        h.fix_with(
            10 * DAY + ONE_AM,
            &at_fix2(10, 53_000 * USDC, 51_000 * USDC),
        )
        .unwrap();
        assert_eq!(h.s.usdc, 10 * USDC);
        let out = h.cancel_pending(&mut ta).unwrap();
        assert_eq!((out.shares, out.refund_usdc), (0, 990 * USDC));
        assert_eq!(h.s.pending_count, 0);
    }

    #[test]
    fn cancelling_a_pending_subscription_refunds_the_unfilled_part_and_keeps_filled_shares() {
        let mut h = Harness::seeded(50_000 * USDC);
        h.c.trade_cap_usdc = 1_000 * USDC;
        let (mut ta, mut tb) = (Ticket::default(), Ticket::default());
        h.subscribe(&mut ta, 1_500 * USDC, 10 * DAY);
        h.subscribe(&mut tb, 500 * USDC, 10 * DAY);
        h.fix_day(10, 51_000 * USDC);
        let t0 = 10 * DAY + ONE_AM;
        // 第一笔买 1000 USDC，发 1000 份；还有 1000 USDC 没成交
        h.trade_at(t0, same(51_000 * USDC), 0).unwrap();
        // A 领过一部分之后撤销：已成交的 3/4 份额全部给他，没成交的 1000 USDC 退回他那 3/4
        assert_eq!(h.claim(&mut ta).unwrap(), (750 * USDC, 0));
        let out = h.cancel_pending(&mut ta).unwrap();
        assert_eq!(
            out,
            PendingCancel {
                shares: 0,
                refund_usdc: 750 * USDC
            }
        );
        assert!(ta.is_empty());
        assert_eq!(h.s.oldest_pending().map(|p| p.usdc), Some(250 * USDC));

        // 之后买到的只归 B：他领到自己那 250 份加上剩下 250 USDC 买到的全部份额
        let (plans, why, _) = h.trade_until_done(t0 + 60, same(50_000 * USDC), 0);
        assert_eq!((plans.len(), why), (1, FundError::NothingToTrade));
        let r = h.rec(10);
        assert!(r.subscription_filled);
        let (b, _) = h.claim(&mut tb).unwrap();
        assert_eq!(b, r.minted_shares - 750 * USDC);
        assert!(b > 500 * USDC);
        assert_eq!(
            (
                h.rec(10).unclaimed_shares,
                h.rec(10).unclaimed_subscription_usdc
            ),
            (0, 0)
        );
        assert_eq!(h.s.share_supply, 750 * USDC + b);
    }

    #[test]
    fn the_last_pending_subscriber_to_cancel_takes_everything_left() {
        let mut h = Harness::seeded(50_000 * USDC);
        h.c.trade_cap_usdc = 1_000 * USDC;
        let (mut ta, mut tb) = (Ticket::default(), Ticket::default());
        h.subscribe(&mut ta, 1_001 * USDC, 10 * DAY);
        h.subscribe(&mut tb, 2_002 * USDC + 1, 10 * DAY);
        h.fix_day(10, 51_000 * USDC);
        h.trade_at(10 * DAY + ONE_AM, same(51_000 * USDC), 0)
            .unwrap();
        // 两人都撤销：第二个是最后一位，拿走剩下的全部份额和 USDC，没有尘埃
        let a = h.cancel_pending(&mut ta).unwrap();
        let b = h.cancel_pending(&mut tb).unwrap();
        let r = h.rec(10);
        assert_eq!(a.shares + b.shares, r.minted_shares);
        assert_eq!(
            a.refund_usdc + b.refund_usdc,
            3_003 * USDC + 1 - 1_000 * USDC
        );
        assert!(r.subscription_filled);
        assert_eq!((r.unclaimed_shares, r.unclaimed_subscription_usdc), (0, 0));
        assert_eq!(h.s.pending_count, 0);
        assert_eq!(
            h.trade_at(10 * DAY + ONE_AM + 60, same(51_000 * USDC), 0),
            Err(FundError::NothingToTrade)
        );
    }

    #[test]
    fn only_a_pending_subscription_can_be_cancelled_after_its_fixing() {
        let mut h = Harness::seeded(50_000 * USDC);
        let (mut ta, mut tb) = (Ticket::default(), Ticket::default());
        h.subscribe(&mut ta, 1_000 * USDC, 10 * DAY);
        // 还没定价：用 cancel，不是 cancel_pending
        assert_eq!(h.cancel_pending(&mut ta), Err(FundError::EpochNotSettled));
        h.settle_day(10, 51_000 * USDC, 0);
        // 已经全部成交：没有可撤销的，直接领取
        assert_eq!(h.cancel_pending(&mut ta), Err(FundError::NothingToCancel));
        h.claim(&mut ta).unwrap();
        // 只有赎回的排队记录也没有可撤销的申购
        h.redeem(&mut tb, 100 * USDC, 11 * DAY);
        h.fix_day(11, 51_000 * USDC);
        assert_eq!(h.cancel_pending(&mut tb), Err(FundError::NothingToCancel));
    }

    #[test]
    fn pausing_blocks_new_subscriptions_only() {
        let mut h = Harness::seeded(50_000 * USDC);
        let mut ta = Ticket::default();
        h.subscribe(&mut ta, 1_000 * USDC, 10 * DAY);
        h.settle_day(10, 51_000 * USDC, 0);
        h.claim(&mut ta).unwrap();

        // 暂停之前已经排队的申购照常处理；暂停之后新的申购被拒绝
        let (mut tb, mut tc, mut tr) = (Ticket::default(), Ticket::default(), Ticket::default());
        h.subscribe(&mut tb, 500 * USDC, 11 * DAY);
        h.subscribe(&mut tc, 200 * USDC, 11 * DAY);
        h.s.subscriptions_paused = true;
        let before = h.s.clone();
        assert_eq!(
            request_subscription(&mut h.s, &mut Ticket::default(), 100 * USDC, 11 * DAY, &h.c),
            Err(FundError::SubscriptionsPaused)
        );
        assert_eq!(h.s, before);

        // 赎回、撤销、定价、交易和领取都照常
        h.redeem(&mut tr, 300 * USDC, 11 * DAY);
        assert_eq!(h.cancel(&mut tc, 11 * DAY).unwrap().usdc, 200 * USDC);
        let r = h.settle_day(11, 52_000 * USDC, 0);
        assert_eq!(
            (r.subscription_usdc, r.redemption_shares),
            (500 * USDC, 300 * USDC)
        );
        assert!(h.claim(&mut tb).unwrap().0 > 0);
        assert!(h.claim(&mut tr).unwrap().1 > 0);

        // 恢复之后又可以申购
        h.s.subscriptions_paused = false;
        h.subscribe(&mut Ticket::default(), 100 * USDC, 12 * DAY);
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
            // 取款费率在代码里封顶 1%
            Config {
                withdrawal_fee_bps: 101,
                ..cfg()
            },
            // 申购上限不能低于最低申购金额
            Config {
                subscription_cap_usdc: USDC - 1,
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
        assert_eq!(cfg().withdrawal_fee_bps, 100);
        assert_eq!(cfg().subscription_cap_usdc, 1_000_000 * USDC);
        assert_eq!(cfg().signal_feed, SignalFeed::Btc);
    }

    #[test]
    fn the_moving_average_records_the_chosen_feeds_fixing_price() {
        let mut h = Harness::seeded(50_000 * USDC);
        let mut t = Ticket::default();
        h.subscribe(&mut t, 1_000 * USDC, 10 * DAY);
        h.settle_day(10, 51_000 * USDC, 0);
        h.claim(&mut t).unwrap();
        // BTC/USD 52,000，cbBTC/USD 51,500（约低 1%，未脱锚）
        let fix11 = 11 * DAY + ONE_AM;
        let p = at_fix2(11, 52_000 * USDC, 51_500 * USDC);

        // 用 cbBTC/USD：均线记录 cbBTC 的定价价格，BTC/USD 只作脱锚检查的参照
        let mut cb = h.clone();
        cb.c.signal_feed = SignalFeed::CbBtc;
        let r = cb.fix_with(fix11, &p).unwrap();
        assert_eq!(
            (r.signal_price, r.reference_price),
            (51_500 * USDC, 52_000 * USDC)
        );
        assert_eq!(
            r.moving_average,
            (48 * 50_000 + 51_000 + 51_500) * USDC / 50
        );

        // 默认用 BTC/USD：均线记录 BTC 的定价价格，没有参照
        assert_eq!(h.c.signal_feed, SignalFeed::Btc);
        let r = h.fix_with(fix11, &p).unwrap();
        assert_eq!((r.signal_price, r.reference_price), (52_000 * USDC, 0));
        assert_eq!(
            r.moving_average,
            (48 * 50_000 + 51_000 + 52_000) * USDC / 50
        );
        assert!(r.target_btc && h.s.target_btc);
    }

    #[test]
    fn the_depeg_check_runs_only_when_both_feeds_exist() {
        let mut h = Harness::seeded(50_000 * USDC);
        h.c.signal_feed = SignalFeed::CbBtc; // 两个价格源：cbBTC/USD 作信号，BTC/USD 检查脱锚
        let mut t = Ticket::default();
        h.subscribe(&mut t, 1_000 * USDC, 10 * DAY);
        h.fix_day(10, 51_000 * USDC);
        let now = 10 * DAY + 2 * ONE_AM;
        let plan = |h: &Harness, m: MarketPrices| plan_trade(&h.s, now, &m, Caller::Keeper, &h.c);
        let min_out_at =
            |price: u64| apply_bps_down(usdc_to_btc(1_000 * USDC, price).unwrap(), 30).unwrap();
        // 用 cbBTC/USD：两个价格源都有、相差 3% 时不买入；不脱锚时按 cbBTC 的价格买入
        let gap = mp(53_000 * USDC, 51_400 * USDC);
        assert_eq!(plan(&h, gap), Err(FundError::DepegBlocksBuy));
        let p = plan(&h, mp(51_500 * USDC, 51_400 * USDC)).unwrap();
        assert_eq!(p.min_out, min_out_at(51_400 * USDC));
        // 缺少 BTC/USD 参照时没法确认没有脱锚：也不买入（少传参照绕不过检查）
        let cb_only = MarketPrices {
            cbbtc: Some(51_400 * USDC),
            btc: None,
        };
        assert_eq!(plan(&h, cb_only), Err(FundError::DepegBlocksBuy));
        // 缺少选定的 cbBTC/USD：价格无效
        let no_cb = MarketPrices {
            cbbtc: None,
            btc: Some(51_000 * USDC),
        };
        assert_eq!(plan(&h, no_cb), Err(FundError::BadPrice));

        // 用 BTC/USD（没有 cbBTC/USD 价格源）：cbBTC/USD 不用，不检查脱锚，min_out 按 BTC 的价格
        h.c.signal_feed = SignalFeed::Btc;
        let p = plan(&h, gap).unwrap();
        assert_eq!(
            (p.price, p.min_out),
            (53_000 * USDC, min_out_at(53_000 * USDC))
        );
        let no_btc = MarketPrices {
            cbbtc: Some(51_000 * USDC),
            btc: None,
        };
        assert_eq!(plan(&h, no_btc), Err(FundError::BadPrice));
    }

    #[test]
    fn a_missing_reference_blocks_buys_but_not_sells_or_fixing() {
        // A 持有 cbBTC，赎回一半；B 申购。定价时 BTC/USD 参照缺失或不是定价时刻发布的：
        // 照常定价，但不轧差；赎回资产照常卖出，买入等参照恢复
        let mut h = Harness::seeded(50_000 * USDC);
        h.c.signal_feed = SignalFeed::CbBtc; // 两个价格源：cbBTC/USD 作信号，BTC/USD 检查脱锚
        let (mut ta, mut tb) = (Ticket::default(), Ticket::default());
        h.subscribe(&mut ta, 1_000 * USDC, 10 * DAY);
        h.settle_day(10, 51_000 * USDC, 0);
        let (a, _) = h.claim(&mut ta).unwrap();
        h.redeem(&mut ta, a / 2, 11 * DAY);
        h.subscribe(&mut tb, 300 * USDC, 11 * DAY);
        let fix11 = 11 * DAY + ONE_AM;
        let cb = PricePoint {
            price: 52_000 * USDC,
            publish_time: fix11,
        };
        let late_btc = PricePoint {
            price: 52_000 * USDC,
            publish_time: fix11 + FIX_WINDOW_SECS,
        };
        for btc in [None, Some(late_btc)] {
            let r = fix(
                &mut h.s.clone(),
                fix11 + 120,
                &FixPrices {
                    cbbtc: Some(cb),
                    btc,
                },
                Caller::Keeper,
                &h.c,
            )
            .unwrap()
            .record;
            assert!(r.target_btc && r.slice_btc > 0);
            assert_eq!((r.netted_btc, r.reference_price), (0, 0), "不轧差");
        }
        // 选定的 cbBTC/USD 缺失或不是定价时刻发布的：不能定价
        let no_cb = FixPrices {
            cbbtc: None,
            btc: Some(cb),
        };
        assert_eq!(
            fix(&mut h.s.clone(), fix11 + 120, &no_cb, Caller::Keeper, &h.c),
            Err(FundError::BadPrice)
        );
        let late_cb = FixPrices {
            cbbtc: Some(late_btc),
            btc: Some(cb),
        };
        assert_eq!(
            fix(
                &mut h.s.clone(),
                fix11 + 120,
                &late_cb,
                Caller::Keeper,
                &h.c
            ),
            Err(FundError::PriceNotAtFixTime)
        );

        h.fix_with(
            fix11 + 120,
            &FixPrices {
                cbbtc: Some(cb),
                btc: None,
            },
        )
        .unwrap();
        let cb_only = MarketPrices {
            cbbtc: Some(52_000 * USDC),
            btc: None,
        };
        let (plans, why, _) = h.trade_until_done(fix11 + 120, cb_only, 0);
        assert_eq!(why, FundError::DepegBlocksBuy);
        assert!(plans.iter().all(|p| p.side == Side::SellBtc));
        assert!(h.claim(&mut ta).unwrap().1 > 0, "赎回照常完成");
        // 参照恢复之后买入
        h.trade_at(fix11 + 3_600, same(52_000 * USDC), 0).unwrap();
        assert!(h.claim(&mut tb).unwrap().0 > 0);
    }

    #[test]
    fn backfill_uses_the_chosen_feed() {
        let mut h = Harness::seeded(50_000 * USDC);
        h.c.signal_feed = SignalFeed::Btc;
        h.fix_day(9, 50_000 * USDC);
        let at = |price: u64| PricePoint {
            price,
            publish_time: 10 * DAY + ONE_AM,
        };
        let prices = FixPrices {
            cbbtc: Some(at(59_000 * USDC)),
            btc: Some(at(60_000 * USDC)),
        };
        assert_eq!(
            backfill_price(&mut h.s, 10, &prices, 11 * DAY, &h.c),
            Ok(60_000 * USDC)
        );
        let no_btc = FixPrices {
            cbbtc: Some(at(59_000 * USDC)),
            btc: None,
        };
        assert_eq!(
            backfill_price(&mut h.s.clone(), 10, &no_btc, 11 * DAY, &h.c),
            Err(FundError::BadPrice)
        );
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
    fn late_subscriber_gets_shares_by_what_their_usdc_buys() {
        // 老持有人 A 在 BTC 里；B 在价格大涨之后才申购，按实际买到的 cbBTC 发份额
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
        // B 拿到的份额应约为 A 的 1/1.2（价格涨了 20%，同样的 USDC 少买 1/6 的 cbBTC）
        let ratio = b_shares as f64 / a_shares as f64;
        assert!((ratio - 1.0 / 1.2).abs() < 1e-4, "ratio {ratio}");
        // A 的价值不被稀释：A 那部分基金资产 ≈ 1000 × 1.2
        let a_value = (a_shares as u128 * fund_value(&h.s, 61_200 * USDC)
            / h.s.share_supply as u128) as f64
            / USDC as f64;
        assert!((a_value - 1_200.0).abs() < 0.05, "a_value {a_value}");
    }

    #[test]
    fn subscription_buys_become_shares_by_the_cbbtc_they_add_and_are_claimed_in_parts() {
        let mut h = Harness::seeded(50_000 * USDC);
        h.c.trade_cap_usdc = 1_000 * USDC;
        let (mut ta, mut tb) = (Ticket::default(), Ticket::default());
        h.subscribe(&mut ta, 1_500 * USDC, 10 * DAY);
        h.subscribe(&mut tb, 500 * USDC, 10 * DAY);
        let r = h.fix_day(10, 51_000 * USDC);
        assert!(r.target_btc);
        assert_eq!(r.minted_shares, 0, "定价时还没有买入，不发份额");
        assert_eq!(h.claim(&mut ta), Err(FundError::NothingToClaim));

        // 第一笔：1000 USDC @ 51,000 = 1,960,784 sat。基金还没有份额：1 USDC 换 1 份
        let t0 = 10 * DAY + ONE_AM;
        let first = h.trade_at(t0, same(51_000 * USDC), 0).unwrap();
        assert_eq!(first.bucket, TradeBucket::Subscription { epoch: 10 });
        assert_eq!(h.rec(10).minted_shares, 1_000 * USDC);
        // 先领已成交的部分：A 占 3/4
        assert_eq!(h.claim(&mut ta).unwrap(), (750 * USDC, 0));
        assert_eq!(h.claim(&mut ta), Err(FundError::NothingToClaim));

        // 第二笔：1000 USDC @ 50,000 = 2,000,000 sat
        // 份额 = 2,000,000 × 1,000,000,000 ÷ 1,960,784 = 1,020,000,163
        h.trade_at(t0 + 60, same(50_000 * USDC), 0).unwrap();
        let r = h.rec(10);
        assert!(r.subscription_filled);
        assert_eq!(r.minted_shares, 2_020_000_163);
        assert_eq!((h.s.btc, h.s.share_supply), (3_960_784, 2_020_000_163));
        // A 再领剩下的部分，B 是最后一位，拿到精确的余数
        assert_eq!(h.claim(&mut ta).unwrap(), (765_000_122, 0));
        assert_eq!(h.claim(&mut tb).unwrap(), (505_000_041, 0));
        assert!(ta.is_empty() && tb.is_empty());
        assert_eq!(
            (
                h.rec(10).unclaimed_shares,
                h.rec(10).unclaimed_subscription_usdc
            ),
            (0, 0)
        );
    }

    #[test]
    fn netting_fills_subscriptions_with_the_slice_cbbtc_at_the_fixing_price() {
        let mut h = Harness::seeded(50_000 * USDC);
        let (mut ta, mut tb) = (Ticket::default(), Ticket::default());
        h.subscribe(&mut ta, 1_000 * USDC, 10 * DAY);
        h.settle_day(10, 51_000 * USDC, 0); // 1,960,784 sat，1000 份
        h.claim(&mut ta).unwrap();

        // A 赎回一半，B 申购 300：基金全是 cbBTC、目标仍是 cbBTC，B 直接拿 A 的 cbBTC
        h.redeem(&mut ta, 500 * USDC, 11 * DAY);
        h.subscribe(&mut tb, 300 * USDC, 11 * DAY);
        let r = h.fix_day(11, 52_000 * USDC);
        // 赎回资产 980,392 sat；300 USDC @ 52,000 = 576,923 sat 轧差给 B
        // B 的份额 = 576,923 × 500,000,000 ÷ 980,392（留下的 cbBTC）= 294,230,777
        assert_eq!(
            (r.slice_btc, r.netted_btc, r.netted_usdc),
            (980_392, 576_923, 300 * USDC)
        );
        assert!(r.subscription_filled);
        assert_eq!(r.minted_shares, 294_230_777);
        assert_eq!(h.s.btc, 980_392 + 576_923);
        // 赎回资产只剩 403,469 sat 要卖，已有 300 USDC
        assert_eq!(
            h.s.oldest_slice(),
            Some(&Slice {
                epoch: 11,
                btc: 403_469,
                usdc: 300 * USDC
            })
        );
        assert_eq!(h.claim(&mut tb).unwrap(), (294_230_777, 0));
        let (plans, why, _) = h.trade_until_done(11 * DAY + ONE_AM, same(52_000 * USDC), 0);
        assert_eq!(why, FundError::NothingToTrade);
        assert_eq!(plans.len(), 1);
        // 403,469 sat × 52,000 = 209.80388 USDC
        assert_eq!(h.claim(&mut ta).unwrap(), (0, 300 * USDC + 209_803_880));
    }

    #[test]
    fn subscriptions_fill_without_a_trade_when_the_target_is_usdc() {
        let mut h = Harness::seeded(50_000 * USDC);
        let (mut ta, mut tb) = (Ticket::default(), Ticket::default());
        h.subscribe(&mut ta, 1_000 * USDC, 10 * DAY);
        let r = h.fix_day(10, 50_000 * USDC); // 不高于均线：目标 USDC
        assert!(!r.target_btc);
        let t0 = 10 * DAY + ONE_AM;
        let plan = h.trade_at(t0, same(50_000 * USDC), 0).unwrap();
        assert_eq!(
            (plan.side, plan.amount_in, plan.bucket),
            (
                Side::WithoutTrade,
                1_000 * USDC,
                TradeBucket::Subscription { epoch: 10 }
            )
        );
        assert_eq!(h.claim(&mut ta).unwrap(), (1_000 * USDC, 0));
        assert_eq!((h.s.usdc, h.s.btc), (1_000 * USDC, 0));

        // 第二天：按基金的 USDC 计算份额，不交易、不占用交易间隔
        h.subscribe(&mut tb, 500 * USDC, 11 * DAY);
        h.fix_day(11, 50_000 * USDC);
        let last_trade = h.s.last_trade_time;
        h.trade_at(11 * DAY + ONE_AM, same(50_000 * USDC), 0)
            .unwrap();
        assert_eq!(h.s.last_trade_time, last_trade);
        assert_eq!(h.claim(&mut tb).unwrap(), (500 * USDC, 0));
        assert_eq!(
            h.trade_at(11 * DAY + ONE_AM, same(50_000 * USDC), 0),
            Err(FundError::NothingToTrade)
        );
    }

    #[test]
    fn subscriptions_wait_until_the_fund_finishes_its_rebalance() {
        let mut h = Harness::seeded(50_000 * USDC);
        h.c.trade_cap_usdc = 1_000 * USDC;
        let (mut ta, mut tb) = (Ticket::default(), Ticket::default());
        h.subscribe(&mut ta, 2_000 * USDC, 10 * DAY);
        h.settle_day(10, 50_000 * USDC, 0); // 目标 USDC，不交易
        h.claim(&mut ta).unwrap();

        // 第二天目标变成 cbBTC：A 赎回 500 份（只有 USDC，定价时就完成），B 申购 1000
        h.redeem(&mut ta, 500 * USDC, 11 * DAY);
        h.subscribe(&mut tb, 1_000 * USDC, 11 * DAY);
        let r = h.fix_day(11, 51_000 * USDC);
        assert!(r.target_btc && r.slice_sold_out);
        assert_eq!(r.netted_btc, 0, "基金还是 USDC，不轧差");
        let (plans, why, _) = h.trade_until_done(11 * DAY + ONE_AM, same(51_000 * USDC), 0);
        assert_eq!(why, FundError::NothingToTrade);
        // 先把基金的 1500 USDC 买完，再为 B 买入
        let buckets: Vec<_> = plans.iter().map(|p| (p.bucket, p.amount_in)).collect();
        assert_eq!(
            buckets,
            [
                (TradeBucket::Fund, 1_000 * USDC),
                (TradeBucket::Fund, 500 * USDC),
                (TradeBucket::Subscription { epoch: 11 }, 1_000 * USDC)
            ]
        );
        // 基金 2,941,176 sat、1500 份；B 买到 1,960,784 sat → 1000 份
        assert_eq!(h.claim(&mut tb).unwrap(), (1_000 * USDC, 0));
    }

    #[test]
    fn pending_subscriptions_fill_oldest_first_across_fixings() {
        let mut h = Harness::seeded(50_000 * USDC);
        let (mut ta, mut tb) = (Ticket::default(), Ticket::default());
        h.subscribe(&mut ta, 1_000 * USDC, 10 * DAY);
        h.fix_day(10, 51_000 * USDC);
        // 第 10 天所有成交都低于 min_out
        let (plans, why, _) = h.trade_until_done(10 * DAY + ONE_AM, same(51_000 * USDC), 100);
        assert_eq!((plans.len(), why), (0, FundError::SlippageExceeded));

        h.subscribe(&mut tb, 1_000 * USDC, 11 * DAY);
        h.fix_day(11, 52_000 * USDC);
        let (plans, why, _) = h.trade_until_done(11 * DAY + ONE_AM, same(52_000 * USDC), 0);
        assert_eq!(why, FundError::NothingToTrade);
        let buckets: Vec<_> = plans.iter().map(|p| p.bucket).collect();
        assert_eq!(
            buckets,
            [
                TradeBucket::Subscription { epoch: 10 },
                TradeBucket::Subscription { epoch: 11 }
            ]
        );
        assert!(h.claim(&mut ta).unwrap().0 > 0);
        assert!(h.claim(&mut tb).unwrap().0 > 0);
    }

    #[test]
    fn netting_waits_for_older_pending_subscriptions() {
        let mut h = Harness::seeded(50_000 * USDC);
        let (mut ta, mut tb, mut tc) = (Ticket::default(), Ticket::default(), Ticket::default());
        h.subscribe(&mut ta, 1_000 * USDC, 10 * DAY);
        h.settle_day(10, 51_000 * USDC, 0);
        h.claim(&mut ta).unwrap();
        // 第 11 天 B 的申购一笔都没成交
        h.subscribe(&mut tb, 500 * USDC, 11 * DAY);
        h.fix_day(11, 52_000 * USDC);
        let (plans, _, _) = h.trade_until_done(11 * DAY + ONE_AM, same(52_000 * USDC), 100);
        assert!(plans.is_empty());

        // 第 12 天 A 赎回、C 申购：B 还在排队，C 不能先拿 A 的 cbBTC
        h.redeem(&mut ta, 500 * USDC, 12 * DAY);
        h.subscribe(&mut tc, 300 * USDC, 12 * DAY);
        let r = h.fix_day(12, 53_000 * USDC);
        assert_eq!((r.netted_usdc, r.netted_btc, r.minted_shares), (0, 0, 0));
        let (plans, why, _) = h.trade_until_done(12 * DAY + ONE_AM, same(53_000 * USDC), 0);
        assert_eq!(why, FundError::NothingToTrade);
        let buckets: Vec<_> = plans.iter().map(|p| p.bucket).collect();
        assert_eq!(
            buckets,
            [
                TradeBucket::Slice { epoch: 12 },
                TradeBucket::Subscription { epoch: 11 },
                TradeBucket::Subscription { epoch: 12 }
            ]
        );
    }

    #[test]
    fn subscriptions_beyond_the_pending_limit_are_refunded_and_fixing_continues() {
        // 长时间脱锚：申购一直买不了。待成交申购满了之后，新一期的申购退款，
        // 定价照常进行，赎回不受影响。
        let mut h = Harness::seeded(50_000 * USDC);
        h.c.signal_feed = SignalFeed::CbBtc; // 两个价格源：cbBTC/USD 作信号，BTC/USD 检查脱锚
        h.c.subscription_fee_bps = 100;
        let mut holder = Ticket::default();
        h.subscribe(&mut holder, 10_000 * USDC, 9 * DAY);
        h.settle_day(9, 51_000 * USDC, 0);
        h.claim(&mut holder).unwrap();
        let depeg = |day: i64| at_fix2(day, 53_000 * USDC, 51_000 * USDC);
        let mut tickets = vec![];
        for day in 10..10 + MAX_PENDING as i64 {
            let mut t = Ticket::default();
            h.subscribe(&mut t, 100 * USDC, day * DAY);
            h.fix_with(day * DAY + ONE_AM, &depeg(day)).unwrap();
            tickets.push(t);
        }
        assert_eq!(h.s.pending_count as usize, MAX_PENDING);

        let day = 10 + MAX_PENDING as i64;
        let mut late = Ticket::default();
        h.subscribe(&mut late, 100 * USDC, day * DAY);
        h.redeem(&mut holder, 1_000 * USDC, day * DAY);
        let r = h.fix_with(day * DAY + ONE_AM, &depeg(day)).unwrap();
        assert!(r.redemption_shares > 0 && r.subscription_filled);
        // 全额退款，不收申购费
        assert_eq!(h.claim(&mut late).unwrap(), (0, 100 * USDC));
        assert!(late.is_empty());
        // 赎回照常：赎回资产在卖出后可以领取
        let m = mp(53_000 * USDC, 51_000 * USDC);
        let (plans, why, _) = h.trade_until_done(day * DAY + ONE_AM, m, 0);
        assert_eq!((plans.len(), why), (1, FundError::DepegBlocksBuy));
        assert!(h.claim(&mut holder).unwrap().1 > 0);
    }

    #[test]
    fn a_holder_who_stays_keeps_their_cbbtc_per_share_despite_other_flows() {
        // 价格不变、交易所收 0.25%：别人的申购赎回成本由他们自己承担
        let mut h = Harness::seeded(50_000 * USDC);
        let (mut stay, mut ta, mut tb) = (Ticket::default(), Ticket::default(), Ticket::default());
        h.subscribe(&mut stay, 5_000 * USDC, 10 * DAY);
        h.subscribe(&mut ta, 3_000 * USDC, 10 * DAY);
        h.settle_day(10, 51_000 * USDC, 25);
        let (stay_shares, _) = h.claim(&mut stay).unwrap();
        let (a_shares, _) = h.claim(&mut ta).unwrap();
        let per_share = |h: &Harness| (h.s.btc as f64) / (h.s.share_supply as f64);
        let before = per_share(&h);

        // A 全部赎回，B 申购 4000：轧差一部分，其余在市场上买卖
        h.redeem(&mut ta, a_shares, 11 * DAY);
        h.subscribe(&mut tb, 4_000 * USDC, 11 * DAY);
        h.settle_day(11, 51_000 * USDC, 25);
        h.claim(&mut ta).unwrap();
        h.claim(&mut tb).unwrap();
        assert!(per_share(&h) >= before, "{} < {before}", per_share(&h));
        assert!(stay_shares > 0 && h.s.usdc < 2);
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
            (
                Side::BuyBtc,
                1_000 * USDC,
                TradeBucket::Subscription { epoch: 10 }
            )
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
        h.c.signal_feed = SignalFeed::CbBtc; // 两个价格源：cbBTC/USD 作信号，BTC/USD 检查脱锚
        let mut t = Ticket::default();
        h.subscribe(&mut t, 1_000 * USDC, 10 * DAY);
        h.fix_day(10, 51_000 * USDC);
        // 晚上 9 点才交易，这时 cbBTC 是 52,500（BTC/USD 52,600，未脱锚）
        let now = 10 * DAY + 21 * 3600;
        let p = mp(52_600 * USDC, 52_500 * USDC);
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
        h.c.signal_feed = SignalFeed::CbBtc; // 两个价格源：cbBTC/USD 作信号，BTC/USD 检查脱锚
        let mut t = Ticket::default();
        h.subscribe(&mut t, 1_000 * USDC, 10 * DAY);
        // 信号要求持有 cbBTC，但 cbBTC 比 BTC 低 3%：定价照常，买入被拒绝
        let r = h
            .fix_with(
                10 * DAY + ONE_AM,
                &at_fix2(10, 53_000 * USDC, 51_400 * USDC),
            )
            .unwrap();
        assert!(r.target_btc, "目标仍是 cbBTC");
        let t0 = 10 * DAY + ONE_AM;
        let depegged = mp(53_000 * USDC, 51_400 * USDC);
        assert_eq!(h.trade_at(t0, depegged, 0), Err(FundError::DepegBlocksBuy));
        assert_eq!(
            h.claim(&mut t),
            Err(FundError::NothingToClaim),
            "申购还没成交"
        );

        // 每笔交易前都检查：当天恢复锚定后照常买入
        let plan = h.trade_at(t0 + 3_600, same(51_000 * USDC), 0).unwrap();
        assert_eq!(
            (plan.side, plan.bucket),
            (Side::BuyBtc, TradeBucket::Subscription { epoch: 10 })
        );
        assert_eq!(h.claim(&mut t).unwrap(), (1_000 * USDC, 0));

        // 脱锚期间卖出不受影响：持有人赎回一半，信号跌破均线
        h.redeem(&mut t, 500 * USDC, 11 * DAY);
        h.fix_with(
            11 * DAY + ONE_AM,
            &at_fix2(11, 40_000 * USDC, 38_000 * USDC),
        )
        .unwrap();
        let depegged = mp(40_000 * USDC, 38_000 * USDC);
        let (plans, why, _) = h.trade_until_done(11 * DAY + ONE_AM, depegged, 0);
        assert_eq!(why, FundError::NothingToTrade);
        let buckets: Vec<_> = plans.iter().map(|p| (p.side, p.bucket)).collect();
        assert_eq!(
            buckets,
            [
                (Side::SellBtc, TradeBucket::Slice { epoch: 11 }),
                (Side::SellBtc, TradeBucket::Fund)
            ]
        );
        assert_eq!(h.s.btc, 0);
        assert!(h.claim(&mut t).unwrap().1 > 0);
    }

    #[test]
    fn fund_trades_wait_for_the_fixing_step_but_slices_keep_selling() {
        // 定价时刻之后、当天定价之前，基金调仓和申购暂停：目标可能在定价时改变。
        // 赎回资产不算基金资产，照常卖出。
        let mut h = Harness::seeded(50_000 * USDC);
        h.c.trade_cap_usdc = 1_000 * USDC;
        let (mut ta, mut tb) = (Ticket::default(), Ticket::default());
        h.subscribe(&mut ta, 1_000 * USDC, 10 * DAY);
        h.subscribe(&mut tb, 3_000 * USDC, 10 * DAY);
        h.settle_day(10, 51_000 * USDC, 0);
        h.claim(&mut ta).unwrap();
        h.claim(&mut tb).unwrap();

        // 第 11 天：A 全部赎回，目标变成 USDC；卖完 A 的赎回资产后，基金只卖了一笔
        h.redeem(&mut ta, 1_000 * USDC, 11 * DAY);
        h.fix_day(11, 40_000 * USDC);
        let p = same(40_000 * USDC);
        let t11 = 11 * DAY + ONE_AM;
        assert_eq!(
            h.trade_at(t11, p, 0).unwrap().bucket,
            TradeBucket::Slice { epoch: 11 }
        );
        assert_eq!(
            h.trade_at(t11 + 60, p, 0).unwrap().bucket,
            TradeBucket::Fund
        );
        h.redeem(&mut tb, 1_000 * USDC, t11 + 3_600);

        // 第 12 天 0 点到 1 点之间，前一天的调仓照常继续
        assert_eq!(
            h.trade_at(12 * DAY, p, 0).unwrap().bucket,
            TradeBucket::Fund
        );
        // 1 点之后、定价之前：基金调仓等待定价
        let fix_at = 12 * DAY + ONE_AM;
        assert_eq!(h.trade_at(fix_at, p, 0), Err(FundError::FixPending));
        assert_eq!(
            h.trade_at(fix_at + 5 * 3600, p, 0),
            Err(FundError::FixPending)
        );
        // 定价之后：B 的赎回资产先卖
        h.fix_with(fix_at + 5 * 3600, &at_fix(12, 40_000 * USDC))
            .unwrap();
        // 第 13 天 1 点之后、定价之前：B 没卖完的赎回资产照常卖出
        let plan = h.trade_at(13 * DAY + ONE_AM + 60, p, 0).unwrap();
        assert_eq!(plan.bucket, TradeBucket::Slice { epoch: 12 });
        assert_eq!(
            h.trade_at(13 * DAY + ONE_AM + 120, p, 0),
            Err(FundError::FixPending)
        );
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
        assert_eq!(first.bucket, TradeBucket::Slice { epoch: 11 });
        assert_eq!(first.amount_in, 1_960_784);
        assert_eq!(h.claim(&mut ta), Err(FundError::SliceNotSold));
        h.trade_at(t0 + 60, same(50_000 * USDC), 0).unwrap();
        // 1,960,784 sat × 51,000 + 1,960,784 sat × 50,000
        let proceeds = 999_999_840 + 980_392_000;
        let r = h.rec(11);
        assert!(r.slice_sold_out);
        assert_eq!(r.payout_usdc, proceeds);

        let (_, a) = h.claim(&mut ta).unwrap();
        let (_, b) = h.claim(&mut tb).unwrap();
        assert_eq!((a, b), (proceeds / 2, proceeds / 2));
        assert_eq!(h.rec(11).unclaimed_payout_usdc, 0);
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
        // A 那份没卖完的 cbBTC 不算基金资产：B 的赎回资产是基金 3,921,568 sat 的一半
        assert_eq!(r2.slice_btc, 1_960_784);

        // 先卖第 11 天的，再卖第 12 天的，最后才是基金调仓
        let (plans, why, _) = h.trade_until_done(12 * DAY + ONE_AM, same(40_000 * USDC), 0);
        assert_eq!(why, FundError::NothingToTrade);
        let buckets: Vec<_> = plans.iter().map(|p| p.bucket).collect();
        assert_eq!(
            buckets,
            [
                TradeBucket::Slice { epoch: 11 },
                TradeBucket::Slice { epoch: 12 },
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
        // 当天只成交了一笔：基金有 1,960,784 sat，还有 2000 USDC 待成交
        h.trade_at(10 * DAY + ONE_AM, same(51_000 * USDC), 0)
            .unwrap();
        assert_eq!((h.s.usdc, h.s.btc), (0, 1_960_784));
        assert_eq!(h.s.oldest_pending().map(|p| p.usdc), Some(2_000 * USDC));

        // 第二天目标变成 USDC：不再为申购买入，先卖掉已经买的，剩下的申购款不交易直接成交
        h.fix_day(11, 40_000 * USDC);
        let (plans, why, _) = h.trade_until_done(11 * DAY + ONE_AM, same(40_000 * USDC), 0);
        assert_eq!(why, FundError::NothingToTrade);
        let steps: Vec<_> = plans
            .iter()
            .map(|p| (p.side, p.amount_in, p.bucket))
            .collect();
        assert_eq!(
            steps,
            [
                (Side::SellBtc, 1_960_784, TradeBucket::Fund),
                (
                    Side::WithoutTrade,
                    2_000 * USDC,
                    TradeBucket::Subscription { epoch: 10 }
                )
            ]
        );
        assert_eq!(h.s.btc, 0);
        assert!(h.claim(&mut t).unwrap().0 > 0);
        assert!(t.is_empty());
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
            .all(|p| p.bucket == TradeBucket::Slice { epoch: 11 }
                && btc_value(p.amount_in, 52_000 * USDC).unwrap() <= 200_000 * USDC));
        let (_, ua) = h.claim(&mut ta).unwrap();
        let (_, ub) = h.claim(&mut tb).unwrap();
        assert_eq!(ua + ub, h.rec(11).payout_usdc);
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
        assert_eq!(a_usdc, h.rec(11).payout_usdc);

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
            btc: Some(PricePoint {
                price: 52_000 * USDC,
                publish_time: fix_at + 30,
            }),
            cbbtc: Some(PricePoint {
                price: 52_000 * USDC,
                publish_time: fix_at + 30,
            }),
        };
        // 晚上 9 点才定价：信号、轧差、赎回资产都只取决于定价价格
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
        assert!(b.record.slice_btc > 0 && b.record.minted_shares > 0);
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
        subscription_buys: u32,
        without_trade: u32,
        slice_sells: u32,
        fund_sells: u32,
        failed_fills: u32,
        too_soon: u32,
        depeg_blocked: u32,
        depeg_sells: u32,
        netted: usize,
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
            let m = mp(
                rng.wiggle(p.btc.unwrap(), 30),
                rng.wiggle(p.cbbtc.unwrap(), 30),
            );
            // 按配置选出的价格：成交价，以及有参照时的脱锚检查
            let chosen = choose_feed(m.cbbtc, m.btc, &h.c).unwrap();
            let depegged = chosen.blocks_buys(&h.c);
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
            assert!(btc_value_of(&plan, chosen.price) <= h.c.trade_cap_usdc);
            // 随机失败，或者交易所的手续费超过这个调用人的滑点上限
            let got = if plan.min_out > 0 && rng.below(100) < fail_pct {
                plan.min_out - 1
            } else {
                exec(&plan, chosen.price, fee_bps)
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
                (Side::BuyBtc, bucket) => {
                    assert!(!depegged, "脱锚时不能买入");
                    match bucket {
                        TradeBucket::Fund => tally.buys += 1,
                        _ => tally.subscription_buys += 1,
                    }
                }
                (Side::SellBtc, TradeBucket::Slice { .. }) => tally.slice_sells += 1,
                (Side::SellBtc, TradeBucket::Fund) => tally.fund_sells += 1,
                (Side::SellBtc, TradeBucket::Subscription { .. }) => panic!("申购不会卖出"),
                (Side::WithoutTrade, _) => tally.without_trade += 1,
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
            Side::WithoutTrade => 0,
        }
    }

    /// 模拟里的用户：还没领完的排队记录（每期一张）、持有的份额，以及每次定价已领取的总额
    struct Users {
        tickets: Vec<Vec<Ticket>>,
        held: Vec<u64>,
        claimed: Vec<(u64, u64)>, // 每次定价（h.records 的下标）领取的 (份额, USDC)
        cancels: u32,
        cancels_rejected: u32,
        paused_rejections: u32,
        pending_cancels: u32,
        cap_rejections: u32,
    }

    /// 排队记录还没被定价处理
    fn unfixed(t: &Ticket, h: &Harness) -> bool {
        !t.is_empty() && t.epoch as i64 > h.s.last_fix_day
    }

    impl Users {
        fn new(n: usize) -> Self {
            Users {
                tickets: vec![vec![]; n],
                held: vec![0; n],
                claimed: vec![],
                cancels: 0,
                cancels_rejected: 0,
                paused_rejections: 0,
                pending_cancels: 0,
                cap_rejections: 0,
            }
        }

        /// 用户 u 在 now 开放的那一期的排队记录（没有就新开一张）
        fn ticket_at(&mut self, h: &Harness, u: usize, now: i64) -> usize {
            let epoch = request_epoch(now, &h.c).unwrap();
            let list = &mut self.tickets[u];
            match list.iter().position(|t| t.epoch == epoch && !t.is_empty()) {
                Some(i) => i,
                None => {
                    list.push(Ticket::default());
                    list.len() - 1
                }
            }
        }

        /// 申购，返回是否接受；暂停期间或超过申购上限时必须被拒绝、什么都不改
        fn subscribe(&mut self, h: &mut Harness, u: usize, amount: u64, now: i64) -> bool {
            if h.s.subscriptions_paused {
                let before = h.s.clone();
                let mut t = Ticket::default();
                let got = request_subscription(&mut h.s, &mut t, amount, now, &h.c);
                assert_eq!(got, Err(FundError::SubscriptionsPaused));
                assert_eq!(h.s, before);
                self.paused_rejections += 1;
                return false;
            }
            // 超过申购上限（算上待成交的）必须被拒绝、什么都不改
            let outstanding = h.s.outstanding_subscription_usdc().unwrap();
            if outstanding + amount > h.c.subscription_cap_usdc {
                let before = h.s.clone();
                let got = request_subscription(&mut h.s, &mut Ticket::default(), amount, now, &h.c);
                assert_eq!(got, Err(FundError::SubscriptionCapExceeded));
                assert_eq!(h.s, before);
                self.cap_rejections += 1;
                return false;
            }
            let i = self.ticket_at(h, u, now);
            h.subscribe(&mut self.tickets[u][i], amount, now);
            true
        }

        fn redeem(&mut self, h: &mut Harness, u: usize, shares: u64, now: i64) {
            let i = self.ticket_at(h, u, now);
            self.held[u] -= shares;
            h.redeem(&mut self.tickets[u][i], shares, now);
        }

        /// 随机撤销还没定价的排队记录：关闭它的定价时刻之前必须成功、全额退回，之后必须被拒绝
        fn random_cancels(&mut self, h: &mut Harness, rng: &mut Rng, now: i64) {
            for u in 0..self.tickets.len() {
                for i in 0..self.tickets[u].len() {
                    let t = self.tickets[u][i];
                    if !unfixed(&t, h) || rng.below(8) != 0 {
                        continue;
                    }
                    let closes_at = cancel_deadline(t.epoch, now, &h.c).unwrap();
                    match h.cancel(&mut self.tickets[u][i], now) {
                        Ok(out) => {
                            assert!(now < closes_at);
                            assert_eq!(
                                (out.usdc, out.shares),
                                (t.subscription_usdc, t.redemption_shares)
                            );
                            self.held[u] += out.shares;
                            self.cancels += 1;
                        }
                        Err(FundError::TooLateToCancel) => {
                            assert!(now >= closes_at);
                            self.cancels_rejected += 1;
                        }
                        Err(e) => panic!("cancel: {e:?}"),
                    }
                }
                self.tickets[u].retain(|t| !t.is_empty());
            }
        }

        /// 随机撤销已定价、还没成交完的申购：已成交的份额回到用户手里，算进这一期领取的份额
        fn random_pending_cancels(&mut self, h: &mut Harness, rng: &mut Rng) {
            self.claimed.resize(h.records.len(), (0, 0));
            for u in 0..self.tickets.len() {
                for i in 0..self.tickets[u].len() {
                    let t = self.tickets[u][i];
                    if t.subscription_usdc == 0 || unfixed(&t, h) || rng.below(10) != 0 {
                        continue;
                    }
                    let k = h.records.iter().position(|r| r.covers(t.epoch)).unwrap();
                    if h.records[k].subscription_filled {
                        continue;
                    }
                    let out = h.cancel_pending(&mut self.tickets[u][i]).unwrap();
                    self.held[u] += out.shares;
                    self.claimed[k].0 += out.shares;
                    self.pending_cancels += 1;
                }
                self.tickets[u].retain(|t| !t.is_empty());
            }
        }

        /// 领取所有已定价、可以领取的记录
        fn claim_ready(&mut self, h: &mut Harness) {
            self.claimed.resize(h.records.len(), (0, 0));
            for u in 0..self.tickets.len() {
                for i in 0..self.tickets[u].len() {
                    let t = self.tickets[u][i];
                    if t.is_empty() || unfixed(&t, h) {
                        continue;
                    }
                    let k = h.records.iter().position(|r| r.covers(t.epoch)).unwrap();
                    match h.claim(&mut self.tickets[u][i]) {
                        Ok((sh, us)) => {
                            self.held[u] += sh;
                            self.claimed[k].0 += sh;
                            self.claimed[k].1 += us;
                        }
                        Err(FundError::SliceNotSold) => assert!(t.redemption_shares > 0),
                        // 申购还在成交中，没有新的份额
                        Err(FundError::NothingToClaim) => assert!(t.subscription_usdc > 0),
                        Err(e) => panic!("claim: {e:?}"),
                    }
                }
                self.tickets[u].retain(|t| !t.is_empty());
            }
        }

        /// 已发行份额 = 用户持有 + 还没定价的排队赎回份额（可能有好几期）
        /// + 已发行、在托管账户里等申购人领取的份额。
        /// 申购可以分几次领取，取整剩下的零头要等全部成交后最后一次领取。
        fn assert_share_supply(&self, h: &Harness) {
            let queued: u64 = self
                .tickets
                .iter()
                .flatten()
                .filter(|t| unfixed(t, h))
                .map(|t| t.redemption_shares)
                .sum();
            let unclaimed: u64 = h.records.iter().map(|r| r.unclaimed_shares).sum();
            assert_eq!(
                h.s.share_supply,
                self.held.iter().sum::<u64>() + queued + unclaimed
            );
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
            // 欠管理人的取款费结清之后，基金名下的 USDC 只剩基金资产
            h.collect_fees();
            assert_eq!(h.s.fees_owed, 0);
            assert_eq!(h.s.slice_count, 0, "赎回资产都应卖完");
            assert_eq!(h.s.pending_count, 0, "申购都应成交");
            assert!(
                self.tickets.iter().all(|l| l.is_empty()),
                "每张记录都应领取"
            );
            assert_eq!(self.claimed.len(), h.records.len());
            for (r, &(sh, us)) in h.records.iter().zip(&self.claimed) {
                assert!(r.slice_sold_out && r.subscription_filled);
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
        let mut pending_cancels = 0;
        for seed in 1..=20u64 {
            let mut rng = Rng(seed);
            let mut h = Harness::seeded(50_000 * USDC);
            h.c.trade_cap_usdc = 2_000 * USDC; // 用户的金额相对上限较大，每天多笔交易
                                               // 一半的种子用 cbBTC/USD 作信号（BTC/USD 检查脱锚），另一半用默认的 BTC/USD
            if seed % 2 == 1 {
                h.c.signal_feed = SignalFeed::CbBtc;
            }
            let mut users = Users::new(8);
            let mut tally = Tally::default();
            let mut price = 50_000 * USDC;
            let mut valuation = price;
            for day in 10..400i64 {
                // 0 点和当晚（定价之后，属于下一期）随机申购赎回，再随机撤销
                let random_requests = |users: &mut Users, h: &mut Harness, rng: &mut Rng, now| {
                    for u in 0..users.tickets.len() {
                        match rng.below(14) {
                            0 | 1 => {
                                let amt = (1 + rng.below(5_000)) * USDC;
                                users.subscribe(h, u, amt, now);
                            }
                            2 if users.held[u] > 0 => {
                                let sh = 1 + rng.below(users.held[u]);
                                users.redeem(h, u, sh, now);
                            }
                            _ => {}
                        }
                    }
                    users.random_cancels(h, rng, now);
                    users.assert_share_supply(h);
                };
                random_requests(&mut users, &mut h, &mut rng, day * DAY);
                // 价格随机游走（±8%）；cbBTC 相对 BTC 偏离 -150..=150 bps，不触发脱锚
                price = rng.wiggle(price, 800).max(1_000 * USDC);
                valuation = rng.wiggle(price, 150);
                let fix_at = day * DAY + ONE_AM;
                match h.fix_with(fix_at, &at_fix2(day, price, valuation)) {
                    Ok(_) | Err(FundError::TooManySlices) => {}
                    Err(e) => panic!("fix: {e:?}"),
                }
                // 定价之后、成交之前，有人撤销刚成为待成交的申购
                users.random_pending_cancels(&mut h, &mut rng);
                let p = mp(price, valuation);
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
                users.random_pending_cancels(&mut h, &mut rng);
                users.assert_share_supply(&h);
                random_requests(&mut users, &mut h, &mut rng, fix_at + DAY - 2 * ONE_AM);
            }
            users.drain(&mut h, 400, valuation);
            tally.netted += h.records.iter().filter(|r| r.netted_btc > 0).count();
            assert!(
                tally.buys > 0
                    && tally.slice_sells > 0
                    && tally.fund_sells > 0
                    && tally.subscription_buys > 0
                    && tally.without_trade > 0
                    && tally.netted > 0,
                "seed {seed}: {tally:?}"
            );
            assert!(tally.failed_fills > 0 && tally.too_soon > 0, "{tally:?}");
            assert!(tally.keeper_only > 0 && tally.public_plans > 0, "{tally:?}");
            assert!(users.cancels > 0, "seed {seed}: 没有撤销");
            assert!(h.fees > 0, "seed {seed}: 没有收到取款费");
            pending_cancels += users.pending_cancels;
            // 价值守恒：基金总资产 ≈ 用户份额价值之和（误差为取整尘埃），按估值价格计
            let assets = fund_value(&h.s, valuation);
            let supply = h.s.share_supply as u128;
            let user_value: u128 = users
                .held
                .iter()
                .map(|&x| x as u128 * assets / supply)
                .sum();
            assert!(assets >= user_value);
            assert!(
                assets - user_value < 1_000 * USDC as u128 / 1000,
                "seed {seed}: 尘埃过大 {}",
                assets - user_value
            );
        }
        assert!(pending_cancels > 0, "没有撤销待成交申购");
    }

    /// 随机模拟：交易所收 0.25%（接近 0.3% 滑点上限），随机晚定价、随机脱锚、
    /// 随机交易时刻和失败成交、整天没有流动性、极小额赎回。基金永远不会卡住：
    /// 每次定价都成功（或因赎回资产太多暂时顺延），最后所有赎回资产都能卖完、所有记录都能领取。
    #[test]
    fn random_simulation_with_trading_costs_never_gets_stuck() {
        let mut tally = Tally::default();
        let (mut late, mut dust, mut carried, mut illiquid_days) = (0, 0, 0, 0);
        let (mut public_fixes, mut public_fixes_rejected) = (0, 0);
        let (mut late_cancels_rejected, mut cancels, mut paused_rejections) = (0, 0, 0);
        let (mut skipped_fixes, mut backfills) = (0, 0);
        let (mut pending_cancels, mut cap_rejections) = (0, 0);
        for seed in 1..=20u64 {
            let mut rng = Rng(seed * 7919);
            let mut h = Harness::seeded(50_000 * USDC);
            h.c.trade_cap_usdc = 1_000 * USDC;
            // 两个价格源，才有脱锚
            h.c.signal_feed = SignalFeed::CbBtc;
            // 申购上限设得小一些（用户每笔最多 3,000），有时会被拒绝
            h.c.subscription_cap_usdc = 6_000 * USDC;
            let mut users = Users::new(6);
            let mut price = 50_000 * USDC;
            let mut valuation = price;
            let mut fail_pct = 30;
            let mut skipped: Option<(i64, FixPrices)> = None;
            for day in 10..300i64 {
                // 昨天没人定价：一半的时候有人用昨天定价窗口内的价格回填，只能回填一次
                if let Some((d, prices)) = skipped.take() {
                    if rng.below(2) == 0 {
                        backfill_price(&mut h.s, d, &prices, day * DAY, &h.c).unwrap();
                        assert_eq!(
                            backfill_price(&mut h.s, d, &prices, day * DAY, &h.c),
                            Err(FundError::PriceAlreadyRecorded)
                        );
                        backfills += 1;
                    }
                }
                // 有时有人结清欠管理人的取款费
                if rng.below(10) == 0 {
                    h.collect_fees();
                }
                // 管理员随机暂停、恢复申购：赎回、定价、交易和领取照常
                if rng.below(15) == 0 {
                    h.s.subscriptions_paused = !h.s.subscriptions_paused;
                }
                for u in 0..users.tickets.len() {
                    let r = rng.below(14);
                    if r < 2 {
                        let amt = (1 + rng.below(3_000)) * USDC;
                        users.subscribe(&mut h, u, amt, day * DAY);
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
                        users.redeem(&mut h, u, sh, day * DAY);
                    }
                }
                users.random_cancels(&mut h, &mut rng, day * DAY);
                // 昨天的价格附近：定价之前，前几天没做完的交易继续
                let yesterday = mp(price, valuation);
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
                    // 定价时刻之后、定价之前：已经排队的不能撤销，新请求进入下一期
                    late += 1;
                    let t = fix_at + rng.below((now - fix_at) as u64) as i64;
                    let u = rng.below(users.tickets.len() as u64) as usize;
                    for i in 0..users.tickets[u].len() {
                        let tk = users.tickets[u][i];
                        if unfixed(&tk, &h) && tk.epoch as i64 <= day {
                            assert_eq!(
                                h.cancel(&mut users.tickets[u][i], t),
                                Err(FundError::TooLateToCancel)
                            );
                            late_cancels_rejected += 1;
                        }
                    }
                    if users.subscribe(&mut h, u, (1 + rng.below(1_000)) * USDC, t) {
                        assert!(users.tickets[u].iter().any(|tk| tk.epoch == day as u64 + 1));
                    }
                    users.random_cancels(&mut h, &mut rng, t);
                    users.assert_share_supply(&h);
                }
                if h.s.slice_count > 0 {
                    carried += 1;
                }
                let publish =
                    |rng: &mut Rng| (fix_at + rng.below(FIX_WINDOW_SECS as u64) as i64).min(now);
                let p = FixPrices {
                    btc: Some(PricePoint {
                        price,
                        publish_time: publish(&mut rng),
                    }),
                    cbbtc: Some(PricePoint {
                        price: valuation,
                        publish_time: publish(&mut rng),
                    }),
                };
                // 有时整天没人定价：这一期顺延，第二天可能有人回填这天的信号价格
                if rng.below(25) == 0 {
                    skipped = Some((day, p));
                    skipped_fixes += 1;
                } else {
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
                }
                let m = mp(price, valuation);
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
                users.random_pending_cancels(&mut h, &mut rng);
                users.assert_share_supply(&h);
            }
            h.s.subscriptions_paused = false;
            users.drain(&mut h, 300, price);
            cancels += users.cancels;
            pending_cancels += users.pending_cancels;
            cap_rejections += users.cap_rejections;
            paused_rejections += users.paused_rejections;
            tally.netted += h.records.iter().filter(|r| r.netted_btc > 0).count();
        }
        // 各种交易路径、晚定价、脱锚的两种情况、顺延和极小额赎回都应被覆盖到
        assert!(
            tally.buys > 0
                && tally.slice_sells > 0
                && tally.fund_sells > 0
                && tally.subscription_buys > 0
                && tally.without_trade > 0
                && tally.netted > 0,
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
        assert!(
            cancels > 0 && late_cancels_rejected > 0 && paused_rejections > 0,
            "{cancels} {late_cancels_rejected} {paused_rejections}"
        );
        assert!(
            skipped_fixes > 0 && backfills > 0,
            "{skipped_fixes} {backfills}"
        );
        assert!(
            pending_cancels > 0 && cap_rejections > 0,
            "{pending_cancels} {cap_rejections}"
        );
    }

    /// 不变量 8：记账只用整数。fund-logic 和程序里测试以外的代码都不能出现浮点数
    #[test]
    fn money_code_uses_integers_only() {
        let sources = [
            ("fund-logic", include_str!("lib.rs")),
            ("program", include_str!("../../programs/tide/src/lib.rs")),
        ];
        for (name, src) in sources {
            let code = src.split("#[cfg(test)]").next().unwrap();
            for float in ["f32", "f64"] {
                assert!(!code.contains(float), "{name} 用了 {float}");
            }
        }
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
