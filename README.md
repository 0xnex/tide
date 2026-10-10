# tide

**A pooled Bitcoin fund on Solana: hold cbBTC when BTC is above its 50-day moving average, hold USDC when it is below.**

> **Devnet prototype. Do not use with real money.** The oracle and exchange are mocks. Mainnet needs a security audit and legal advice: pooling other people's money and charging a performance fee is regulated in many jurisdictions.

> **Design in transition.** This README shows the target design ([`CONTEXT.md`](CONTEXT.md) for terms, [`docs/adr/`](docs/adr/) for decisions). The code runs settlement as a fixing step plus capped trades, with redeemers paid their slice's actual proceeds. Subscriptions are still filled at the fixing NAV, and the keeper window, performance fee and governance are not built yet. Sections marked *(current code)* describe what runs today.

---

## Why this rule

Backtest on Binance data, BTC, Mar 2018 – Sep 2026:

| Approach | Total return | Max drawdown |
|---|---|---|
| Buy and hold | +872% | −77% |
| 50-day MA filter | +2,719% | −58% |
| 100-day MA filter | +2,076% | −39% |
| 200-day MA filter | +1,083% | −64% |

- The filters cut drawdowns mainly by holding cash through the 2018 and 2022 bear markets.
- The best window changed between periods, so treat 50 days as a default, not an optimum.
- Short-term altcoin strategies lost money in the same study, so the fund holds only BTC.
- Past results do not predict future returns.

---

## How it works

### One day

```mermaid
flowchart LR
  A["Any time<br/>subscribe · redeem<br/>cancel before 01:00"] --> B["01:00 UTC<br/>fixing step"]
  B --> C["01:00–01:30<br/>keeper trades"]
  C --> D["After 01:30<br/>anyone trades<br/>tighter slippage"]
  D --> E["Claim<br/>shares or USDC"]
  D -. "unfinished at next 01:00" .-> B
```

### Fixing step (01:00 UTC)

```mermaid
flowchart TD
  BTC["Pyth BTC/USD<br/>signal price"] --> MA["50-day moving average"]
  BTC --> SIG{"price > MA?"}
  MA --> SIG
  SIG -- yes --> T1["target: all cbBTC"]
  SIG -- no --> T2["target: all USDC"]
  CB["Pyth cbBTC/USD<br/>valuation price"] --> NAV["NAV"]
  NAV --> FEE["performance fee<br/>(gains above high-water mark)"]
  FEE --> SL["set aside redemption slice"]
  SL --> NET["net against subscriptions"]
```

### Where the money goes

```mermaid
flowchart LR
  SUB["Subscribers' USDC"] -- "netted part" --> RED["Redeemers"]
  SLICE["Redemption slice<br/>(cbBTC + USDC)"] -- "netted part" --> FUND["Fund<br/>→ shares for subscribers"]
  SUB -- "remainder: buy on Jupiter" --> FUND
  SLICE -- "remainder: sell on Jupiter" --> RED
```

Netting uses the valuation price. Only the difference trades on Jupiter, and each side pays for its own trades ([ADR 0002](docs/adr/0002-flows-pay-their-own-execution.md)).

### Trade order

```mermaid
flowchart LR
  A["1. Unsold redemption slices<br/>oldest first"] --> B["2. Pending subscriptions<br/>oldest first"] --> C["3. Fund toward target"]
```

### Every trade

```mermaid
flowchart TD
  S["trade requested"] --> W{"01:00–01:30?"}
  W -- yes --> K{"caller is keeper?"}
  K -- no --> R["reject"]
  K -- yes --> I
  W -- no --> I{"≥ 60 s since last trade?"}
  I -- no --> R
  I -- yes --> D{"buying cbBTC while<br/>cbBTC/BTC gap > 2%?"}
  D -- yes --> R
  D -- no --> Z["size = min(remaining, 200K USDC)"]
  Z --> J["Jupiter swap"]
  J --> M{"out ≥ min_out?<br/>valuation price now − bound<br/>keeper 0.3% · others 0.1%"}
  M -- no --> V["revert this trade"]
  M -- yes --> OK["record fill"]
```

See [ADR 0001](docs/adr/0001-multi-step-settlement.md).

### Subscription

```mermaid
flowchart LR
  A["subscribe"] --> Q["Queued"]
  Q -- "cancel" --> X["USDC refunded"]
  Q -- "01:00" --> P["Pending<br/>fills part by part"]
  P -- "filled" --> D["Done"]
  P -- "cancel rest" --> D
  D -- "claim" --> SH["Shares"]
```

| Step | Rule |
|---|---|
| subscribe | At most 1M USDC per epoch, including pending |
| cancel | Until 01:00 |
| partial fill | Filled part becomes shares at that day's NAV |
| filled | Fully bought, or target is USDC (no trade needed) |
| cancel rest | Any time; unfilled USDC refunded, no shares for it |

### Redemption

```mermaid
flowchart LR
  A["redeem"] --> Q["Queued"]
  Q -- "cancel" --> X["Shares returned"]
  Q -- "01:00" --> S["Selling<br/>part by part"]
  S -- "sold out" --> C["Claimable"]
  C -- "claim" --> U["USDC"]
```

| Step | Rule |
|---|---|
| redeem | No cap, can never be paused |
| cancel | Until 01:00 |
| 01:00 | Pro-rata redemption slice set aside |
| sell part | Netting first, then Jupiter, oldest slices first |
| claim | Actual USDC proceeds, only after the whole slice is sold |

### Prices

```mermaid
flowchart LR
  BTC["Pyth BTC/USD"] --> MA["moving average"] & SIG["signal"]
  CB["Pyth cbBTC/USD"] --> NAV["NAV"] & MIN["trade min_out"]
  BTC & CB --> G{"gap > 2%?"}
  G -- "yes: depeg" --> NB["no cbBTC buys<br/>sells and settlement continue"]
```

Both use prices published in `[01:00, 01:01)` UTC. A missed day is backfilled with that day's Pyth price ([ADR 0003](docs/adr/0003-btc-signal-cbbtc-valuation.md)).

### Performance fee

```mermaid
flowchart LR
  N["NAV at 01:00"] --> Q{"NAV > high-water mark?"}
  Q -- no --> X["no fee"]
  Q -- yes --> F["5% of gain above mark<br/>→ new shares to fee address"]
  F --> H["mark := NAV after fee<br/>(never reset)"]
```

The fee is charged before that epoch's subscriptions and redemptions and is capped at 20% in code ([ADR 0004](docs/adr/0004-performance-fee-high-water-mark.md)).

### Governance

```mermaid
flowchart LR
  MS["Admin multisig"] -- "48 h timelock" --> CFG["config: caps, interval, slippage,<br/>depeg threshold, fee rate,<br/>keeper, fee address"]
  MS -- "immediate" --> P["pause subscriptions"]
  MS -. "never" .-> N["move fund assets<br/>pause redemptions"]
```

### Other guarantees

| Guarantee | How |
|---|---|
| Donations can't move the share price | NAV comes from internal records, not token balances |
| No rounding dust | The last claimant of an epoch gets the exact remainder |
| Settlement never gets stuck | Unfinished work carries into the next settlement; a depeg only blocks buys |

---

## Repository layout

```
tide/
├── fund-logic/          Pure-Rust accounting + unit tests
├── programs/tide/       Anchor program: accounts and token transfers
├── tests/tide.ts        End-to-end test (bankrun, time travel)
├── scripts/crank.ts     Daily settlement script (devnet)
├── docs/adr/            Architecture decisions
├── docs/agents/         Agent skill configuration
├── CONTEXT.md           Domain vocabulary
└── AGENTS.md            Rules for agents and contributors
```

```mermaid
flowchart LR
  P["programs/tide<br/>accounts · CPIs"] -- "calls" --> L["fund-logic<br/>all money math"]
  T["tests/tide.ts"] --> P
  C["scripts/crank.ts"] --> P
```

---

## Getting started *(current code)*

Needs Rust (stable), Solana CLI 2.x, Anchor 0.31.1 (via `avm`), Node.js 20+ and Yarn.

```bash
cd fund-logic && cargo test && cd ..   # logic tests, no Solana toolchain
anchor keys sync                       # write program ID
anchor build
yarn install && anchor test            # end-to-end
```

> The Anchor program has never been compiled in the original author's environment. If `anchor build` fails, the cause is most likely a small API mismatch in the wrapper, not in the accounting.

### Deploy to devnet

```bash
solana config set --url devnet && solana airdrop 2
anchor deploy --provider.cluster devnet
```

```mermaid
flowchart LR
  A["mock USDC (6)<br/>mock BTC (8) mints"] --> B["init_mock_oracle"] --> C["init_mock_market<br/>+ liquidity"] --> D["initialize"] --> E["init_vaults"] --> F["seed_prices<br/>last 49 days"]
```

Run the crank daily at 01:00 UTC. The mock price must be posted within 60 s:

```cron
0 1 * * * cd /path/to/tide && ANCHOR_PROVIDER_URL=https://api.devnet.solana.com ANCHOR_WALLET=$HOME/.config/solana/id.json yarn ts-node scripts/crank.ts >> crank.log 2>&1
```

```mermaid
flowchart LR
  BN["Binance API<br/>BTC price"] --> CR["crank.ts"] -- "set_mock_price" --> O["mock oracle"]
  CR -- "fix, then trade<br/>every 60 s until done" --> P["tide program"]
```

---

## Program reference *(current code)*

| Instruction | Who | Purpose |
|---|---|---|
| `initialize(config)` | admin | Create fund account and share mint |
| `init_vaults` | admin | Create the five fund-owned token accounts |
| `seed_prices(prices)` | admin | Pre-load historical prices, before the first settlement only |
| `request_subscription(amount)` | anyone | Queue a subscription |
| `request_redemption(shares)` | holder | Queue a redemption |
| `fix` | anyone | Fixing step, once a day after `settle_offset_secs` |
| `trade` | anyone | One capped trade; answers `NothingToTrade`, `TradeTooSoon`, `DepegBlocksBuy` or `FixPending` when there is none |
| `claim` | subscriber / redeemer | Collect shares or USDC |
| `init_mock_oracle`, `set_mock_price`, `init_mock_market`, `set_mock_market_fee` | admin | Devnet only |

### Configuration

| Field | Default | Meaning | |
|---|---|---|---|
| `window` | 50 | Moving-average days (max 64) | current |
| `settle_offset_secs` | 3600 | Fixing time after 00:00 UTC | current |
| `max_slippage_bps` | 30 | Keeper slippage bound | current |
| `subscription_fee_bps` / `redemption_fee_bps` | 0 | Subscription / redemption fee, kept in fund | current |
| `min_subscription` | 1 USDC | Minimum subscription | current |
| `max_staleness_secs` | 600 | Max oracle price age | current |
| `depeg_threshold_bps` | 200 | cbBTC vs BTC gap that stops buys | current |
| `trade_cap_usdc` | 200,000 USDC | Max notional per trade | current |
| `min_trade_interval_secs` | 60 | Seconds between trades | current |
| Public slippage bound | 10 bps | Non-keeper callers after 01:30 | planned |
| Subscription cap | 1,000,000 USDC | Per epoch, incl. pending | planned |
| Performance fee | 500 bps (max 2,000) | Above high-water mark | planned |
| Keeper, fee address | set at creation | Change via 48 h timelock | planned |

### Accounts (PDAs)

| Account | Seeds |
|---|---|
| Fund | `["fund", admin]` |
| Share mint | `["share_mint", fund]` |
| USDC vault / BTC vault | `["usdc_vault", fund]` / `["btc_vault", fund]` |
| Subscription escrow / Share escrow / Payout vault | `["subscription_escrow", fund]` / `["share_escrow", fund]` / `["payout_vault", fund]` |
| Ticket (per user) | `["ticket", fund, user]` |
| Epoch record | `["epoch", fund, epoch as u64 LE]` |
| Mock oracle / Mock market | `["oracle", authority]` / `["market", authority]` |
| Market vaults | `["market_usdc", market]` / `["market_btc", market]` |

### Units

| Quantity | Unit |
|---|---|
| USDC | 6 decimals |
| cbBTC | 8 decimals |
| Shares | 6 decimals; first share = 1 USDC |
| Price | micro-USD per 1 BTC |
| NAV | USDC base units per share base unit × 10¹² |

---

## Trade execution

| Technique | Purpose | Status |
|---|---|---|
| Oracle-based `min_out` | Reject bad fills and sandwiches | Implemented (per trade in target design) |
| Capped, spaced trades | Reduce price impact | Designed, ADR 0001 |
| Flows pay their own execution | Holders who stay don't pay for others | Designed, ADR 0002 |
| Keeper window + tighter public bound | Limit what third-party callers can extract | Designed |
| Randomized execution time | Harder to anticipate | Optional |
| MEV-protected submission (Jito) | Avoid public mempool | Mainnet |
| Limit orders (Jupiter) | Price control | Mainnet |

---

## Roadmap to mainnet

```mermaid
flowchart LR
  A["✓ Rename to<br/>subscription / redemption"] --> B["Migrate fund-logic<br/>ADRs 0001–0004"]
  B --> C["Pyth BTC/USD +<br/>cbBTC/USD"] & D["Jupiter<br/>route off-chain,<br/>min_out on-chain"]
  B --> E["Keeper · cancel ·<br/>MA backfill"]
  B --> F["Multisig · timelock ·<br/>subscription pause"]
  C & D & E & F --> G["Monitoring"] --> H["Security audit"] --> I["Legal review"]
```

### Known limitations

- *(current code)* A day without `fix` leaves a gap; the average covers the last 50 *observed* prices.
- cbBTC carries custodian risk. The depeg rule stops new buys but cannot recover lost value.
- Redemptions pay out only after the slice is fully sold, which can take more than a day in thin liquidity or a depeg.

---

## Disclaimer

Experimental prototype for educational purposes. Not investment advice, not audited, may contain bugs that lose all funds. Backtest results do not guarantee future performance.
