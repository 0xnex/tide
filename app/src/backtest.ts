// Backtest of the fund's rule on daily BTC/USD closes, for display only (the fund's accounting
// lives in fund-logic). Each day's close stands in for the 01:00 UTC fixing: it goes into the
// 50-day average, and if the close is above the average the strategy holds BTC for the next day,
// otherwise USDC. Each switch costs SWITCH_COST_BPS (trading fee plus slippage).

export const MA_DAYS = 50;
export const SWITCH_COST_BPS = 20;
export const WINDOWS = [
  { key: "30D", days: 30 },
  { key: "90D", days: 90 },
  { key: "180D", days: 180 },
  { key: "1Y", days: 365 },
] as const;
export type WindowKey = (typeof WINDOWS)[number]["key"];

export interface DailyClose {
  day: number; // unix day number (UTC) the candle covers
  close: number; // USD
}

export interface Point {
  day: number;
  strategy: number; // value of 1 invested at the window start
  hold: number;
  inBtc: boolean; // position held during this day
}

export interface Result {
  points: Point[];
  strategyReturn: number;
  holdReturn: number;
  strategyDrawdown: number;
  holdDrawdown: number;
  switches: number;
  timeInBtc: number; // share of days held in BTC
}

export interface Spot {
  price: number; // BTC/USD now
  change24h: number | null; // change since 24 hours ago (0.01 = +1%)
}

/** BTC/USD now and its 24-hour change (Coinbase); null when it cannot be read */
export async function fetchSpot(): Promise<Spot | null> {
  try {
    const r = await fetch("https://api.exchange.coinbase.com/products/BTC-USD/stats");
    if (!r.ok) return null;
    const j = (await r.json()) as { last?: string; open?: string };
    const price = Number(j.last);
    const open = Number(j.open);
    if (!(price > 0)) return null;
    return { price, change24h: open > 0 ? price / open - 1 : null };
  } catch {
    return null;
  }
}

/** Daily BTC/USD closes, oldest first, without today's unfinished candle. Coinbase, then Binance. */
export async function fetchDailyCloses(days = 600): Promise<DailyClose[]> {
  try {
    return await fromCoinbase(days);
  } catch {
    return await fromBinance(days);
  }
}

async function fromCoinbase(days: number): Promise<DailyClose[]> {
  const out = new Map<number, number>();
  const now = Math.floor(Date.now() / 1000);
  // Coinbase returns at most 300 candles per request. Each request ends where the previous one starts, so the
  // candle on the boundary is never skipped (the map drops the duplicate).
  for (let end = now; out.size < days && end > now - (days + 300) * 86_400; end -= 299 * 86_400) {
    const start = end - 299 * 86_400;
    const url = `https://api.exchange.coinbase.com/products/BTC-USD/candles?granularity=86400&start=${new Date(start * 1000).toISOString()}&end=${new Date(end * 1000).toISOString()}`;
    const r = await fetch(url);
    if (!r.ok) throw new Error(`Coinbase HTTP ${r.status}`);
    const rows = (await r.json()) as number[][]; // [time, low, high, open, close, volume]
    if (rows.length === 0) break;
    for (const [time, , , , close] of rows) out.set(Math.floor(time / 86_400), close);
  }
  return finish(out);
}

async function fromBinance(days: number): Promise<DailyClose[]> {
  const r = await fetch(`https://data-api.binance.vision/api/v3/klines?symbol=BTCUSDT&interval=1d&limit=${Math.min(days, 1000)}`);
  if (!r.ok) throw new Error(`Binance HTTP ${r.status}`);
  const rows = (await r.json()) as (string | number)[][]; // [openTime ms, open, high, low, close, ...]
  const out = new Map<number, number>();
  for (const row of rows) out.set(Math.floor(Number(row[0]) / 86_400_000), Number(row[4]));
  return finish(out);
}

function finish(m: Map<number, number>): DailyClose[] {
  const today = Math.floor(Date.now() / 86_400_000);
  return [...m.entries()]
    .filter(([day, close]) => day < today && close > 0)
    .sort((a, b) => a[0] - b[0])
    .map(([day, close]) => ({ day, close }));
}

/** true when the close on index i is above the average of the last MA_DAYS closes (including i) */
function aboveAverage(closes: DailyClose[], i: number): boolean {
  let sum = 0;
  for (let k = i - MA_DAYS + 1; k <= i; k++) sum += closes[k].close;
  return closes[i].close > sum / MA_DAYS;
}

/**
 * The strategy's value, as a share price, from `days` closes before close day `anchorDay` to the last close,
 * scaled to be 1 on `anchorDay`: what a share would have been worth around the fund's $1 start (display only,
 * simulated). points[days] is the anchor; later points continue the simulation next to the actual fund.
 */
export function simulatedSharePrice(closes: DailyClose[], anchorDay: number, days: number): Point[] | null {
  const anchor = closes.findIndex((c) => c.day === anchorDay);
  if (anchor < 0) return null;
  const r = backtest(closes, closes.length - 1 - anchor + days);
  if (!r) return null;
  const scale = r.points[days].strategy;
  return r.points.map((p) => ({ ...p, strategy: p.strategy / scale }));
}

function maxDrawdown(values: number[]): number {
  let peak = values[0];
  let worst = 0;
  for (const v of values) {
    peak = Math.max(peak, v);
    worst = Math.min(worst, v / peak - 1);
  }
  return worst;
}

/** The last `days` days. Returns null when there is not enough history for the average. */
export function backtest(closes: DailyClose[], days: number): Result | null {
  const start = closes.length - 1 - days;
  if (start < MA_DAYS - 1) return null;
  const cost = 1 - SWITCH_COST_BPS / 10_000;
  let position = aboveAverage(closes, start); // decided at the window's first close
  let value = 1;
  let switches = 0;
  let daysInBtc = 0;
  const points: Point[] = [{ day: closes[start].day, strategy: 1, hold: 1, inBtc: position }];
  for (let i = start + 1; i < closes.length; i++) {
    if (position) {
      value *= closes[i].close / closes[i - 1].close;
      daysInBtc++;
    }
    const held = position;
    const next = aboveAverage(closes, i);
    if (next !== position) {
      value *= cost;
      switches++;
      position = next;
    }
    points.push({ day: closes[i].day, strategy: value, hold: closes[i].close / closes[start].close, inBtc: held });
  }
  return {
    points,
    strategyReturn: value - 1,
    holdReturn: closes[closes.length - 1].close / closes[start].close - 1,
    strategyDrawdown: maxDrawdown(points.map((p) => p.strategy)),
    holdDrawdown: maxDrawdown(points.map((p) => p.hold)),
    switches,
    timeInBtc: daysInBtc / days,
  };
}
