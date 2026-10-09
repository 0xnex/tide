# AGENTS.md

Guidance for AI coding agents (and human contributors) working on this repository. Read this before changing anything. Money logic bugs in this project can lock or drain user funds, so the rules below are strict on purpose.

## Project in one paragraph

`tide` is a Solana (Anchor) program for a pooled BTC fund. Users queue USDC deposits and share withdrawals; once a day after 01:00 UTC anyone calls `settle`, which records the oracle price, compares it with the 50-day moving average, fills the queue at a single NAV, and swaps the fund fully into BTC (above the average) or USDC (below). Users then `claim` their shares or USDC. It is a **devnet prototype**: oracle and exchange are mocks.

## Architecture

```
fund-logic/            ← ALL accounting. Pure Rust, no dependencies, fully unit-tested.
programs/tide/         ← Thin Anchor wrapper: validates accounts, moves tokens, calls fund-logic.
tests/                 ← End-to-end tests (TypeScript, bankrun with clock control).
scripts/crank.ts       ← Daily settlement script (devnet).
```

**Rule: arithmetic about money belongs in `fund-logic`, never in the Anchor program.** The program may only:
- convert between Anchor account structs and `fund_logic` structs,
- perform CPIs (transfer, mint, burn) for amounts that `fund-logic` returned,
- check things `fund-logic` cannot see (signers, PDAs, oracle staleness, mint decimals).

If you need new math, add a function to `fund-logic`, test it there, then call it from the program.

## Commands

| Task | Command | Needs Solana toolchain |
|---|---|---|
| Logic tests | `cd fund-logic && cargo test` | No |
| Format | `rustfmt --edition 2021 fund-logic/src/lib.rs programs/tide/src/lib.rs` | No |
| Sync program ID | `anchor keys sync` | Yes |
| Build | `anchor build` | Yes |
| End-to-end tests | `yarn install && anchor test` | Yes |

`fund-logic` is its own Cargo workspace (empty `[workspace]` in its `Cargo.toml`) so it builds and tests offline without Anchor. Do not add it back to the root workspace members.

Before finishing any change: `cd fund-logic && cargo test` must pass. If you touched the program, `anchor build` and `anchor test` must pass too. If you cannot run them in your environment, say so explicitly in your summary.

## Invariants — never break these

1. **Forward pricing.** Deposits and withdrawals are only filled inside `settle`, at the NAV computed in that settlement *before* the swap. Never fill at a previously stored NAV.
2. **Fixed pricing time.** NAV, the moving average and the signal use `SettlePrices.fix`, the price published in `[fix_time, fix_time + FIX_WINDOW_SECS)` where `fix_time` = UTC midnight + `settle_offset_secs`. Never the price at the moment `settle` is called: anyone can call it, so a call-time price lets the caller choose their own fill. `request_deposit` / `request_withdraw` are rejected (`QueueClosed`) from `fix_time` until that day is settled, because the fixing price is already public.
3. **Internal accounting.** NAV uses `FundState.usdc`, `FundState.btc` and `FundState.share_supply`. Never derive NAV from token-account balances (prevents donation/inflation attacks).
4. **Exact claims.** `claim` uses `take_pro_rata`, which gives the last claimant the exact remainder. After all tickets of an epoch are claimed, `unclaimed_*` fields are zero and the sum of claims equals the epoch totals.
5. **Share supply.** `FundState.share_supply` must always equal (shares held by users) + (shares queued for withdrawal). Tests assert this exactly.
6. **No stuck states.** A settlement must never fail permanently. In particular, a full exit while holding BTC must succeed even with trading costs (`finish_settle` caps the payout to available USDC only when the swap was a `SellBtc`).
7. **Slippage guard.** Every swap passes `min_out` derived from the latest oracle price (`SettlePrices.exec`); `finish_settle` rejects fills below it, which reverts the whole transaction. `exec` may differ from `fix` by at most `MAX_PRICE_DRIFT_BPS`, otherwise `begin_settle` fails with `PriceDrift` (that day is skipped, the next day settles normally), because in a near-full exit the drift would fall on the holders who stay.
8. **Integer math only.** No floats in `fund-logic` or the program. Use `u128` intermediates and checked arithmetic; round in favor of the fund (down for amounts paid out, up for amounts the fund must sell to cover a payout).
9. **One settlement per UTC day,** only after `settle_offset_secs`.
10. **Atomicity.** In `settle`, all token movements and the `finish_settle` check happen in one instruction so any failure rolls everything back.

## Keeping the two layers in sync

These are easy to break when editing:

- `StateData` (program) mirrors `fund_logic::FundState` field-for-field. Update `from_logic` / `to_logic` together.
- `StateData.prices` is `[u64; 64]` as a **literal** (the IDL needs it). It must equal `fund_logic::MAX_WINDOW`; a compile-time assertion enforces this.
- `EpochRecord` (program) mirrors `fund_logic::EpochRecord`, including the four `unclaimed_*` fields. Update `set_from_logic`, `set_unclaimed`, and `to_logic`.
- Every `fund_logic::FundError` variant needs a matching `FundErrorCode` and an arm in `map_err`. The match is exhaustive, so the compiler will remind you.
- `ConfigData` has one extra field (`max_staleness_secs`) that is checked in the program, not in `fund-logic`.

## Units

| Quantity | Representation |
|---|---|
| USDC | `u64`, 6 decimals |
| BTC (wrapped) | `u64`, 8 decimals |
| Shares | `u64`, 6 decimals; initial NAV is 1 share = 1 USDC |
| Price | `u64`, micro-USD per 1 BTC |
| NAV | `u128`, USDC base units per share base unit × `NAV_SCALE` (10¹²) |
| Basis points | `u16`, out of 10,000 |

`initialize` enforces USDC = 6 decimals and BTC = 8 decimals. Don't relax this without updating every conversion.

## PDA seeds

| Account | Seeds |
|---|---|
| Fund | `"fund", admin` |
| Share mint | `"share_mint", fund` |
| Vaults | `"usdc_vault" / "btc_vault" / "deposit_escrow" / "share_escrow" / "payout_vault", fund` |
| Ticket | `"ticket", fund, user` |
| Epoch record | `"epoch", fund, epoch.to_le_bytes()` |
| Mock oracle / market | `"oracle", authority` / `"market", authority` |
| Market vaults | `"market_usdc" / "market_btc", market` |

Changing seeds breaks existing deployments and the tests/scripts that derive these addresses. Update `tests/tide.ts` and `scripts/crank.ts` in the same change.

## Testing expectations

- **Every behavior change needs a `fund-logic` test.** Prefer a focused scenario test plus coverage in the randomized simulations.
- The two randomized tests (`random_simulation_invariants`, `random_simulation_with_trading_costs_never_gets_stuck`) are the main safety net. Do not weaken their assertions to make a change pass; if one fails, the change is probably wrong.
- When fixing a bug, first add a test that reproduces it.
- End-to-end tests control time with bankrun's `setClock`; advance the slot before setting the clock so identical transactions are not deduplicated.

## Mock components

`MockOracle`, `MockMarket`, `init_mock_oracle`, `set_mock_price`, and `init_mock_market` exist only for devnet. When replacing them:

- **Oracle (Pyth):** check publish time against `max_staleness_secs`, reject wide confidence intervals, convert the exponent to micro-USD carefully.
- **Exchange (Jupiter):** compute the route off-chain, pass route accounts into `settle` (or a separate `rebalance` instruction), and keep the on-chain `min_out` check against the oracle. Never trust a price supplied by the caller.
- Remove the mock instructions from mainnet builds (e.g. behind a cargo feature).

## Security checklist for any change

- [ ] Every account is constrained (seeds, `has_one`, `address`, mint, authority).
- [ ] Only the fund PDA can move vault tokens; signer seeds are `["fund", admin, bump]`.
- [ ] No arithmetic that can overflow or underflow without a checked operation.
- [ ] No path where a user can claim twice, claim another user's ticket, or claim before settlement.
- [ ] No new instruction lets the admin take user funds. (Currently the admin cannot withdraw from vaults; keep it that way.)
- [ ] The fund cannot end up in a state where `settle` always fails.

## Things not to do

- Don't add strategy "improvements" (different windows, extra indicators, leverage, altcoins) without a backtest. The project's research found that short-term altcoin strategies lost money; the 50-day rule was chosen deliberately and its edge is modest.
- Don't add marketing language implying guaranteed or safe returns anywhere in code, docs, or UI.
- Don't deploy to mainnet or accept outside funds. That requires an audit and legal review first; flag it to the maintainer instead.
- Don't disable TLS verification or embed private keys in scripts.

## Style

- Rust: `rustfmt` defaults (edition 2021). Small pure functions in `fund-logic`; return `Result<_, FundError>`.
- Comments explain *why*, especially around rounding direction and ordering inside `settle`.
- TypeScript: keep helper functions in tests small; derive PDAs with the same seeds as the program.
