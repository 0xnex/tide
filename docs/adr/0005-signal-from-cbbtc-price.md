# Signal from cbBTC/USD when Pyth has it

> Status: amended. The feed is not detected at run time: the `signal_feed` config names it, and the default is BTC/USD. Choosing cbBTC/USD is the case this ADR describes.

The fund trades cbBTC, so the moving average and the hold-cbBTC-or-USDC signal use the Pyth cbBTC/USD fixing price when that feed exists, and fall back to BTC/USD otherwise. Each trade is still bounded by `min_out` from the current price and the slippage bound. This replaces the signal half of ADR 0003. Its depeg rule stays: when both feeds exist, a gap of more than 2% between cbBTC/USD and BTC/USD blocks buys, and sells continue.

## Consequences

- One price drives the signal, the moving average, netting and `min_out`: the feed named by the `signal_feed` config (set at `initialize`, changed only through the timelock).
- With cbBTC/USD, BTC/USD is only the depeg reference. A missing or stale reference blocks buys and netting but never sells or the fixing step.
- With only BTC/USD there is nothing to compare, so there is no depeg check and trades rely on the slippage bound.
- Switching the feed mixes the two in the moving average until the older prices leave the window; outside a depeg they track each other closely.
