# Signal from cbBTC/USD when Pyth has it

The fund trades cbBTC, so the moving average and the hold-cbBTC-or-USDC signal use the Pyth cbBTC/USD fixing price when that feed exists, and fall back to BTC/USD otherwise. Each trade is still bounded by `min_out` from the current price and the slippage bound. This replaces the signal half of ADR 0003. Its depeg rule stays: when both feeds exist, a gap of more than 2% between cbBTC/USD and BTC/USD blocks buys, and sells continue.
