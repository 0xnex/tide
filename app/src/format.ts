// 数字和时间的显示。链上单位：USDC 和份额 6 位小数，cbBTC 8 位小数，价格是微美元。
import { BN } from "@coral-xyz/anchor";

type Num = BN | bigint | number;
const toBig = (x: Num) => (typeof x === "bigint" ? x : BigInt(x.toString()));

/** 定点整数转成带千分位的字符串，最多保留 digits 位小数 */
export function fixed(x: Num, decimals: number, digits = 2): string {
  const v = Number(toBig(x)) / 10 ** decimals;
  return v.toLocaleString("en-US", { minimumFractionDigits: 0, maximumFractionDigits: digits });
}

export const usdc = (x: Num) => fixed(x, 6, 2);
export const shares = (x: Num) => fixed(x, 6, 4);
export const btc = (x: Num) => fixed(x, 8, 6);
export const price = (x: Num) => `$${fixed(x, 6, 0)}`;

/** 用户输入的小数转成链上整数（向下取整）；无效时返回 null */
export function parseAmount(text: string, decimals: number): bigint | null {
  const t = text.trim();
  if (!/^\d+(\.\d*)?$/.test(t)) return null;
  const [whole, frac = ""] = t.split(".");
  return BigInt(whole + frac.slice(0, decimals).padEnd(decimals, "0"));
}

/** 日期序号（unix 秒 / 86400）显示成 UTC 日期 */
export const dayLabel = (day: number) => new Date(day * 86_400_000).toISOString().slice(0, 10);

/** unix 秒显示成 UTC 时间 */
export const timeLabel = (sec: number) => new Date(sec * 1000).toISOString().slice(0, 16).replace("T", " ") + " UTC";

/** 剩余时间，比如 3h 12m */
export function countdown(sec: number): string {
  if (sec <= 0) return "now";
  const h = Math.floor(sec / 3600);
  const m = Math.floor((sec % 3600) / 60);
  return h > 0 ? `${h}h ${m}m` : `${m}m ${sec % 60}s`;
}

export const short = (addr: string) => `${addr.slice(0, 4)}…${addr.slice(-4)}`;
export const explorer = (sig: string) => `https://explorer.solana.com/tx/${sig}?cluster=devnet`;
export const explorerAddress = (addr: string) => `https://explorer.solana.com/address/${addr}?cluster=devnet`;
