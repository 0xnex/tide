// 折线图：一条或两条线共用一个纵轴，可选的背景带（比如持有 BTC 的日子）、竖直标记线，十字线和提示框（鼠标或方向键）。
// 一条线可以只覆盖一部分日期（其余是 null），比如模拟部分和实际部分各画一条。
// 颜色用 .viz-root 里的 --series-1 / --series-2（参考调色板第 1、2 位，浅色和深色都校验过）。
import { useEffect, useRef, useState, type ReactNode } from "react";

export interface Series {
  label: string;
  className: "s1" | "s2";
  values: (number | null)[]; // null：这一天没有这条线
  dashed?: boolean;
}

interface Props {
  days: number[]; // 每个点的日期序号（unix 秒 / 86400），横轴按点的顺序均匀排开
  series: Series[];
  bands?: boolean[]; // 第 i 个点之前那一段是否加背景带
  includeZero?: boolean; // 纵轴是否包含 0（收益率图要，价格图不要）
  format: (v: number) => string; // 提示框和直接标签里的数值
  tick: (v: number, step: number) => string; // 纵轴刻度
  directLabel?: (s: Series, last: number) => string; // 右端的直接标签（宽度够时才显示）
  ariaLabel: string;
  extra?: (i: number) => ReactNode; // 提示框里额外的一行
  marker?: { index: number; label: string }; // 在第 index 个点画一条竖线
}

const dateLabel = (day: number) =>
  new Date(day * 86_400_000).toLocaleDateString("en-US", { month: "short", day: "numeric", timeZone: "UTC" });
const fullDate = (day: number) =>
  new Date(day * 86_400_000).toLocaleDateString("en-US", { year: "numeric", month: "short", day: "numeric", timeZone: "UTC" });

/** 刻度间隔取 1、2、5 × 10^k */
function niceStep(range: number, count: number): number {
  const raw = range / count;
  const mag = 10 ** Math.floor(Math.log10(raw));
  const n = raw / mag;
  return (n <= 1 ? 1 : n <= 2 ? 2 : n <= 5 ? 5 : 10) * mag;
}

function useWidth() {
  const ref = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(640);
  useEffect(() => {
    if (!ref.current) return;
    const ro = new ResizeObserver(([e]) => setWidth(Math.max(260, Math.floor(e.contentRect.width))));
    ro.observe(ref.current);
    return () => ro.disconnect();
  }, []);
  return { ref, width };
}

/** 最后一个有值的点 */
function lastIndex(values: (number | null)[]): number {
  for (let i = values.length - 1; i >= 0; i--) if (values[i] !== null) return i;
  return -1;
}

export function LineChart({ days, series, bands, includeZero, format, tick, directLabel, ariaLabel, extra, marker }: Props) {
  const { ref, width } = useWidth();
  const [hover, setHover] = useState<number | null>(null);
  const n = days.length;
  const height = 260;
  // 窄屏不放右侧的直接标签（上面有图例或标题），省出宽度给曲线
  const showDirect = !!directLabel && width >= 520;
  const m = { top: 14, right: showDirect ? 128 : 12, bottom: 28, left: 56 };
  const w = width - m.left - m.right;
  const h = height - m.top - m.bottom;

  const all = series.flatMap((s) => s.values).filter((v): v is number => v !== null);
  let lo = Math.min(...all, ...(includeZero ? [0] : []));
  let hi = Math.max(...all, ...(includeZero ? [0] : []));
  if (hi - lo < 1e-12) {
    lo -= Math.abs(lo) * 0.01 || 0.01;
    hi += Math.abs(hi) * 0.01 || 0.01;
  }
  const step = niceStep(hi - lo, 4);
  lo = Math.floor(lo / step) * step;
  hi = Math.ceil(hi / step) * step;
  const x = (i: number) => m.left + (n === 1 ? w / 2 : (i / (n - 1)) * w);
  const y = (v: number) => m.top + (1 - (v - lo) / (hi - lo)) * h;
  const yTicks: number[] = [];
  for (let v = lo; v <= hi + step / 2; v += step) yTicks.push(v);
  const xTicks = n === 1 ? [0] : Array.from(new Set(Array.from({ length: 5 }, (_, k) => Math.round((k * (n - 1)) / 4))));

  // 遇到 null 断开，下一个有值的点重新起笔
  const path = (values: (number | null)[]) =>
    values
      .map((v, i) => (v === null ? "" : `${i && values[i - 1] !== null ? "L" : "M"}${x(i).toFixed(1)},${y(v).toFixed(1)}`))
      .join("");
  const ends = series.map((s) => lastIndex(s.values));

  const runs: [number, number][] = [];
  if (bands) {
    for (let i = 1; i < n; i++) {
      if (!bands[i]) continue;
      const last = runs[runs.length - 1];
      if (last && last[1] === i - 1) last[1] = i;
      else runs.push([i - 1, i]);
    }
  }

  // 两个直接标签太近时上下推开
  const labelY = series.map((s, k) => y(s.values[ends[k]] ?? 0));
  if (labelY.length === 2 && Math.abs(labelY[0] - labelY[1]) < 16) {
    const mid = (labelY[0] + labelY[1]) / 2;
    const up = labelY[0] <= labelY[1] ? -8 : 8;
    labelY[0] = mid + up;
    labelY[1] = mid - up;
  }

  const onMove = (clientX: number) => {
    const rect = ref.current!.getBoundingClientRect();
    const i = n === 1 ? 0 : Math.round(((clientX - rect.left - m.left) / w) * (n - 1));
    setHover(Math.max(0, Math.min(n - 1, i)));
  };
  // 后画的线在上面：第一条（主线）最后画
  const drawOrder = [...series].reverse();

  return (
    <div className="chart" ref={ref}>
      <svg
        width={width}
        height={height}
        role="img"
        aria-label={ariaLabel}
        tabIndex={0}
        onPointerMove={(e) => onMove(e.clientX)}
        onPointerLeave={() => setHover(null)}
        onKeyDown={(e) => {
          if (e.key === "ArrowLeft") setHover((v) => Math.max(0, (v ?? n - 1) - 1));
          if (e.key === "ArrowRight") setHover((v) => Math.min(n - 1, (v ?? 0) + 1));
          if (e.key === "Escape") setHover(null);
        }}
        onBlur={() => setHover(null)}
      >
        {runs.map(([a, b]) => (
          <rect key={a} className="band" x={x(a)} y={m.top} width={Math.max(1, x(b) - x(a))} height={h} />
        ))}
        {yTicks.map((v) => (
          <g key={v}>
            <line
              className={includeZero && Math.abs(v) < 1e-9 ? "zero" : "grid"}
              x1={m.left}
              x2={m.left + w}
              y1={y(v)}
              y2={y(v)}
            />
            <text className="tick" x={m.left - 8} y={y(v)} textAnchor="end" dominantBaseline="middle">
              {tick(v, step)}
            </text>
          </g>
        ))}
        {xTicks.map((i, k) => (
          <text
            key={k}
            className="tick"
            x={x(i)}
            y={height - 8}
            textAnchor={n === 1 ? "middle" : k === 0 ? "start" : k === xTicks.length - 1 ? "end" : "middle"}
          >
            {dateLabel(days[i])}
          </text>
        ))}
        {marker && (
          <g>
            <line className="marker" x1={x(marker.index)} x2={x(marker.index)} y1={m.top} y2={m.top + h} />
            <text className="tick" x={x(marker.index) - 4} y={m.top + 10} textAnchor="end">
              {marker.label}
            </text>
          </g>
        )}
        {drawOrder.map((s) => (
          <path key={s.label} className={`series ${s.className}${s.dashed ? " dashed" : ""}`} d={path(s.values)} />
        ))}
        {series.map(
          (s, k) =>
            ends[k] >= 0 && <circle key={s.label} className={`end ${s.className}`} cx={x(ends[k])} cy={y(s.values[ends[k]]!)} r={4} />,
        )}
        {showDirect &&
          series.map(
            (s, k) =>
              ends[k] >= 0 && (
                <text key={s.label} className="direct" x={x(ends[k]) + 10} y={labelY[k]} dominantBaseline="middle">
                  {directLabel!(s, s.values[ends[k]]!)}
                </text>
              ),
          )}
        {hover !== null && (
          <g>
            <line className="crosshair" x1={x(hover)} x2={x(hover)} y1={m.top} y2={m.top + h} />
            {drawOrder.map((s) => {
              const v = s.values[hover];
              return v !== null && <circle key={s.label} className={`dot ${s.className}`} cx={x(hover)} cy={y(v)} r={4} />;
            })}
          </g>
        )}
      </svg>
      {hover !== null && (
        <div className="tooltip" style={{ left: Math.min(Math.max(x(hover) + 12, 0), width - 190), top: m.top }} role="status">
          <div className="tooltip-date">{fullDate(days[hover])}</div>
          {series.map((s) => {
            const v = s.values[hover];
            return (
              v !== null && (
                <div key={s.label} className="tooltip-row">
                  <span className={`swatch line ${s.className}${s.dashed ? " dashed" : ""}`} />
                  <strong>{format(v)}</strong>
                  <span className="muted">{s.label}</span>
                </div>
              )
            );
          })}
          {extra && <div className="tooltip-row muted">{extra(hover)}</div>}
        </div>
      )}
    </div>
  );
}
