# Manager performance fee on a fund-level high-water mark

The manager earns 5% (capped at 20% in code) of NAV gains above a high-water mark. This is the one deliberate exception to "the admin can never take user funds".

```mermaid
flowchart LR
  N["NAV at 01:00"] --> Q{"NAV > high-water mark?"}
  Q -- no --> X["no fee"]
  Q -- yes --> F["5% of gain<br/>→ new shares to fee address<br/>(fixed at initialize)"]
  F --> H["mark := NAV after fee<br/>never reset"]
  X & H --> FL["then fill subscriptions<br/>and redemptions"]
```

## Considered Options

| Option | Rejected because |
|---|---|
| Per-user cost basis, charged at redemption | Needs non-transferable shares and per-user accounting |
| Fee kept in the fund | The fee is the manager's compensation |
