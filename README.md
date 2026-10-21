# tide

**A pooled Bitcoin fund on Solana: hold cbBTC when BTC is above its 50-day moving average, hold USDC when it is below.**

> **Devnet prototype. Do not use with real money.** Prices come from Pyth; the exchange is a mock that fills at the Pyth price (Jupiter is not integrated yet). Mainnet needs a security audit and legal advice: pooling other people's money and charging a fee for managing it is regulated in many jurisdictions.

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
  SP["signal price<br/>Pyth cbBTC/USD<br/>(BTC/USD if no feed)"] --> MA["50-day moving average"]
  SP --> SIG{"price > MA?"}
  MA --> SIG
  SIG -- yes --> T1["target: all cbBTC"]
  SIG -- no --> T2["target: all USDC"]
  SL["set aside redemption slice<br/>in kind, pro-rata"] --> NET["net against subscriptions<br/>at Pyth cbBTC/USD"]
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
  A["1. Unsold redemption slices<br/>oldest first"] --> C["2. Fund toward target"] --> B["3. Pending subscriptions<br/>oldest first"]
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
| partial fill | Filled part becomes shares by the cbBTC it adds, after the fund's rebalance ([ADR 0006](docs/adr/0006-shares-counted-in-fund-asset.md)) |
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
  CB["Pyth cbBTC/USD"] --> MA["moving average"] & SIG["signal"] & NT["netting"] & MIN["trade min_out"]
  BTC["Pyth BTC/USD"] -. "only if no cbBTC feed" .-> MA
  BTC & CB --> G{"gap > 2%?"}
  G -- "yes: depeg" --> NB["no cbBTC buys<br/>sells and settlement continue"]
```

The signal and netting use prices published in `[01:00, 01:01)` UTC. A missed day is backfilled with that day's Pyth price. The fund is never valued in USD ([ADR 0005](docs/adr/0005-signal-from-cbbtc-price.md), [ADR 0003](docs/adr/0003-btc-signal-cbbtc-valuation.md)).

### Withdrawal fee

```mermaid
flowchart LR
  C["redeemer claims USDC"] --> F["1% owed to the manager<br/>collect_fees → fee address"]
  C --> U["99% → redeemer"]
```

There is no performance fee. Redeemers pay the 1% on losses too ([ADR 0007](docs/adr/0007-withdrawal-fee-replaces-performance-fee.md)).

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
| Donations can't move the share price | Shares come from internal records, not token balances |
| No rounding dust | The last claimant of an epoch gets the exact remainder |
| Settlement never gets stuck | Unfinished work carries into the next settlement; a depeg only blocks buys |

---

## Repository layout

```
tide/
├── fund-logic/          Pure-Rust accounting + unit tests
├── programs/tide/       Anchor program: accounts and token transfers
├── tests/tide.ts        End-to-end test (bankrun, time travel)
├── scripts/crank.ts     Daily settlement script (devnet): Pyth prices, fix, trades, fees
├── scripts/setup.ts     Create a devnet fund (mock tokens and market)
├── scripts/faucet.ts    Mint mock USDC to a test wallet
├── scripts/share-metadata.ts  Name and icon for the share token (Metaplex metadata)
├── app/                 Web frontend (Vite + React) and app/api/faucet.ts, deployed on Vercel
├── .github/workflows/   Daily settlement on devnet (GitHub Actions)
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
  A["app/ (web)"] --> P
```

---

## Getting started

Needs Rust (stable), Solana CLI 2.x, Anchor 0.31.1 (via `avm`), Node.js 20+ and Yarn.

```bash
cd fund-logic && cargo test && cd ..   # logic tests, no Solana toolchain
anchor keys sync                       # write program ID
anchor build
yarn install && anchor test            # end-to-end
```

### Deploy to devnet

```bash
solana config set --url devnet && solana airdrop 2
anchor deploy --provider.cluster devnet
```

```mermaid
flowchart LR
  A["mock USDC (6)<br/>mock BTC (8) mints"] --> C["init_mock_market<br/>+ liquidity"] --> P["init_mock_pool<br/>+ interest reserve"] --> D["initialize"] --> E["init_vaults<br/>init_receipt_vault"] --> F["seed_prices<br/>last 49 days"]
```

`scripts/setup.ts` runs these steps with the wallet as admin, keeper and fee address (env: `PYTH_API_KEY` or `SEED_PRICE` for the seed price, `SETTLE_OFFSET_SECS`, `SIGNAL_FEED`, `TRADE_CAP_USDC`, `LENDING_CAP_BPS`, `POOL_APY_BPS`, `DEMO_USDC` for a first subscription). `scripts/faucet.ts <address> [usdc] [sol]` mints mock USDC (and sends a little SOL) to a test wallet; run it with the wallet that holds the mock USDC mint authority.

```bash
ANCHOR_PROVIDER_URL=https://api.devnet.solana.com ANCHOR_WALLET=$HOME/.config/solana/id.json yarn ts-node scripts/setup.ts
```

The GitHub Actions workflow `.github/workflows/crank.yml` runs the crank every day at 01:05 UTC (and on demand from the Actions tab) with the repository secrets `KEEPER_KEYPAIR` and `PYTH_API_KEY`. To run it yourself instead, run the crank daily after 01:00 UTC. It needs a Hermes API key from [Pyth Terminal](https://pythdata.app/signup) (Hermes requires one since August 2026), read from `PYTH_API_KEY`:

```cron
5 1 * * * cd /path/to/tide && PYTH_API_KEY=$(cat ~/.config/pyth/api-key) ANCHOR_PROVIDER_URL=https://api.devnet.solana.com ANCHOR_WALLET=$HOME/.config/solana/id.json yarn ts-node scripts/crank.ts >> crank.log 2>&1
```

```mermaid
flowchart LR
  H["Hermes<br/>signed Pyth prices"] --> CR["crank.ts"] -- "post update" --> R["Pyth receiver<br/>PriceUpdateV2 accounts"]
  CR -- "fix (fixing-window price), then trade<br/>every 60 s until done,<br/>then collect_fees" --> P["tide program"]
  R --> P
```

---

## Program reference

| Instruction | Who | Purpose |
|---|---|---|
| `initialize(config)` | admin | Create fund account and share mint |
| `init_vaults` | admin | Create the five fund-owned token accounts |
| `init_receipt_vault` | admin | Create the fund's token account for lending receipts |
| `init_share_metadata(name, symbol, uri)` | admin | Create the share token's Metaplex metadata so wallets show its name and icon; the admin becomes its update authority |
| `seed_prices(prices)` | admin | Pre-load the signal prices of the days ending yesterday, before the first settlement only |
| `request_subscription(amount, epoch)` | anyone | Queue a subscription; `epoch` is the day of the next fixing time, used for the ticket address |
| `request_redemption(shares, epoch)` | holder | Queue a redemption; `epoch` as above |
| `cancel` | ticket owner | Withdraw a queued ticket in full, until the fixing time that closes its epoch |
| `cancel_pending` | ticket owner | Any time after the fixing: refund the unfilled part of a subscription and take the shares already filled |
| `fix(epoch)` | keeper in 01:00–01:30, then anyone | Fixing step, once a day after `settle_offset_secs`; `epoch` is today's day number; pass Pyth price updates published in the fixing window |
| `trade` | keeper in 01:00–01:30, then anyone | One capped trade with current Pyth price updates (at most `max_staleness_secs` old); others in the window get `KeeperOnly`; while the target is USDC it also deposits to or withdraws from the lending pool (one action per call, ADR 0008); answers `NothingToTrade`, `TradeTooSoon`, `DepegBlocksBuy`, `FixPending` or `PoolIlliquid` when there is none |
| `claim` | subscriber / redeemer | Collect filled shares (repeatable) or redemption USDC; pass the record of the fixing that closed the ticket's epoch |
| `collect_fees` | anyone | Pay the withdrawal fees owed to the manager to the fee address; claims never depend on that account |
| `backfill_price(day)` | anyone | Record a missed day's signal price from a Pyth price update published in that day's fixing window |
| `propose_config(config)`, `apply_config` | admin | Config change, applied 48 h after the proposal |
| `pause_subscriptions`, `resume_subscriptions` | admin | Immediate; redemptions are never paused |
| `lower_lending_cap(bps)` | admin | Immediate, down to 0; raising goes through the timelock |
| `init_mock_market`, `set_mock_market_fee` | admin | Devnet only: a mock exchange that fills at the Pyth price, until Jupiter is integrated |
| `init_mock_pool(apy_bps)`, `set_mock_pool(apy_bps, withdraw_limit, haircut_bps)` | admin | Devnet only: a mock lending pool with a set rate, a withdrawable limit (0 = cannot pay) and a one-off loss |

Price inputs to `fix`, `trade` and `backfill_price` are two optional accounts, `btc_price_update` and `cbbtc_price_update`: Pyth `PriceUpdateV2` accounts. The program accepts only accounts owned by the Pyth receiver (`rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ`), fully verified through Wormhole, with the feed ID from the config; a confidence interval wider than `max_confidence_bps` of the price is rejected.

### Configuration

Every row below except "Config timelock" and "Subscription pause" is a field of the `config` passed to `initialize`. Fields change later only through `propose_config` / `apply_config`, except `window` and `settle_offset_secs`, which are fixed at creation.

| Field | Default | Meaning |
|---|---|---|
| `window` | 50 | Moving-average days (max 64) |
| `settle_offset_secs` | 3600 | Fixing time after 00:00 UTC |
| `max_slippage_bps` | 30 | Keeper slippage bound |
| `subscription_fee_bps` | 0 | Subscription fee, kept in fund |
| `min_subscription` | 1 USDC | Minimum subscription |
| `max_staleness_secs` | 120 (setup) | Max age of the Pyth price used by `trade` |
| `max_confidence_bps` | 100 | Pyth prices whose confidence interval is wider than this share of the price are rejected |
| `btc_feed_id` / `cbbtc_feed_id` | Pyth BTC/USD `e62df6…5b43` / cbBTC/USD `2817d7…9a97` | Pyth feed IDs; the cbBTC one is needed only with `signal_feed = CbBtc` |
| `depeg_threshold_bps` | 200 | cbBTC vs BTC gap that stops buys (with `CbBtc`; a missing BTC/USD reference also stops buys) |
| `signal_feed` | `Btc` | `CbBtc`: signal, moving average, netting and `min_out` use cbBTC/USD, and BTC/USD is the depeg reference (a missing or stale reference blocks buys). `Btc`: no cbBTC/USD feed, everything uses BTC/USD and there is no depeg check. Change via 48 h timelock; switching mixes the two feeds in the moving average for up to 50 days |
| `trade_cap_usdc` | 200,000 USDC | Max notional per trade |
| `min_trade_interval_secs` | 60 | Seconds between trades |
| `public_slippage_bps` | 10 | Non-keeper callers after the window (≤ `max_slippage_bps`) |
| `keeper_window_secs` | 1800 | Keeper-only time after the fixing time |
| `subscription_cap_usdc` | 1,000,000 USDC | Max subscription USDC not yet bought: queued plus pending; a request that would exceed it is rejected; redemptions are uncapped |
| `withdrawal_fee_bps` | 100 (max 100) | Of each redemption claim, rounded down, owed to the manager; share claims and refunds pay none |
| `keeper` | set at creation | Keeper address; change via 48 h timelock |
| Config timelock | 48 h, fixed in code | `propose_config` a whole config, `apply_config` after 48 h; a new proposal restarts the clock. `window` and `settle_offset_secs` are set at creation and cannot change |
| Subscription pause | off | `pause_subscriptions` / `resume_subscriptions`, immediate, admin only; redemptions can never be paused |
| `fee_address` | set at creation | Manager's wallet; `collect_fees` pays the owed fees to its USDC associated token account. Change via 48 h timelock |
| `lending_cap_bps` | 8000 | Share of the fund's USDC (vault + lent) that may be lent while the target is USDC. `lower_lending_cap` lowers it at once; raising goes through the timelock |

### Accounts (PDAs)

| Account | Seeds |
|---|---|
| Fund | `["fund", admin]` |
| Share mint | `["share_mint", fund]` |
| USDC vault / BTC vault | `["usdc_vault", fund]` / `["btc_vault", fund]` |
| Subscription escrow / Share escrow / Payout vault | `["subscription_escrow", fund]` / `["share_escrow", fund]` / `["payout_vault", fund]` |
| Ticket (per user per epoch) | `["ticket", fund, user, epoch as u64 LE]` |
| Epoch record | `["epoch", fund, epoch as u64 LE]` (epoch = fixing day, unix / 86400) |
| Mock market | `["market", authority]` |
| Market vaults | `["market_usdc", market]` / `["market_btc", market]` |
| Mock lending pool | `["pool", authority]` |
| Pool USDC vault / receipt mint | `["pool_usdc", pool]` / `["pool_receipt", pool]` |
| Fund receipt vault | `["receipt_vault", fund]` |

### Units

| Quantity | Unit |
|---|---|
| USDC | 6 decimals |
| cbBTC | 8 decimals |
| Shares | 6 decimals; first share = 1 USDC |
| Price | micro-USD per 1 BTC |

---

## Trade execution

| Technique | Purpose | Status |
|---|---|---|
| Oracle-based `min_out` | Reject bad fills and sandwiches | Implemented with Pyth, checked on every trade |
| Capped, spaced trades | Reduce price impact | Implemented, ADR 0001 |
| Flows pay their own execution | Holders who stay don't pay for others | Implemented, ADR 0002 / 0006 |
| Keeper window + tighter public bound | Limit what third-party callers can extract | Implemented |
| Randomized execution time | Harder to anticipate | Optional |
| MEV-protected submission (Jito) | Avoid public mempool | Mainnet |
| Limit orders (Jupiter) | Price control | Mainnet |

---

## Roadmap to mainnet

```mermaid
flowchart LR
  A["✓ Rename to<br/>subscription / redemption"] --> B["✓ Migrate settlement<br/>ADRs 0001–0007"]
  B --> C["Pyth BTC/USD +<br/>cbBTC/USD"] & D["Jupiter<br/>route off-chain,<br/>min_out on-chain"]
  B --> E["✓ Keeper · cancel ·<br/>MA backfill"]
  B --> F["✓ Timelock · subscription pause<br/>(admin is an external multisig)"]
  B --> L["Real lending pool<br/>(mock done, ADR 0008)"]
  C & D & E & F & L --> G["Monitoring"] --> H["Security audit"] --> I["Legal review"]
```

### Known limitations

- A day without `fix` leaves a gap until someone backfills it. A day never backfilled is skipped, and the average uses an earlier day instead. Prices are stored for 64 days, so if fewer than 50 of the last 64 days have a price, the fund holds USDC.
- cbBTC carries custodian risk. The depeg rule stops new buys but cannot recover lost value.
- Redemptions pay out only after the slice is fully sold, which can take more than a day in thin liquidity or a depeg.

---

## Disclaimer

Experimental prototype for educational purposes. Not investment advice, not audited, may contain bugs that lose all funds. Backtest results do not guarantee future performance.
