import type { ProcessTrendData, TrendKind, TrendPoint, TrendRange, TrendSeries } from '../types/ProcessTrend';

export const trendKindLabels: Record<TrendKind, string> = {
  actual: '實際值', prediction: '預測', target: '目標', command: '指令', plan: '未來計畫',
  residual: '預測殘差（實際 − 預測）', movement: '每步命令變化 ΔMV',
  spec_low: '品質規格下限', spec_high: '品質規格上限', operating_low: '操作下限', operating_high: '操作上限',
  soft_low: '軟帶下緣', soft_high: '軟帶上緣', valid_low: '有效量程下限', valid_high: '有效量程上限',
};
export const isTrendBound = (kind: TrendKind) => kind.endsWith('_low') || kind.endsWith('_high');
export const finitePoint = (point: TrendPoint) => point.value !== null && Number.isFinite(point.value);
export function parseTrendDomain(low: string, high: string): [number, number] | null {
  if (!low.trim() || !high.trim()) return null;
  const a = Number(low), b = Number(high);
  return Number.isFinite(a) && Number.isFinite(b) && a < b ? [a, b] : null;
}
export function trendDomain(series: TrendSeries[], variableId: string, mode: 'detail' | 'all', range: TrendRange): [number, number] {
  let low = Infinity, high = -Infinity;
  for (const item of series) {
    if (!item.available || item.variableId !== variableId || item.kind === 'residual' || item.kind === 'movement' || (mode === 'detail' && isTrendBound(item.kind))) continue;
    for (const point of item.points) {
      if (point.time < range.start || point.time > range.end || !finitePoint(point) || point.quality === 'bad' || point.quality === 'missing') continue;
      low = Math.min(low, point.value!); high = Math.max(high, point.value!);
    }
  }
  if (!Number.isFinite(low)) return [0, 1]; // Empty axis only; no sample is created.
  const padding = high === low ? Math.max(Math.abs(low) * 0.05, 0.01) : (high - low) * 0.06;
  return [low - padding, high + padding];
}
export function nearestTrendTime(times: number[], time: number): number {
  if (!times.length) return time;
  let low = 0, high = times.length;
  while (low < high) { const middle = (low + high) >>> 1; if (times[middle] < time) low = middle + 1; else high = middle; }
  if (!low) return times[0];
  if (low === times.length) return times[low - 1];
  return time - times[low - 1] <= times[low] - time ? times[low - 1] : times[low];
}
export function pointAt(series: TrendSeries, time: number): TrendPoint | undefined {
  // Exact valid-time matching is deliberate: do not interpolate missing evidence.
  return series.points.find(point => point.time === time);
}
export function trendResidual(series: TrendSeries[], forecast: TrendSeries, point: TrendPoint): number | null {
  if (forecast.kind !== 'prediction' || point.quality !== 'good' || !finitePoint(point)) return null;
  const actuals = series.filter(item => item.available && item.kind === 'actual' && item.variableId === forecast.variableId && item.method === forecast.method);
  // A method with multiple variants must identify its own actual by the shared ID prefix.
  const prefix = forecast.id.replace(/prediction$/, '');
  const actual = actuals.find(item => item.id === `${prefix}actual`) ?? (actuals.length === 1 && actuals[0].source === forecast.source ? actuals[0] : undefined);
  const observed = actual && pointAt(actual, point.time);
  return observed?.quality === 'good' && finitePoint(observed) ? observed.value! - point.value! : null;
}
export function mergeTrendData(datasets: ProcessTrendData[]): ProcessTrendData | null {
  if (!datasets.length) return null;
  const first = datasets[0];
  return {
    ...first,
    variables: [...new Map(datasets.flatMap(data => data.variables).map(variable => [variable.id, variable])).values()],
    series: datasets.flatMap(data => data.series),
    groups: [...new Map(datasets.flatMap(data => data.groups ?? []).map(group => [group.id, group])).values()],
    overview: datasets.some(data => data.overview) ? {
      range: {
        start: Math.min(...datasets.flatMap(data => data.overview ? [data.overview.range.start] : [])),
        end: Math.max(...datasets.flatMap(data => data.overview ? [data.overview.range.end] : [])),
      },
      series: datasets.flatMap(data => data.overview?.series ?? []),
      splits: [...new Map(datasets.flatMap(data => data.overview?.splits ?? []).map(split => [`${split.name}:${split.start}:${split.end}`, split])).values()],
    } : undefined,
    windowSummary: datasets.every(data => data.windowSummary
      && data.windowSummary.range.start === first.range.start && data.windowSummary.range.end === first.range.end) ? {
        range: first.range,
        basis: [...new Set(datasets.map(data => data.windowSummary!.basis))].join(' · '),
        rows: datasets.flatMap(data => data.windowSummary!.rows),
      } : undefined,
    events: [...new Map(datasets.flatMap(data => data.events).map(event => [`${event.time}:${event.kind}:${event.variableId}:${event.groupId}:${event.label}`, event])).values()].sort((a, b) => a.time - b.time),
    range: { start: Math.min(...datasets.map(data => data.range.start)), end: Math.max(...datasets.map(data => data.range.end)) },
    rawSamples: datasets.reduce((sum, data) => sum + data.rawSamples, 0),
    displaySamples: datasets.reduce((sum, data) => sum + data.displaySamples, 0),
    warnings: [...new Set(datasets.flatMap(data => data.warnings))],
    provenance: [...new Set(datasets.map(data => data.provenance))].join(' · '),
  };
}
