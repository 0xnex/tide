import { useEffect, useMemo, useRef, useState } from "react";
import {
  MA_DAYS,
  SWITCH_COST_BPS,
  WINDOWS,
  backtest,
  fetchDailyCloses,
  type DailyClose,
  type Result,
  type WindowKey,
} from "./backtest";

const pct = (x: number, digits = 1) => `${x >= 0 ? "+" : ""}${(x * 100).toFixed(digits)}%`;
const dateLabel = (day: number) =>
  new Date(day * 86_400_000).toLocaleDateString("en-US", { month: "short", day: "numeric", timeZone: "UTC" });
const fullDate = (day: number) =>
  new Date(day * 86_400_000).toLocaleDateString("en-US", { year: "numeric", month: "short", day: "numeric", timeZone: "UTC" });

export function Backtest() {
  const [closes, setCloses] = useState<DailyClose[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [key, setKey] = useState<WindowKey>("1Y");

  useEffect(() => {
    fetchDailyCloses()
      .then(setCloses)
      .catch((e) => setError(String(e?.message ?? e)));
  }, []);

  const results = useMemo(() => {
    if (!closes) return null;
    return Object.fromEntries(WINDOWS.map((w) => [w.key, backtest(closes, w.days)])) as Record<WindowKey, Result | null>;
  }, [closes]);
  const result = results?.[key] ?? null;

  return (
    <section className="card viz-root">
      <div className="card-head">
        <div>
          <h2>Backtest</h2>
          <p className="muted small">The fund's rule on daily BTC/USD closes, compared with holding BTC.</p>
        </div>
        <div className="segmented" role="tablist" aria-label="Time range">
          {WINDOWS.map((w) => (
            <button
              key={w.key}
              role="tab"
              aria-selected={w.key === key}
              className={w.key === key ? "seg active" : "seg"}
              onClick={() => setKey(w.key)}
            >
              {w.key}
            </button>
          ))}
        </div>
      </div>

      {error && <p className="muted">Could not load price history: {error}</p>}
      {!error && !result && <p className="muted">Loading price history…</p>}
      {result && (
        <>
          <div className="stats five">
            <Stat label="Strategy" value={pct(result.strategyReturn)} tone={result.strategyReturn >= 0 ? "up" : "down"} />
            <Stat label="Buy & hold BTC" value={pct(result.holdReturn)} tone={result.holdReturn >= 0 ? "up" : "down"} />
            <Stat label="Max drawdown" value={`${pct(result.strategyDrawdown)} vs ${pct(result.holdDrawdown)}`} small />
            <Stat label="Switches" value={String(result.switches)} />
            <Stat label="Time in BTC" value={`${Math.round(result.timeInBtc * 100)}%`} />
          </div>
          <Chart result={result} />
          <SummaryTable results={results!} />
          <p className="muted small note">
            Each day's close stands in for the 01:00 UTC fixing. Above the {MA_DAYS}-day average the strategy holds BTC for
            the next day, otherwise USDC. Each switch costs {SWITCH_COST_BPS / 100}% for fees and slippage; the 1%
            withdrawal fee is not included. Data: Coinbase BTC-USD daily candles. Past results do not predict future
            returns.
          </p>
        </>
      )}
    </section>
  );
}

function Stat({ label, value, tone, small }: { label: string; value: string; tone?: "up" | "down"; small?: boolean }) {
  return (
    <div className="stat">
      <div className="stat-label">{label}</div>
      <div className={`stat-value ${small ? "stat-small" : ""} ${tone ?? ""}`}>{value}</div>
    </div>
  );
}

function SummaryTable({ results }: { results: Record<WindowKey, Result | null> }) {
  return (
    <div className="table-wrap summary">
      <table>
        <thead>
          <tr>
            <th>Period</th>
            <th className="num">Strategy</th>
            <th className="num">Buy & hold</th>
            <th className="num">Max drawdown (strategy / hold)</th>
            <th className="num">Switches</th>
            <th className="num">Time in BTC</th>
          </tr>
        </thead>
        <tbody>
          {WINDOWS.map((w) => {
            const r = results[w.key];
            if (!r) return null;
            return (
              <tr key={w.key}>
                <td>{w.key}</td>
                <td className="num">{pct(r.strategyReturn)}</td>
                <td className="num">{pct(r.holdReturn)}</td>
                <td className="num">
                  {pct(r.strategyDrawdown)} / {pct(r.holdDrawdown)}
                </td>
                <td className="num">{r.switches}</td>
                <td className="num">{Math.round(r.timeInBtc * 100)}%</td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

/** "Nice" tick step for a range split into about `count` parts: 1, 2 or 5 × 10^k */
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
    const ro = new ResizeObserver(([e]) => setWidth(Math.max(280, Math.floor(e.contentRect.width))));
    ro.observe(ref.current);
    return () => ro.disconnect();
  }, []);
  return { ref, width };
}

function Chart({ result }: { result: Result }) {
  const { ref, width } = useWidth();
  const [hover, setHover] = useState<number | null>(null);
  const pts = result.points;
  const n = pts.length;
  const height = 280;
  // 窄屏不放右侧的直接标签（上面有图例），省出宽度给曲线
  const showDirect = width >= 520;
  const m = { top: 14, right: showDirect ? 128 : 12, bottom: 28, left: 48 };
  const w = width - m.left - m.right;
  const h = height - m.top - m.bottom;

  // one y axis: return in % for both series
  const values = pts.flatMap((p) => [p.strategy - 1, p.hold - 1]);
  let lo = Math.min(0, ...values);
  let hi = Math.max(0, ...values);
  const step = niceStep(hi - lo || 0.01, 4);
  lo = Math.floor(lo / step) * step;
  hi = Math.ceil(hi / step) * step;
  const x = (i: number) => m.left + (n === 1 ? 0 : (i / (n - 1)) * w);
  const y = (v: number) => m.top + (1 - (v - lo) / (hi - lo)) * h;
  const yTicks: number[] = [];
  for (let v = lo; v <= hi + step / 2; v += step) yTicks.push(v);
  const xTicks = Array.from({ length: 5 }, (_, k) => Math.round((k * (n - 1)) / 4));

  const line = (key: "strategy" | "hold") =>
    pts.map((p, i) => `${i ? "L" : "M"}${x(i).toFixed(1)},${y(p[key] - 1).toFixed(1)}`).join("");

  // shaded bands: days the strategy held BTC (the move from point i-1 to i)
  const bands: [number, number][] = [];
  for (let i = 1; i < n; i++) {
    if (!pts[i].inBtc) continue;
    const last = bands[bands.length - 1];
    if (last && last[1] === i - 1) last[1] = i;
    else bands.push([i - 1, i]);
  }

  // direct labels at the right end, pushed apart when they would overlap
  const end = pts[n - 1];
  let ys = y(end.strategy - 1);
  let yh = y(end.hold - 1);
  if (Math.abs(ys - yh) < 16) {
    const mid = (ys + yh) / 2;
    const up = ys <= yh ? -8 : 8;
    ys = mid + up;
    yh = mid - up;
  }

  const hp = hover === null ? null : pts[hover];
  const onMove = (clientX: number) => {
    const rect = ref.current!.getBoundingClientRect();
    const i = Math.round(((clientX - rect.left - m.left) / w) * (n - 1));
    setHover(Math.max(0, Math.min(n - 1, i)));
  };

  return (
    <div className="chart" ref={ref}>
      <div className="legend">
        <span className="legend-item">
          <span className="swatch line s1" /> Strategy
        </span>
        <span className="legend-item">
          <span className="swatch line s2" /> Buy & hold BTC
        </span>
        <span className="legend-item">
          <span className="swatch band" /> Holding BTC
        </span>
      </div>
      <svg
        width={width}
        height={height}
        role="img"
        aria-label={`Return of the strategy (${pct(result.strategyReturn)}) and of holding BTC (${pct(result.holdReturn)}) over the period`}
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
        {bands.map(([a, b]) => (
          <rect key={a} className="band" x={x(a)} y={m.top} width={Math.max(1, x(b) - x(a))} height={h} />
        ))}
        {yTicks.map((v) => (
          <g key={v}>
            <line className={Math.abs(v) < 1e-9 ? "zero" : "grid"} x1={m.left} x2={m.left + w} y1={y(v)} y2={y(v)} />
            <text className="tick" x={m.left - 8} y={y(v)} textAnchor="end" dominantBaseline="middle">
              {Math.abs(v) < 1e-9 ? "0%" : pct(v, Math.abs(step) < 0.01 ? 1 : 0)}
            </text>
          </g>
        ))}
        {xTicks.map((i, k) => (
          <text
            key={k}
            className="tick"
            x={x(i)}
            y={height - 8}
            textAnchor={k === 0 ? "start" : k === xTicks.length - 1 ? "end" : "middle"}
          >
            {dateLabel(pts[i].day)}
          </text>
        ))}
        <path className="series s2" d={line("hold")} />
        <path className="series s1" d={line("strategy")} />
        <circle className="end s2" cx={x(n - 1)} cy={y(end.hold - 1)} r={4} />
        <circle className="end s1" cx={x(n - 1)} cy={y(end.strategy - 1)} r={4} />
        {showDirect && (
          <>
            <text className="direct" x={x(n - 1) + 10} y={ys} dominantBaseline="middle">
              Strategy {pct(end.strategy - 1)}
            </text>
            <text className="direct" x={x(n - 1) + 10} y={yh} dominantBaseline="middle">
              Hold {pct(end.hold - 1)}
            </text>
          </>
        )}
        {hp && hover !== null && (
          <g>
            <line className="crosshair" x1={x(hover)} x2={x(hover)} y1={m.top} y2={m.top + h} />
            <circle className="dot s2" cx={x(hover)} cy={y(hp.hold - 1)} r={4} />
            <circle className="dot s1" cx={x(hover)} cy={y(hp.strategy - 1)} r={4} />
          </g>
        )}
      </svg>
      {hp && hover !== null && (
        <div
          className="tooltip"
          style={{ left: Math.min(Math.max(x(hover) + 12, 0), width - 190), top: m.top }}
          role="status"
        >
          <div className="tooltip-date">{fullDate(hp.day)}</div>
          <div className="tooltip-row">
            <span className="swatch line s1" />
            <strong>{pct(hp.strategy - 1)}</strong>
            <span className="muted">Strategy</span>
          </div>
          <div className="tooltip-row">
            <span className="swatch line s2" />
            <strong>{pct(hp.hold - 1)}</strong>
            <span className="muted">Buy & hold</span>
          </div>
          <div className="tooltip-row muted">Holding {hp.inBtc ? "BTC" : "USDC"}</div>
        </div>
      )}
    </div>
  );
}
