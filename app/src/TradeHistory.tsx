import { useCallback, useEffect, useRef, useState } from "react";
import { useConnection } from "@solana/wallet-adapter-react";
import * as fmt from "./format";
import { loadFundTrades, tradePrice, type FundTrade } from "./trades";

export function TradeHistory() {
  const { connection } = useConnection();
  const [trades, setTrades] = useState<FundTrade[]>([]);
  const [next, setNext] = useState<string | null | undefined>(undefined);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inFlight = useRef(false);

  const load = useCallback(
    async (before?: string) => {
      if (inFlight.current) return;
      inFlight.current = true;
      setLoading(true);
      try {
        const page = await loadFundTrades(connection, before);
        setTrades((previous) => {
          const bySignature = new Map(previous.map((t) => [t.signature, t]));
          for (const trade of page.trades) bySignature.set(trade.signature, trade);
          return [...bySignature.values()].sort((a, b) => (b.blockTime ?? 0) - (a.blockTime ?? 0));
        });
        setNext((current) => (before || current === undefined ? page.next : current));
        setError(null);
      } catch (e) {
        setError(String((e as Error)?.message ?? e));
      } finally {
        inFlight.current = false;
        setLoading(false);
      }
    },
    [connection],
  );

  useEffect(() => {
    void load();
    const timer = setInterval(() => void load(), 60_000);
    return () => clearInterval(timer);
  }, [load]);

  return (
    <section className="card">
      <h2>Vault trades</h2>
      <p className="muted small">Executed cbBTC trades, including subscriptions and redemptions. Price comes from the actual amounts.</p>
      {trades.length > 0 && (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Time</th>
                <th>Action</th>
                <th className="num">cbBTC</th>
                <th className="num">Price / BTC</th>
                <th className="num">Total USDC</th>
                <th>Transaction</th>
              </tr>
            </thead>
            <tbody>
              {trades.map((trade) => {
                const price = tradePrice(trade);
                return (
                  <tr key={trade.signature}>
                    <td>{trade.blockTime ? fmt.timeLabel(trade.blockTime) : "—"}</td>
                    <td>
                      <span className={`pill ${trade.side === "buy" ? "pill-blue" : "pill-gray"}`}>
                        {trade.side === "buy" ? "Buy" : "Sell"}
                      </span>
                      <span className="trade-purpose muted">{trade.purpose}</span>
                    </td>
                    <td className="num">{fmt.fixed(trade.btcAmount, 8, 8)}</td>
                    <td className="num">
                      {price === null
                        ? "—"
                        : `$${(Number(price) / 1e6).toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`}
                    </td>
                    <td className="num">{fmt.fixed(trade.usdcAmount, 6, 6)}</td>
                    <td>
                      <a href={fmt.explorer(trade.signature)} target="_blank" rel="noreferrer">
                        {fmt.short(trade.signature)} ↗
                      </a>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
      {!loading && trades.length === 0 && !error && <p className="muted">No vault trades found.</p>}
      {error && <p className="trade-error">Could not load trades: {error}</p>}
      {loading && <p className="muted">Loading trades…</p>}
      {next && !loading && <button className="btn small" onClick={() => void load(next)}>Load older trades</button>}
    </section>
  );
}
