# Settlement is a multi-step process of capped trades

Swapping the whole fund at once costs too much in price impact. Settlement is therefore one fixing step followed by trades of at most 200K USDC, spaced at least 60 s apart. Each step is atomic on its own; work left at the next fixing carries over, so settlement never gets stuck.

```mermaid
sequenceDiagram
  participant K as Keeper (anyone after 01:30)
  participant P as tide program
  participant J as Jupiter
  K->>P: fix (01:00 prices)
  Note over P: NAV · signal · fee · redemption slice · netting
  loop until done (≤ 200K each, ≥ 60 s apart)
    K->>P: trade
    P->>J: swap
    J-->>P: out (must be ≥ min_out)
  end
  Note over P: unfinished at next 01:00 → next settlement
```

## Considered Options

| Option | Rejected because |
|---|---|
| One atomic swap per day (original) | Heavy price impact; a swap beyond the slippage bound fails every day |
| Daily cap with gated redemptions | Requests should complete the same day when liquidity allows |
