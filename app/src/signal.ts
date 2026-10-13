// 下一次定价的买入门槛，只用来显示。真正的判断在 fund-logic 的 fix 里。
import { BN } from "@coral-xyz/anchor";
import { DAY, type Fund } from "./chain";

const MAX_WINDOW = 64; // 和 fund_logic::MAX_WINDOW 一样：均线最多往回看这么多天

/** 下一次定价时刻（unix 秒） */
export function nextFixTime(fund: Fund, now: number): number {
  const offset = fund.config.settleOffsetSecs.toNumber();
  const today = Math.floor(now / DAY);
  return now < today * DAY + offset ? today * DAY + offset : (today + 1) * DAY + offset;
}

/**
 * 第 day 天定价时，目标变成 cbBTC 需要的信号价格（微美元，向下取整，价格要严格高于它）；价格不够算均线时为 null。
 * 均线包含当天的价格 p：目标是 cbBTC 当且仅当 p > floor((S + p) / w)，S 是之前最近 w - 1 个价格之和。
 * 对整数 p，这等价于 S + p < w·p，也就是 p > S / (w - 1)。
 */
export function buyThreshold(fund: Fund, day: number): bigint | null {
  const w = fund.config.window;
  const s = fund.state;
  if (w < 2) return null; // w = 1 时均线就是 p 本身，p > p 永远不成立
  const before = (s.prices as BN[])
    .map((p, i) => ({ p: BigInt(p.toString()), d: (s.priceDays as BN[])[i].toNumber() }))
    .filter(({ p, d }) => p > 0n && d < day && d > day - MAX_WINDOW)
    .sort((a, b) => b.d - a.d)
    .slice(0, w - 1);
  if (before.length < w - 1) return null;
  const sum = before.reduce((x, { p }) => x + p, 0n);
  return sum / BigInt(w - 1);
}
