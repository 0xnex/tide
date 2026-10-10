# A 1% withdrawal fee replaces the performance fee

The manager is paid 1% of the USDC each redeemer claims, sent to the fee address fixed at `initialize`. The rate is config, changeable only through the timelock and capped at 1% in code. It also replaces the optional redemption fee that stayed in the fund. There is no performance fee, no high-water mark and no per-user cost basis. Shares therefore stay ordinary transferable tokens. This supersedes ADR 0004 and is the only value that goes to a non-user.

## Considered Options

| Option | Rejected because |
|---|---|
| 5% of NAV gains above a fund-level high-water mark (ADR 0004) | Needs a USD NAV, which ADR 0006 removes |
| 5% of each redeemer's gain, from an average cost per user | Needs per-user cost records and non-transferable shares |

## Consequences

- Redeemers pay the fee on losses too.
