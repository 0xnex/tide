// 指标旁边的小图标（24×24 线条，颜色跟随文字）
const paths = {
  tag: "M3 12V4h8l10 10-8 8L3 12zM7.5 8.5h.01",
  wallet: "M3 7h15a3 3 0 0 1 3 3v8H3V7zm0 0l13-3v3M16 13h2",
  btc: "M8 5v14M11 5v2m0 10v2M7 7h6.5a2.5 2.5 0 0 1 0 5H7m0 0h7.5a2.5 2.5 0 0 1 0 5H7",
  usd: "M12 3v18M17 7.5C16 6 14.3 5.5 12 5.5c-2.8 0-4.5 1.3-4.5 3.2 0 4.6 9.5 2.2 9.5 6.8 0 2-1.8 3.2-5 3.2-2.4 0-4.2-.7-5-2.2",
  target: "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18zm0-5a4 4 0 1 0 0-8 4 4 0 0 0 0 8zm0-3.5h.01",
  clock: "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18zm0-13v4l3 2",
  sync: "M20 11a8 8 0 0 0-14.5-4M4 4v3h3M4 13a8 8 0 0 0 14.5 4M20 20v-3h-3",
  gauge: "M4 18a8 8 0 1 1 16 0M12 18l4-6",
  shares: "M12 3l9 5-9 5-9-5 9-5zm-9 9l9 5 9-5M3 16l9 5 9-5",
} as const;

export type IconName = keyof typeof paths;

export function Icon({ name }: { name: IconName }) {
  return (
    <svg className="icon" width="15" height="15" viewBox="0 0 24 24" aria-hidden="true">
      <path d={paths[name]} fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

/** Tide 的标志：蓝色圆底上两道波浪，和 public/favicon.svg、份额代币的图标（public/share.png）是同一个图形 */
export function Logo({ size }: { size: number }) {
  return (
    <svg className="logo" width={size} height={size} viewBox="0 0 512 512" aria-hidden="true">
      <circle cx="256" cy="256" r="256" fill="#2470ff" />
      <g transform="translate(60.4 60.2) scale(17.8)" fill="none" stroke="#fff" strokeWidth="2.6" strokeLinecap="round">
        <path d="M2 8c3-4 6-4 9 0s6 4 9 0" opacity=".5" />
        <path d="M2 14c3-4 6-4 9 0s6 4 9 0" />
      </g>
    </svg>
  );
}
