# tide

**A pooled Bitcoin fund on Solana that follows one rule: hold BTC when the price is above its 50-day moving average, hold USDC when it is below.**

Anyone can deposit USDC and receive fund shares. Once a day, after 01:00 UTC, anyone can trigger settlement: the program records the price published at 01:00 UTC, checks the 50-day moving average, prices all queued deposits and withdrawals at the same net asset value (NAV), and rebalances the whole fund into BTC or USDC.

> **Status: devnet prototype. Do not use with real money.**
> Prices come from an admin-controlled mock oracle and swaps go through a mock exchange. Before any mainnet use you need a real oracle (e.g. Pyth), a real swap route (e.g. Jupiter), a professional security audit, and legal advice. Pooling other people's money into a managed strategy is a regulated activity in many jurisdictions.

[中文说明](README.zh-CN.md)

---

## Why this rule

The strategy came out of a backtest study on Binance data (included in the research that led to this project):

| Approach (BTC, Mar 2018 – Sep 2026) | Total return | Max drawdown |
|---|---|---|
| Buy and hold | +872% | −77% |
| 50-day MA filter | +2,719% | −58% |
| 100-day MA filter | +2,076% | −39% |
| 200-day MA filter | +1,083% | −64% |

All three filters cut the deepest drawdowns, mainly by sitting in cash through the 2018 and 2022 bear markets. Which window worked best changed between periods, so treat 50 days as a reasonable default, not an optimum. Past results do not predict future returns, and drawdowns of 40–60% still happened with the filter on.

The same study found that short-term momentum and breakout strategies on altcoins lost money and did worse than random coin selection, which is why this fund holds only BTC.

---

## How it works

```
 before 01:00 UTC         daily, after 01:00 UTC                  after settlement
┌──────────────────┐     ┌──────────────────────────────────┐    ┌──────────────────┐
│ request_deposit  │     │ settle                           │    │ claim            │
│ request_withdraw │ ──▶ │ 1. read the 01:00 oracle price   │──▶ │ receive shares   │
│ (queued)         │     │ 2. 50-day MA → BTC or USDC       │    │ or USDC          │
└──────────────────┘     │ 3. NAV → process queue           │    └──────────────────┘
                         │ 4. swap with slippage guard      │
                         └──────────────────────────────────┘
```

### Design decisions

- **Forward pricing.** Deposits and withdrawals are queued and filled at the *next* settlement's NAV. Nobody can buy in at a stale price after BTC has already moved.
- **Fixed pricing time.** NAV and the BTC/USDC signal use the oracle price published in the first 60 seconds after 01:00 UTC, no matter when `settle` is called, so the caller cannot pick a favourable moment. The swap itself is sized from the latest price, which may differ from the 01:00 price by at most 1%; otherwise settlement reverts and that day is skipped.
- **Queue closes at the fixing time.** Between 01:00 UTC and that day's settlement, `request_deposit` and `request_withdraw` are rejected, because the 01:00 price is already public. Requests reopen right after settlement (for the next day) or at 00:00 UTC the next day if nobody settled.
- **Internal accounting.** NAV is computed from the program's own records, not token-account balances, so tokens sent directly to a vault cannot manipulate the share price.
- **Exact pro-rata claims.** Each epoch's shares and payouts are split pro-rata; the last claimant receives the exact remainder. No rounding dust accumulates. (An earlier version left dust shares that could permanently lock the fund once all real holders exited — caught by randomized tests and fixed.)
- **Slippage guard.** A swap that returns less than the oracle price minus `max_slippage_bps` (default 0.3%: Binance's 0.075% fee plus about 0.1% slippage, with headroom for oracle-vs-exchange price differences) reverts the whole settlement.
- **Full exits never get stuck.** If selling BTC to pay withdrawals leaves USDC slightly short because of trading costs, that period's withdrawers absorb the shortfall pro-rata.
- **Optional fees.** Deposit and withdrawal fees (default 0) stay inside the fund, so the people generating trades pay their own trading costs.
- **Permissionless settlement.** Anyone can call `settle`; a cron job (`scripts/crank.ts`) does it daily on devnet. With the mock oracle there is only one price, so it must be written and settled within 60 seconds of 01:00 UTC; a real oracle (Pyth) can supply the 01:00 price later in the day.

---

## Repository layout

```
tide/
├── fund-logic/              Pure-Rust accounting (no dependencies) + 18 unit tests
│   └── src/lib.rs
├── programs/tide/           Anchor program: account validation and token transfers
│   └── src/lib.rs
├── tests/tide.ts            End-to-end test using bankrun (time-travels across 4 days)
├── scripts/crank.ts         Daily settlement script for devnet
├── Anchor.toml
├── Cargo.toml               Workspace (the program)
├── package.json
├── AGENTS.md                Guide for AI coding agents and contributors
└── README.zh-CN.md          Chinese README
```

All money logic lives in `fund-logic` so it can be tested without any Solana tooling. The Anchor program only moves tokens and calls into it.

---

## Getting started

### Prerequisites

- Rust (stable)
- Solana CLI 2.x
- Anchor 0.31.1 (install via `avm`)
- Node.js 20+ and Yarn

### Build and test

```bash
# 1. Pure logic tests — no Solana toolchain needed
cd fund-logic && cargo test && cd ..

# 2. Generate a program ID and write it into the code
anchor keys sync

# 3. Build the program
anchor build

# 4. End-to-end tests
yarn install
anchor test
```

> The Anchor program has not yet been compiled by the original author's environment (it had no access to crates.io or the Solana toolchain). `fund-logic` tests all pass. If `anchor build` fails, the error is most likely a small API mismatch in the wrapper, not in the accounting.

### Deploy to devnet

```bash
solana config set --url devnet
solana airdrop 2
anchor deploy --provider.cluster devnet
```

Then initialize in this order (see `before()` in `tests/tide.ts`):

1. Create mock USDC (6 decimals) and mock BTC (8 decimals) mints
2. `init_mock_oracle` with the current BTC price
3. `init_mock_market` and fund its two vaults with liquidity
4. `initialize` with your config
5. `init_vaults`
6. `seed_prices` with the last 49 daily closing prices, so the moving average works from day one

Run settlement daily at 01:00 UTC (server clock in UTC; the mock price must be posted within 60 seconds of 01:00):

```cron
0 1 * * * cd /path/to/tide && ANCHOR_PROVIDER_URL=https://api.devnet.solana.com ANCHOR_WALLET=$HOME/.config/solana/id.json yarn ts-node scripts/crank.ts >> crank.log 2>&1
```

The crank script reads the BTC price from Binance's public API, writes it to the mock oracle (admin wallet required), and calls `settle`.

---

## Program reference

### Instructions

| Instruction | Who | Purpose |
|---|---|---|
| `initialize(config)` | admin | Create the fund account and share mint |
| `init_vaults` | admin | Create the five fund-owned token accounts |
| `seed_prices(prices)` | admin | Pre-load up to `window` historical prices; only before the first settlement |
| `request_deposit(amount)` | anyone | Queue a USDC deposit for the current epoch |
| `request_withdraw(shares)` | holder | Queue a share redemption for the current epoch |
| `settle` | anyone | Daily settlement after `settle_offset_secs` past 00:00 UTC |
| `claim` | depositor / withdrawer | Collect shares or USDC from a settled epoch |
| `init_mock_oracle`, `set_mock_price` | admin | Devnet only |
| `init_mock_market` | admin | Devnet only |

### Configuration

| Field | Default | Meaning |
|---|---|---|
| `window` | 50 | Moving-average length in days (max 64) |
| `settle_offset_secs` | 3600 | Seconds after 00:00 UTC before settlement is allowed |
| `max_slippage_bps` | 30 | Max swap shortfall vs. oracle price |
| `deposit_fee_bps` | 0 | Retained by the fund |
| `withdraw_fee_bps` | 0 | Retained by the fund |
| `min_deposit` | 1 USDC | Minimum deposit |
| `max_staleness_secs` | 600 | Max oracle price age at settlement |

### Accounts (PDAs)

| Account | Seeds |
|---|---|
| Fund | `["fund", admin]` |
| Share mint | `["share_mint", fund]` |
| USDC vault / BTC vault | `["usdc_vault", fund]` / `["btc_vault", fund]` |
| Deposit escrow / Share escrow / Payout vault | `["deposit_escrow", fund]` / `["share_escrow", fund]` / `["payout_vault", fund]` |
| Ticket (per user) | `["ticket", fund, user]` |
| Epoch record | `["epoch", fund, epoch as u64 LE]` |
| Mock oracle / Mock market | `["oracle", authority]` / `["market", authority]` |
| Market vaults | `["market_usdc", market]` / `["market_btc", market]` |

### Units

| Quantity | Unit |
|---|---|
| USDC | 6 decimals |
| BTC (wrapped, e.g. cbBTC) | 8 decimals |
| Shares | 6 decimals; first share = 1 USDC |
| Price | micro-USD per 1 BTC (USD × 10⁶) |
| NAV | USDC base units per share base unit × 10¹² |

---

## Trade execution

A fund that trades at a fixed time on public rules can be front-run, can move the price on large orders, and can see pool prices drift from the oracle.

| Technique | Purpose | Status |
|---|---|---|
| Oracle-based minimum output | Reject bad fills and sandwiches | Implemented |
| Deposit / withdrawal fees | Flows pay their own trading cost | Implemented (default 0) |
| TWAP (split into several trades) | Reduce price impact and predictability | Planned, once size warrants it |
| Randomized execution time | Harder to anticipate | Optional |
| MEV-protected submission (e.g. Jito) | Avoid public mempool exposure | Mainnet |
| Limit orders (e.g. Jupiter) | Price control | Mainnet |

---

## Roadmap to mainnet

- [ ] Replace mock oracle with Pyth BTC/USD (check confidence interval and publish time)
- [ ] Replace mock market with Jupiter (route off-chain, verify min-out on-chain against the oracle)
- [ ] `cancel_request` instruction
- [ ] Pause switch, multisig admin, timelock on config changes
- [ ] Monitoring and alerting for missed settlements
- [ ] TWAP execution
- [ ] Independent security audit
- [ ] Legal review in every jurisdiction where the fund is offered

### Known limitations

- If nobody calls `settle` on a given day, that day's price is missing; the moving average is computed over the last `window` *observed* prices.
- Wrapped BTC on Solana (e.g. cbBTC) carries custodian risk on top of BTC price risk.
- When the fund sells BTC with a margin to cover withdrawals, any leftover USDC stays in cash until the next settlement.

---

## Disclaimer

This software is an experimental prototype provided for educational purposes. It is not investment advice, has not been audited, and may contain bugs that lead to total loss of funds. Historical backtest results do not guarantee future performance.
