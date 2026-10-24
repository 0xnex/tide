import { BorshCoder, EventParser } from "@coral-xyz/anchor";
import type { Connection } from "@solana/web3.js";
import idl from "./idl/tide.json";
import type { Tide } from "./idl/tide";
import { FUND, PROGRAM_ID } from "./chain";

export interface FundTrade {
  signature: string;
  blockTime: number | null;
  side: "buy" | "sell";
  purpose: "rebalance" | "subscription" | "redemption";
  btcAmount: bigint; // 8 decimals
  usdcAmount: bigint; // 6 decimals
}

const PAGE_SIZE = 5;
const parser = new EventParser(PROGRAM_ID, new BorshCoder(idl as Tide));

/** Read one page of successful cbBTC trades from the program's on-chain events. */
export async function loadFundTrades(
  connection: Connection,
  before?: string,
): Promise<{ trades: FundTrade[]; next: string | null }> {
  const signatures = await connection.getSignaturesForAddress(FUND, { limit: PAGE_SIZE, before }, "confirmed");
  const trades: FundTrade[] = [];
  for (const [index, entry] of signatures.entries()) {
    if (entry.err) continue;
    // Public devnet RPCs limit transaction lookups. A small gap keeps pagination usable there.
    if (index > 0) await new Promise((resolve) => setTimeout(resolve, 1_000));
    const tx = await connection.getTransaction(entry.signature, {
      commitment: "confirmed",
      maxSupportedTransactionVersion: 0,
    });
    if (!tx?.meta || tx.meta.err) continue;
    for (const event of parser.parseLogs(tx.meta.logMessages ?? [])) {
      if (event.name !== "Traded") continue;
      const data = event.data as {
        side: { BuyBtc?: object; SellBtc?: object };
        amount_in: { toString(): string };
        amount_out: { toString(): string };
        record_epoch: unknown;
      };
      const buy = "BuyBtc" in data.side;
      if (!buy && !("SellBtc" in data.side)) continue;
      trades.push({
        signature: entry.signature,
        blockTime: entry.blockTime ?? null,
        side: buy ? "buy" : "sell",
        purpose: data.record_epoch === null ? "rebalance" : buy ? "subscription" : "redemption",
        btcAmount: BigInt((buy ? data.amount_out : data.amount_in).toString()),
        usdcAmount: BigInt((buy ? data.amount_in : data.amount_out).toString()),
      });
    }
  }
  return { trades, next: signatures.length === PAGE_SIZE ? signatures[signatures.length - 1].signature : null };
}

/** Effective execution price in micro-USDC per BTC, rounded down. */
export function tradePrice(trade: FundTrade): bigint | null {
  return trade.btcAmount > 0n ? (trade.usdcAmount * 100_000_000n) / trade.btcAmount : null;
}
