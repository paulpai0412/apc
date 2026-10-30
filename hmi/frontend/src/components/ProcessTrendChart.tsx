import { useCallback, useEffect, useMemo, useState } from 'react';
import { Area, CartesianGrid, ComposedChart, Line, ReferenceArea, ReferenceLine, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts';
import type { ProcessTrendData, TrendGroup, TrendKind, TrendRange, TrendSeries, TrendVariable } from '../types/ProcessTrend';
import { finitePoint, isTrendBound, nearestTrendTime, parseTrendDomain, trendDomain, trendKindLabels } from '../lib/processTrend';
import { trendRoleBlocks, trendRoleGeometry } from '../lib/processTrendLayout';
import { ProcessTrendOverview, ProcessTrendSummary } from './ProcessTrendOverview';

export type ProcessTrendChartProps = {
  data: ProcessTrendData; storageKey?: string; initialSelected?: string[]; initialView?: 'compare' | 'lanes' | 'overlay' | 'role_blocks';
  initialDiagnostic?: boolean; loading?: boolean;
  onWindowChange?: (range: TrendRange) => void; fullRange?: TrendRange; anchorTime?: number; onCursorChange?: (time: number) => void;
  onVariableFocus?: (variable: TrendVariable) => void; focusedVariableId?: string | null;
  cursorTime?: number | null; onCursorTimeChange?: (time: number | null) => void; currentTime?: number;
};
type View = NonNullable<ProcessTrendChartProps['initialView']>;
type AxisSetting = { mode: 'detail' | 'all' | 'manual'; locked?: [number, number]; manual?: [number, number] };
type Preferences = { selected?: string[]; variables?: string[]; view?: View; hidden?: TrendKind[]; axes?: Record<string, AxisSetting>; lines?: Record<string, boolean>; diagnosticLines?: Record<string, boolean>; unlocked?: string[]; bands?: string[] };
const colors = ['#38bdf8', '#fbbf24', '#c4b5fd', '#34d399', '#fb923c', '#e879f9'];
const lineDashes: Record<TrendKind, string | undefined> = {
  actual: undefined, prediction: '7 4', command: '2 3', plan: '8 3 2 3', target: '6 4', residual: undefined, movement: undefined,
  spec_low: '3 3', spec_high: '3 3', operating_low: '1 5', operating_high: '1 5',
  soft_low: '8 3 2 3', soft_high: '8 3 2 3', valid_low: '1 2', valid_high: '1 2',
};
const bandKinds = [
  { id: 'spec', label: '品質規格帶', color: '#34d399' }, { id: 'operating', label: '操作限制帶', color: '#fbbf24' },
  { id: 'soft', label: '軟帶', color: '#c4b5fd' }, { id: 'valid', label: '有效量程', color: '#94a3b8' },
];
const control = 'min-w-0 max-w-full rounded border border-slate-600 bg-slate-900 px-2 py-1.5 text-sm text-slate-100';
const button = `${control} disabled:opacity-40 focus-visible:outline-2 focus-visible:outline-sky-400`;
const qualityLabels = { good: '良好', bad: '不良', uncertain: '不確定', unknown: '品質未知', missing: '缺值' };
const plotQualities = ['good', 'uncertain', 'unknown', 'bad'] as const;
const valueLabel = (value: number) => value.toLocaleString('zh-TW', { maximumSignificantDigits: 6 });
const groupId = (series: TrendSeries) => series.groupId ?? 'live';
function loadPreferences(key?: string): Preferences {
  if (!key) return {};
  try {
    const value = JSON.parse(localStorage.getItem(key) || '{}');
    if (!value || typeof value !== 'object') return {};
    const strings = (item: unknown): string[] | undefined => Array.isArray(item) && item.every(id => typeof id === 'string') ? item : undefined;
    return {
      selected: strings(value.selected), variables: strings(value.variables), unlocked: strings(value.unlocked), bands: strings(value.bands),
      hidden: Array.isArray(value.hidden) ? value.hidden.filter((kind: string) => kind in trendKindLabels) : undefined,
      lines: value.lines && typeof value.lines === 'object' && !Array.isArray(value.lines) ? Object.fromEntries(Object.entries(value.lines).filter(([, enabled]) => typeof enabled === 'boolean')) as Record<string, boolean> : undefined,
      diagnosticLines: value.diagnosticLines && typeof value.diagnosticLines === 'object' && !Array.isArray(value.diagnosticLines) ? Object.fromEntries(Object.entries(value.diagnosticLines).filter(([, enabled]) => typeof enabled === 'boolean')) as Record<string, boolean> : undefined,
      view: value.presentationVersion === 2 && ['compare', 'lanes', 'overlay', 'role_blocks'].includes(value.view) ? value.view : value.view === 'overlay' ? 'overlay' : undefined,
      axes: value.axes && typeof value.axes === 'object' ? Object.fromEntries(Object.entries(value.axes).flatMap(([id, raw]) => {
        if (!raw || typeof raw !== 'object') return [];
        const item = raw as AxisSetting;
        const pair = (values?: [number, number]) => Array.isArray(values) && values.length === 2 && values.every(Number.isFinite) && values[0] < values[1] ? values : undefined;
        const manual = pair(item.manual), locked = pair(item.locked);
        if (!['detail', 'all', 'manual'].includes(item.mode) || (item.mode === 'manual' && !manual)) return [];
        return [[id, { mode: item.mode, manual, locked }]];
      })) : undefined,
    };
  } catch { return {}; }
}
function AxisControls({ name, setting, domain, onChange }: { name: string; setting: AxisSetting; domain: [number, number]; onChange: (next: AxisSetting) => void }) {
  const [low, setLow] = useState('');
  const [high, setHigh] = useState('');
  const [error, setError] = useState('');
  return <div className="space-y-2">
    <div className="flex flex-wrap items-center gap-2">
      <label className="flex min-w-0 flex-wrap items-center gap-2 text-xs">{name} 範圍<select aria-label={`${name} 軸範圍`} className={control} value={setting.mode} onChange={event => {
        const mode = event.target.value as AxisSetting['mode'];
        if (mode === 'manual') { const next = setting.manual ?? domain; setLow(String(next[0])); setHigh(String(next[1])); }
        onChange({ ...setting, mode, manual: mode === 'manual' ? setting.manual ?? domain : setting.manual, locked: undefined });
      }}><option value="detail">訊號與目標</option><option value="all">包含各限制</option><option value="manual">手動</option></select></label>
      <button type="button" className={button} aria-label={`${name} ${setting.locked ? '解除鎖軸' : '鎖定此軸'}`} aria-pressed={!!setting.locked} onClick={() => onChange({ ...setting, locked: setting.locked ? undefined : domain })}>{setting.locked ? '解除鎖軸' : '鎖定此軸'}</button>
      <span className="text-xs text-slate-400">{valueLabel(domain[0])} → {valueLabel(domain[1])}</span>
    </div>
    {setting.mode === 'manual' && <form className="flex flex-wrap items-end gap-2" onSubmit={event => {
      event.preventDefault(); const next = parseTrendDomain(low, high);
      if (!next) { setError('上下界須為有限數值，且下界小於上界。'); return; }
      setError(''); onChange({ mode: 'manual', manual: next, locked: setting.locked ? next : undefined });
    }}>
      <label className="text-xs">下界<input aria-label={`${name} 手動下界`} className={`${control} ml-2 w-28`} type="number" step="any" value={low} placeholder={String(domain[0])} onChange={event => setLow(event.target.value)} /></label>
      <label className="text-xs">上界<input aria-label={`${name} 手動上界`} className={`${control} ml-2 w-28`} type="number" step="any" value={high} placeholder={String(domain[1])} onChange={event => setHigh(event.target.value)} /></label>
      <button type="submit" className={button}>套用此軸</button>{error && <p role="alert" className="w-full text-xs text-rose-300">{error}</p>}
    </form>}
  </div>;
}

export function ProcessTrendChart({ data, storageKey, initialSelected, initialView = 'compare', initialDiagnostic = false, loading = false, onWindowChange, fullRange, anchorTime, onCursorChange, onVariableFocus, focusedVariableId, cursorTime: controlledCursor, onCursorTimeChange, currentTime = data.currentTime }: ProcessTrendChartProps) {
  const [preferences] = useState(() => loadPreferences(storageKey));
  const [selection, setSelection] = useState<string[] | null>(preferences.selected ?? null);
  const [inventory, setInventory] = useState(() => preferences.variables ?? data.variables.map(variable => variable.id));
  const selected = useMemo(() => {
    const chosen = selection === null ? initialSelected ?? data.variables.map(variable => variable.id)
      : data.variables.filter(variable => selection.includes(variable.id) || !inventory.includes(variable.id)).map(variable => variable.id);
    return focusedVariableId && data.variables.some(variable => variable.id === focusedVariableId) ? [...new Set([...chosen, focusedVariableId])] : chosen;
  }, [selection, inventory, initialSelected, data.variables, focusedVariableId]);
  const [view, setView] = useState<View>(preferences.view ?? initialView);
  const [diagnostic, setDiagnostic] = useState(initialDiagnostic);
  const [processLines, setProcessLines] = useState<Record<string, boolean>>(preferences.lines ?? {});
  const [diagnosticLines, setDiagnosticLines] = useState<Record<string, boolean>>(preferences.diagnosticLines ?? {});
  const lines = diagnostic ? diagnosticLines : processLines;
  const setLines = diagnostic ? setDiagnosticLines : setProcessLines;
  const [axes, setAxes] = useState<Record<string, AxisSetting>>(preferences.axes ?? {});
  const [unlocked, setUnlocked] = useState<string[]>(preferences.unlocked ?? []);
  const [bands, setBands] = useState<string[]>(preferences.bands ?? ['spec']);
  const [windowRange, setWindowRange] = useState<TrendRange>(data.range);
  const [cursor, setCursor] = useState<number | null>(anchorTime ?? currentTime ?? data.range.end);
  const [rangeError, setRangeError] = useState('');
  const [startDraft, setStartDraft] = useState('');
  const [endDraft, setEndDraft] = useState('');
  const [eventKind, setEventKind] = useState('all');
  const [eventGroup, setEventGroup] = useState('all');
  const [eventPage, setEventPage] = useState(0);
  const [mobile, setMobile] = useState(() => typeof window !== 'undefined' && window.matchMedia('(max-width: 767px)').matches);
  useEffect(() => {
    const query = window.matchMedia('(max-width: 767px)');
    const change = () => setMobile(query.matches);
    query.addEventListener('change', change); return () => query.removeEventListener('change', change);
  }, []);
  useEffect(() => {
    if (loading) return;
    setWindowRange(data.range);
    setStartDraft(String((data.range.start - data.originTime) / 1000));
    setEndDraft(String((data.range.end - data.originTime) / 1000));
  }, [data.range.start, data.range.end, data.originTime, loading]);
  useEffect(() => {
    if (!storageKey) return;
    try { localStorage.setItem(storageKey, JSON.stringify({ presentationVersion: 2, selected, variables: [...new Set([...inventory, ...data.variables.map(variable => variable.id)])], view, lines: processLines, diagnosticLines, hidden: preferences.hidden, axes, unlocked, bands })); } catch { /* Display preferences are optional. */ }
  }, [storageKey, selected, inventory, data.variables, view, processLines, diagnosticLines, preferences.hidden, axes, unlocked, bands]);
  const range = windowRange;
  const completeRange = fullRange ?? data.overview?.range ?? data.range;
  const groups = useMemo<TrendGroup[]>(() => {
    const known = new Map((data.groups ?? []).map(group => [group.id, group]));
    for (const series of data.series) if (!known.has(groupId(series))) known.set(groupId(series), { id: groupId(series), label: series.groupId ?? '即時／歷史' });
    return [...known.values()];
  }, [data.groups, data.series]);
  const groupLabel = (id: string) => groups.find(group => group.id === id)?.label ?? id;
  const variables = useMemo(() => data.variables.filter(variable => selected.includes(variable.id)), [data.variables, selected]);
  const roleBlocks = useMemo(() => trendRoleBlocks(data.variables, selected, mobile), [data.variables, selected, mobile]);
  const defaultLine = useCallback((series: TrendSeries) => diagnostic
    ? ['actual', 'prediction', 'residual', 'command', 'plan', 'movement'].includes(series.kind)
    : !preferences.hidden?.includes(series.kind) && (series.forecastScope === 'latest' || ['actual', 'target', 'command', 'movement'].includes(series.kind)), [diagnostic, preferences.hidden]);
  const enabled = useCallback((series: TrendSeries) => lines[series.id] ?? defaultLine(series), [lines, defaultLine]);
  const toggleLines = useCallback((items: TrendSeries[], choice: boolean | 'default') => setLines(current => ({ ...current, ...Object.fromEntries(items.map(series => [series.id, choice === 'default' ? defaultLine(series) : choice])) })), [defaultLine, setLines]);
  const timeLabel = (time: number, compact = false) => {
    if (data.timeBasis === 'relative') {
      const elapsed = time - data.originTime;
      if (compact) {
        const span = range.end - range.start;
        const [scale, unit] = span >= 172800000 ? [86400000, '天'] : span >= 7200000 ? [3600000, '時'] : span >= 120000 ? [60000, '分'] : [1000, '秒'];
        return `+${(elapsed / Number(scale)).toLocaleString('zh-TW', { maximumFractionDigits: 2 })}${unit}`;
      }
      const duration = Math.abs(elapsed), clock = new Date(duration % 86400000).toISOString().slice(11, duration % 1000 ? 23 : 19);
      return `t${elapsed < 0 ? '−' : '+'}${Math.floor(duration / 86400000)}天 ${clock}`;
    }
    return new Date(time).toLocaleString('zh-TW', !compact ? { hour12: false }
      : range.end - range.start >= 604800000 ? { month: '2-digit', day: '2-digit' }
      : range.end - range.start >= 86400000 ? { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false }
      : { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false });
  };
  const times = useMemo(() => [...new Set(data.series.filter(series => series.available).flatMap(series => series.points.map(point => point.time)))].filter(Number.isFinite).sort((a, b) => a - b), [data.series]);
  const visibleTimes = useMemo(() => times.filter(time => time >= range.start && time <= range.end), [times, range.start, range.end]);
  // Controlled playback never writes back through an effect: only an explicit
  // chart interaction requests a seek, so frame arrival cannot race the parent.
  const hasCursor = controlledCursor === undefined ? cursor !== null : controlledCursor !== null;
  const cursorTime = controlledCursor !== undefined && controlledCursor !== null
    ? controlledCursor : nearestTrendTime(visibleTimes, Math.max(range.start, Math.min(range.end, cursor ?? currentTime ?? range.end)));
  const chartRows = useMemo(() => visibleTimes.map(time => ({ time })), [visibleTimes]);
  const plotted = useMemo(() => new Map(data.series.map(series => {
    const present = new Set<string>();
    let low = Infinity, high = -Infinity, goodLow = Infinity, goodHigh = -Infinity;
    const points = series.points.filter(point => point.time >= range.start && point.time <= range.end).map(point => {
      if (finitePoint(point)) {
        present.add(point.quality);
        if (point.quality !== 'missing') { low = Math.min(low, point.value!); high = Math.max(high, point.value!); }
        if (point.quality === 'good') { goodLow = Math.min(goodLow, point.value!); goodHigh = Math.max(goodHigh, point.value!); }
      }
      return { time: point.time,
        good: point.quality === 'good' && finitePoint(point) ? point.value : null,
        uncertain: point.quality === 'uncertain' && finitePoint(point) ? point.value : null,
        unknown: point.quality === 'unknown' && finitePoint(point) ? point.value : null,
        bad: point.quality === 'bad' && finitePoint(point) ? point.value : null };
    });
    return [series.id, { points, qualities: plotQualities.filter(quality => present.has(quality)), low, high, goodLow, goodHigh }];
  })), [data.series, range.start, range.end]);
  const pointIndex = useMemo(() => new Map(data.series.map(series => [series.id, new Map(series.points.map(point => [point.time, point]))])), [data.series]);
  const bandData = useMemo(() => data.variables.flatMap(variable => groups.flatMap(group => bandKinds.flatMap(band => {
    const candidates = data.series.filter(series => series.variableId === variable.id && groupId(series) === group.id && series.available);
    const low = candidates.find(series => series.kind === `${band.id}_low`), high = candidates.find(series => series.kind === `${band.id}_high`);
    if (!low || !high) return [];
    const pairedTimes = [...new Set([...low.points.map(point => point.time), ...high.points.map(point => point.time)])].filter(time => time >= range.start && time <= range.end).sort((a, b) => a - b);
    return [{ id: `${variable.id}:${group.id}:${band.id}`, variableId: variable.id, groupId: group.id, band: band.id, color: band.color,
      points: pairedTimes.map(time => {
        const a = pointIndex.get(low.id)?.get(time), b = pointIndex.get(high.id)?.get(time);
        return { time, value: a?.quality === 'good' && b?.quality === 'good' && finitePoint(a) && finitePoint(b) && a.value! <= b.value! ? [a.value!, b.value!] : null };
      }) }];
  }))), [data.variables, data.series, groups, pointIndex, range.start, range.end]);
  const domains = useMemo(() => {
    const result: Record<string, [number, number]> = {};
    for (const variable of data.variables) {
      const shared = axes[variable.id] ?? { mode: 'detail' };
      result[variable.id] = shared.locked ?? (shared.mode === 'manual' ? shared.manual : undefined) ?? trendDomain(data.series, variable.id, shared.mode === 'all' ? 'all' : 'detail', range);
      for (const group of groups) {
        const key = `${variable.id}|${group.id}`, setting = axes[key] ?? { mode: 'detail' };
        result[key] = !unlocked.includes(key) ? result[variable.id] : setting.locked ?? (setting.mode === 'manual' ? setting.manual : undefined) ?? trendDomain(data.series.filter(series => groupId(series) === group.id), variable.id, setting.mode === 'all' ? 'all' : 'detail', range);
      }
    }
    return result;
  }, [data.variables, data.series, groups, axes, unlocked, range.start, range.end]);
  const seriesColors = useMemo(() => {
    const occurrences = new Map<string, number>();
    return Object.fromEntries(data.series.map(series => {
      const groupIndex = groups.findIndex(group => group.id === groupId(series));
      const variableIndex = data.variables.findIndex(variable => variable.id === series.variableId);
      const key = `${series.variableId}|${groupId(series)}|${series.kind}`;
      const occurrence = occurrences.get(key) ?? 0;
      occurrences.set(key, occurrence + 1);
      const offset = series.kind === 'target' ? 1 : series.kind === 'prediction' ? 2 : series.kind === 'command' ? 3 : isTrendBound(series.kind) ? 4 : 0;
      return [series.id, colors[(Math.max(0, groupIndex) + Math.max(0, variableIndex) + offset + occurrence) % colors.length]];
    }));
  }, [data.series, data.variables, groups]);
  const renderedLines = useMemo(() => new Map(data.series.map(series => {
    const plot = plotted.get(series.id)!;
    const label = groups.find(group => group.id === groupId(series))?.label ?? groupId(series);
    return [series.id, plot.qualities.map(quality => <Line key={`${series.id}:${quality}`} yAxisId={series.variableId} data={plot.points} dataKey={quality} name={`${label} · ${series.label}`} type={series.interpolation === 'step' ? 'stepAfter' : 'linear'} stroke={quality === 'bad' ? '#fb7185' : quality === 'unknown' ? '#94a3b8' : seriesColors[series.id]} strokeWidth={quality === 'good' ? (series.kind === 'actual' ? 2 : series.kind === 'target' ? 1 : 1.5) : 0} strokeDasharray={lineDashes[series.kind]} dot={quality === 'good' ? (plot.points.length === 1 ? { r: 3, strokeWidth: 0 } : false) : { r: quality === 'bad' ? 3 : 2.5, strokeWidth: 1.5, fill: quality === 'uncertain' ? '#0f172a' : quality === 'bad' ? '#fb7185' : '#94a3b8' }} activeDot={false} connectNulls={false} isAnimationActive={false} onClick={() => { const variable = data.variables.find(item => item.id === series.variableId); if (variable) onVariableFocus?.(variable); }} />)];
  })), [data.series, data.variables, plotted, groups, seriesColors, onVariableFocus]);
  const renderedBands = useMemo(() => new Map(bandData.map(band => [band.id, <Area key={band.id} yAxisId={band.variableId} data={band.points} dataKey="value" type="stepAfter" stroke="none" fill={band.color} fillOpacity={0.1} connectNulls={false} isAnimationActive={false} legendType="none" />])), [bandData]);
  const seek = (time: number) => {
    if (!visibleTimes.length) return;
    const next = nearestTrendTime(visibleTimes, time);
    if (hasCursor && next === cursorTime) return;
    if (controlledCursor === undefined) setCursor(next);
    onCursorTimeChange?.(next); onCursorChange?.(next);
  };
  const changeWindow = (next: TrendRange) => {
    if (next.start >= next.end || !Number.isFinite(next.start) || !Number.isFinite(next.end)) return;
    setWindowRange(next); setRangeError('');
    setStartDraft(String((next.start - data.originTime) / 1000));
    setEndDraft(String((next.end - data.originTime) / 1000));
    if (next.start !== range.start || next.end !== range.end) onWindowChange?.(next);
  };
  const focusWindow = (time: number, hours: number) => {
    const span = Math.min(hours * 3600000, completeRange.end - completeRange.start);
    const start = Math.max(completeRange.start, Math.min(time - span / 2, completeRange.end - span));
    changeWindow({ start, end: start + span });
  };
  const overlay = view === 'overlay' && !mobile && variables.length <= 3;
  const eventKinds: Record<string, string> = { limit: '限制變更', target: '目標變更', mode: '模式', gap: '缺值', violation: '違規', quality: '品質' };
  const events = useMemo(() => data.events.filter(event => (eventKind === 'all' || event.kind === eventKind) && (eventGroup === 'all' || (event.groupId ?? 'live') === eventGroup) && (!event.variableId || selected.includes(event.variableId))), [data.events, eventKind, eventGroup, selected]);
  const page = Math.min(eventPage, Math.max(0, Math.ceil(events.length / 12) - 1));

  const linePicker = useCallback((variable: TrendVariable) => {
    const items = data.series.filter(series => series.variableId === variable.id);
    return <details className="min-w-0 rounded border border-slate-700 px-2 py-1.5">
      <summary className="cursor-pointer text-xs text-indigo-300" aria-label={`${variable.name} 逐條選線`}>選線 · 已選 {items.filter(series => series.available && enabled(series)).length}/{items.length} · {variable.unit || '單位未知'}</summary>
      <fieldset className="mt-2 min-w-0"><legend className="sr-only">{variable.name} 逐條選線</legend>
      <div className="mb-2 flex flex-wrap gap-2">{([['default', '恢復預設'], [true, '全部線'], [false, '清空']] as const).map(([choice, label]) => <button key={label} type="button" className={`${button} text-xs`} aria-label={`${variable.name} ${label}`} onClick={() => toggleLines(items, choice)}>{label}</button>)}</div>
      <div className="grid min-w-0 gap-2 sm:grid-cols-2 xl:grid-cols-3">{groups.filter(group => items.some(series => groupId(series) === group.id)).map(group => <div className="min-w-0" key={group.id}><p className="mb-1 break-words text-xs font-medium text-slate-300">{group.label}</p>{items.filter(series => groupId(series) === group.id).map(series => <label key={series.id} title={`${series.id} · ${series.source || '來源未提供'}`} className={`flex min-w-0 items-start gap-2 py-1 text-xs ${series.available ? 'text-slate-200' : 'text-slate-500'}`}>
        <input type="checkbox" className="mt-0.5 shrink-0" aria-label={`${variable.name} · ${group.label} · ${trendKindLabels[series.kind]} · ${series.label}`} checked={series.available && enabled(series)} disabled={!series.available} onChange={event => setLines(current => ({ ...current, [series.id]: event.target.checked }))} />
        <span className="min-w-0 break-words"><span style={{ color: series.available ? seriesColors[series.id] : undefined }}>{trendKindLabels[series.kind]}{series.label !== group.label && ` · ${series.label.startsWith(`${group.label} · `) ? series.label.slice(group.label.length + 3) : series.label}`}</span>{series.leadSeconds !== undefined && ` · 提前 ${series.leadSeconds} 秒`}{series.forecastScope === 'latest' && <small className="block break-all text-slate-400">{series.source}</small>}{!series.available && <span className="block">不可用：{series.unavailableReason || '來源未提供'}</span>}</span>
      </label>)}</div>)}</div>
      </fieldset>
    </details>;
  }, [data.series, groups, enabled, toggleLines, setLines, seriesColors]);
  const linePickers = useMemo(() => new Map(variables.map(variable => [variable.id, linePicker(variable)])), [variables, linePicker]);
  const renderPlot = (lane: TrendVariable[], group: TrendGroup | undefined, kind?: 'residual' | 'movement', roleLayout = false, showTimeAxis = true) => {
    const geometry = trendRoleGeometry(lane.length, mobile);
    const candidates = data.series.filter(series => lane.some(variable => variable.id === series.variableId) && (!group || groupId(series) === group.id));
    const shown = candidates.filter(series => series.available && enabled(series) && (kind ? series.kind === kind : series.kind !== 'residual' && series.kind !== 'movement'));
    const axisDomain = (variable: TrendVariable): [number, number] => {
      if (!kind) return domains[group ? `${variable.id}|${group.id}` : variable.id];
      let low = 0, high = 0;
      for (const series of data.series) if (series.variableId === variable.id && series.kind === kind && series.available && (!group || !unlocked.includes(`${variable.id}|${group.id}`) || groupId(series) === group.id)) {
        const plot = plotted.get(series.id)!;
        low = Math.min(low, plot.goodLow); high = Math.max(high, plot.goodHigh);
      }
      const pad = (high - low) * 0.06 || 0.01;
      return [low - pad, high + pad];
    };
    const clippingCandidates = kind ? shown : candidates.filter(series => series.available && (shown.includes(series) || bandData.some(band => band.variableId === series.variableId && band.groupId === groupId(series) && bands.includes(band.band) && (series.kind === `${band.band}_low` || series.kind === `${band.band}_high`))));
    const clipped = clippingCandidates.filter(series => {
      const domain = axisDomain(lane.find(variable => variable.id === series.variableId)!);
      const plot = plotted.get(series.id)!;
      return plot.low < domain[0] || plot.high > domain[1];
    });
    return <div className="min-w-0" key={kind ?? 'signal'}>
      {kind && <h5 className="mt-3 text-xs text-slate-300">{trendKindLabels[kind]} · 獨立軸（{lane.map(variable => variable.unit || '單位未知').join('、')}）{kind === 'movement' ? ' · 帶正負號，來源逐步差分' : ' · 同組、相同有效時間且品質良好配對'}</h5>}
      {clipped.length > 0 && <p role="status" className="mt-2 break-words text-xs text-amber-200">超出顯示軸範圍（未改寫數值）：{clipped.map(series => `${groupLabel(groupId(series))} ${series.label} ${trendKindLabels[series.kind]}`).join('、')}。可切換「包含各限制」或調整軸。</p>}
      <div className={`mt-2 min-w-0 ${kind || lane.every(variable => variable.role === 'DV') ? 'h-40' : 'h-60'}`} role="group" aria-label={`${lane.map(variable => variable.name).join('、')} · ${group?.label ?? '各組疊圖'} · ${kind ? trendKindLabels[kind] : '工程曲線'}`}>
        <ResponsiveContainer width="100%" height="100%"><ComposedChart data={chartRows} margin={roleLayout ? geometry.margin : { top: 12, right: 12, bottom: 4, left: 0 }} onMouseMove={state => { if (state.activeLabel != null && Number.isFinite(Number(state.activeLabel))) seek(Number(state.activeLabel)); }} onClick={state => { if (state.activeLabel != null && Number.isFinite(Number(state.activeLabel))) seek(Number(state.activeLabel)); }}>
          <CartesianGrid stroke="#334155" strokeDasharray="3 3" /><Tooltip content={() => null} cursor={false} />
          <XAxis dataKey="time" type="number" domain={[range.start, range.end === range.start ? range.end + 1 : range.end]} allowDataOverflow tickFormatter={time => timeLabel(Number(time), true)} tick={showTimeAxis ? { fill: '#94a3b8', fontSize: 10 } : false} tickLine={showTimeAxis} height={showTimeAxis ? 30 : 8} minTickGap={45} />
          {lane.map((variable, index) => <YAxis key={variable.id} yAxisId={variable.id} domain={axisDomain(variable)} type="number" allowDataOverflow orientation={index % 2 ? 'right' : 'left'} width={geometry.axisWidth} tickFormatter={valueLabel} tick={{ fill: '#cbd5e1', fontSize: 10 }} label={lane.length > 1 ? { value: `${variable.name} (${variable.unit || '?'})`, angle: index % 2 ? 90 : -90, position: 'insideLeft', fill: '#94a3b8', fontSize: 10 } : undefined} />)}
          {currentTime !== undefined && Number.isFinite(currentTime) && currentTime < range.end && <ReferenceArea yAxisId={lane[0]?.id} x1={Math.max(range.start, currentTime)} x2={range.end} fill="#818cf8" fillOpacity={0.07} strokeOpacity={0} />}
          {!kind && bandData.filter(band => lane.some(variable => variable.id === band.variableId) && (!group || group.id === band.groupId) && bands.includes(band.band)).map(band => renderedBands.get(band.id))}
          {shown.flatMap(series => renderedLines.get(series.id) ?? [])}
          {kind && lane.map(variable => <ReferenceLine key={variable.id} yAxisId={variable.id} y={0} stroke="#94a3b8" strokeDasharray="4 4" />)}
          {hasCursor && cursorTime >= range.start && cursorTime <= range.end && <ReferenceLine yAxisId={lane[0]?.id} x={cursorTime} stroke="#e2e8f0" strokeDasharray="2 3" />}
          {currentTime !== undefined && Number.isFinite(currentTime) && currentTime >= range.start && currentTime <= range.end && <ReferenceLine yAxisId={lane[0]?.id} x={currentTime} stroke="#a5b4fc" strokeDasharray="6 3" label={{ value: '目前／未來', fill: '#a5b4fc', fontSize: 10 }} />}
          {diagnostic && anchorTime !== undefined && anchorTime >= range.start && anchorTime <= range.end && <ReferenceLine yAxisId={lane[0]?.id} x={anchorTime} stroke="#fbbf24" label={{ value: '預測起算', fill: '#fbbf24', fontSize: 10 }} />}
        </ComposedChart></ResponsiveContainer>
      </div>
      {!shown.some(series => plotted.get(series.id)?.qualities.length) && <p className="text-xs text-amber-200">{kind === 'movement' ? '沒有已選取的逐步命令變化資料；不從減點曲線推算。' : kind === 'residual' ? '沒有已選取的有效時間配對殘差；不內插或跨組配對。' : '此視窗沒有已選取的可用數值。'}</p>}
      <ul className="mt-2 flex flex-wrap gap-x-3 gap-y-1 text-xs">{shown.map(series => <li key={series.id} className="max-w-full break-words"><svg className="mr-1 inline-block align-middle" width="24" height="8" aria-hidden="true"><line x1="0" y1="4" x2="24" y2="4" stroke={seriesColors[series.id]} strokeWidth="2" strokeDasharray={lineDashes[series.kind]} /></svg>{group ? '' : `${groupLabel(groupId(series))} · `}{lane.length > 1 && `${lane.find(variable => variable.id === series.variableId)?.name} · `}{series.label} · {trendKindLabels[series.kind]}{series.leadSeconds !== undefined && ` · 提前 ${series.leadSeconds} 秒`}</li>)}</ul>
    </div>;
  };
  const renderGroupAxis = (variable: TrendVariable, group: TrendGroup) => {
    const key = `${variable.id}|${group.id}`;
    return <div className="mt-2 space-y-2" key={key}><label className="flex items-start gap-2 text-xs"><input type="checkbox" className="shrink-0" aria-label={`${variable.name} ${group.label} 獨立縮放（解除同變數共軸）`} checked={unlocked.includes(key)} onChange={event => setUnlocked(current => event.target.checked ? [...current, key] : current.filter(item => item !== key))} /><span className="min-w-0 break-words">{variable.name} · 獨立縮放（解除同變數共軸）</span></label>{unlocked.includes(key) && <><p className="text-xs text-amber-200">此組軸已獨立，曲線高度不可直接比較。</p><AxisControls name={`${variable.name} ${group.label}`} setting={axes[key] ?? { mode: 'detail' }} domain={domains[key]} onChange={setting => setAxes(current => ({ ...current, [key]: setting }))} /></>}</div>;
  };
  const renderPanels = (lane: TrendVariable[]) => {
    const panelGroups = view === 'compare' ? groups.filter(group => data.series.some(series => lane.some(variable => variable.id === series.variableId) && groupId(series) === group.id)) : [undefined];
    return <div className={`grid min-w-0 gap-3 ${view === 'compare' && panelGroups.length > 1 ? 'md:grid-cols-2' : ''}`}>{panelGroups.map(group => <div key={group?.id ?? 'all'} className="min-w-0 rounded border border-slate-700 bg-slate-950/40 p-2">
      <h4 className="break-words text-sm font-medium">{group?.label ?? '各組同軸疊圖'} · {lane.map(variable => `${variable.name} (${variable.unit || '單位未知'})`).join('、')}</h4>
      {group && lane.map(variable => renderGroupAxis(variable, group))}
      {renderPlot(lane, group)}
      {lane.some(variable => variable.role === 'CV') && (diagnostic || data.series.some(series => lane.some(variable => variable.id === series.variableId && variable.role === 'CV') && (!group || groupId(series) === group.id) && series.kind === 'residual' && enabled(series))) && (data.series.some(series => lane.some(variable => variable.id === series.variableId && variable.role === 'CV') && (!group || groupId(series) === group.id) && series.kind === 'residual' && series.available) ? renderPlot(lane.filter(variable => variable.role === 'CV'), group, 'residual') : <p className="mt-2 text-xs text-slate-400">來源未提供有效時間配對殘差；不從減點曲線推算。</p>)}
      {lane.some(variable => variable.role === 'MV') && (data.series.some(series => lane.some(variable => variable.id === series.variableId && variable.role === 'MV') && (!group || groupId(series) === group.id) && series.kind === 'movement' && series.available) ? renderPlot(lane.filter(variable => variable.role === 'MV'), group, 'movement') : <p className="mt-2 text-xs text-slate-400">來源未提供逐步命令變化；不從減點曲線推算。</p>)}
    </div>)}</div>;
  };
  const renderRoleStack = (group?: TrendGroup) => {
    const lastRole = roleBlocks.filter(block => block.lanes.length).at(-1)?.role;
    return <section key={group?.id ?? 'empty'} className="min-w-0 space-y-3 rounded-lg border border-slate-700 bg-slate-950/40 p-2" aria-label={`${group?.label ?? '資料來源'} CV MV DV 角色區塊`}>
      <h3 className="break-words text-sm font-semibold">{group?.label ?? '資料來源'}</h3>
      {group && <details className="min-w-0"><summary className="cursor-pointer text-xs text-indigo-300">各變數分組縮放 · 預設與其他組共軸</summary>{variables.map(variable => renderGroupAxis(variable, group))}</details>}
      {roleBlocks.map(block => <section key={block.role} className="min-w-0 border-t border-slate-700 pt-2" aria-label={`${group?.label ?? '資料來源'} ${block.role} 區塊`}>
        <h4 className="text-sm font-medium">{block.role} · {block.role === 'CV' ? '受控變數' : block.role === 'MV' ? '操縱變數' : block.role === 'DV' ? '擾動變數' : '其他工程變數'}</h4>
        {!block.lanes.length && <p className="py-4 text-xs text-slate-400">{block.configured ? `${block.role} 變數已全部隱藏；可在「顯示變數」重新選取。` : `未配置 ${block.role}`}</p>}
        {block.lanes.map((lane, laneIndex) => {
          const sources = data.series.filter(series => lane.some(variable => variable.id === series.variableId) && (!group || groupId(series) === group.id));
          const auxiliary = block.role === 'CV' && (diagnostic || sources.some(series => series.kind === 'residual' && enabled(series))) ? 'residual' : block.role === 'MV' ? 'movement' : undefined;
          const available = auxiliary && sources.some(series => series.kind === auxiliary && series.available);
          const plots: ('residual' | 'movement' | undefined)[] = available ? [undefined, auxiliary] : [undefined];
          const lastLane = block.role === lastRole && laneIndex === block.lanes.length - 1;
          return <div key={lane.map(variable => variable.id).join('|')} className="min-w-0 pt-2">
            <h5 className="break-words text-xs text-slate-300">{lane.map((variable, index) => `${lane.length === 2 ? index ? '右軸 ' : '左軸 ' : ''}${variable.name} (${variable.unit || '單位未知'})`).join(' · ')}</h5>
            {plots.map((kind, index) => renderPlot(lane, group, kind, true, lastLane && index === plots.length - 1))}
            {auxiliary && !available && <p className="mt-2 text-xs text-slate-400">{auxiliary === 'residual' ? '來源未提供有效時間配對殘差；不從減點曲線推算。' : '來源未提供逐步命令變化；不從減點曲線推算。'}</p>}
          </div>;
        })}
      </section>)}
    </section>;
  };

  return <section className="min-w-0 space-y-4 text-slate-200" aria-label="獨立工程尺度趨勢" aria-busy={loading}>
    <div className="flex flex-wrap items-center gap-2"><div className="flex gap-1" role="group" aria-label="趨勢用途"><button type="button" className={button} aria-pressed={!diagnostic} onClick={() => setDiagnostic(false)}>製程比較</button><button type="button" className={button} aria-pressed={diagnostic} onClick={() => setDiagnostic(true)}>預測診斷</button></div><label className="text-xs">排列 <select aria-label="曲線排列" className={control} value={view} onChange={event => setView(event.target.value as View)}><option value="role_blocks">CV／MV／DV 時間對齊區塊</option><option value="compare">同變數分組並排</option><option value="lanes">進階：每變數各組疊圖</option><option value="overlay" disabled={variables.length > 3 || mobile}>進階：至多 3 變數多軸</option></select></label>{loading && <span role="status" className="text-xs text-sky-300">正在載入所選視窗…</span>}</div>
    <p className="text-xs text-slate-400">{diagnostic ? '預測按有效時間與實際值對照；殘差使用獨立零基準軸。游標不改變預測起算時間。' : '預設實際值與目標；MV 顯示命令及可用實際回授。不同變數永不共用尺度，同變數各組預設共軸。'}</p>
    <fieldset className="flex flex-wrap gap-x-4 gap-y-2"><legend className="mb-2 text-xs">顯示變數</legend>{data.variables.map(variable => <label key={variable.id} className="flex max-w-full items-center gap-2 break-words text-xs"><input type="checkbox" aria-label={`顯示 ${variable.name}`} checked={selected.includes(variable.id)} onChange={event => { setInventory(current => [...new Set([...current, ...data.variables.map(item => item.id)])]); setSelection(event.target.checked ? [...selected, variable.id] : selected.filter(id => id !== variable.id)); }} />{variable.role} · {variable.name} ({variable.unit || '單位未知'})</label>)}</fieldset>
    {onVariableFocus && <div className="flex flex-wrap gap-2" role="group" aria-label="設備變數聚焦">{data.variables.map(variable => <button type="button" key={variable.id} className={`${button} text-xs`} aria-pressed={focusedVariableId === variable.id} onClick={() => onVariableFocus(variable)}>聚焦 {variable.role} · {variable.name}</button>)}</div>}
    <ProcessTrendOverview data={data} range={range} cursor={hasCursor ? cursorTime : Number.NaN} timeLabel={timeLabel} onWindowChange={changeWindow} />
    <div className="space-y-2 rounded-lg border border-slate-700 p-3">
      <form className="flex flex-wrap items-end gap-2" onSubmit={event => {
        event.preventDefault(); const parsed = parseTrendDomain(startDraft, endDraft);
        if (!parsed) { setRangeError('開始／結束須為有限秒數，且開始早於結束。'); return; }
        const start = data.originTime + parsed[0] * 1000, end = data.originTime + parsed[1] * 1000;
        if (start < completeRange.start || end > completeRange.end) { setRangeError('超出完整資料期間，請使用總覽所示範圍。'); return; }
        changeWindow({ start, end });
      }}>
        <label className="text-xs">開始（相對秒）<input aria-label="視窗開始相對秒數" className={`${control} mt-1 block w-36`} type="number" step="any" value={startDraft} placeholder={String((range.start - data.originTime) / 1000)} onChange={event => setStartDraft(event.target.value)} /></label>
        <label className="text-xs">結束（相對秒）<input aria-label="視窗結束相對秒數" className={`${control} mt-1 block w-36`} type="number" step="any" value={endDraft} placeholder={String((range.end - data.originTime) / 1000)} onChange={event => setEndDraft(event.target.value)} /></label>
        <button type="submit" className={button}>套用時間</button><button type="button" className={button} onClick={() => { setStartDraft(''); setEndDraft(''); changeWindow(completeRange); }}>重設完整區間</button>
        {[6, 24].map(hours => <button key={hours} type="button" className={button} onClick={() => focusWindow(cursorTime, hours)}>游標附近 {hours}h</button>)}
        {rangeError && <p role="alert" className="w-full text-xs text-rose-300">{rangeError}</p>}
      </form>
      <p className="break-words text-xs text-slate-400">{timeLabel(range.start)} → {timeLabel(range.end)} · {data.timeBasis === 'relative' ? '相對資料起點' : '本機時區（原始 UTC）'} · 僅改變檢視，不改變評分範圍</p>
      {visibleTimes.length > 0 && <label className="block text-xs">共用游標 · {hasCursor ? timeLabel(cursorTime) : '未設定'}<input className="mt-2 block w-full accent-sky-400" aria-label="共用時間游標，方向鍵移動一筆，Home 和 End 到起終點" type="range" min={0} max={visibleTimes.length - 1} step={1} value={Math.max(0, visibleTimes.indexOf(nearestTrendTime(visibleTimes, cursorTime)))} onChange={event => seek(visibleTimes[Number(event.target.value)])} /></label>}
      {onCursorTimeChange && <button type="button" className={`${button} text-xs`} onClick={() => { if (controlledCursor === undefined) setCursor(null); onCursorTimeChange(null); }}>清除共用游標</button>}
    </div>
    {loading ? <p role="status" className="text-xs text-slate-400">所選視窗完整解析度統計載入中；不顯示舊視窗統計。</p> : <ProcessTrendSummary data={data} range={range} variables={selected} />}
    <fieldset className="flex flex-wrap gap-x-4 gap-y-2"><legend className="mb-1 text-xs">可選背景帶（與各上下限線獨立；缺少良好配對界線處留空）</legend>{bandKinds.map(band => {
      const available = bandData.some(item => item.band === band.id && selected.includes(item.variableId));
      return <label key={band.id} className="flex items-center gap-2 text-xs" style={{ color: band.color }}><input type="checkbox" checked={available && bands.includes(band.id)} disabled={!available} onChange={event => setBands(current => event.target.checked ? [...current, band.id] : current.filter(id => id !== band.id))} />{band.label}{!available && '（未提供可配對上下界）'}</label>;
    })}</fieldset>
    {view === 'overlay' && !overlay && <p className="text-xs text-amber-200">手機或超過 3 個變數時，改為每變數獨立泳道。</p>}
    {!variables.length && <p role="status" className="text-sm text-amber-200">目前沒有選取變數。</p>}
    {view === 'role_blocks' ? <div className="min-w-0 space-y-3">
      <p className="text-xs text-slate-400">每組由上而下為 CV、MV、DV；時間範圍與游標一致。每個變數使用原始工程單位與獨立 Y 軸，並非正規化。桌面同角色至多兩變數分列左右軸，更多變數或手機使用獨立子泳道。</p>
      <details className="min-w-0 rounded-lg border border-slate-700 p-3"><summary className="cursor-pointer text-sm text-indigo-300">逐條選線與同變數共用軸設定</summary><div className="mt-3 grid min-w-0 gap-3 lg:grid-cols-2">{variables.map(variable => <div className="min-w-0 space-y-2" key={variable.id}><h4 className="break-words text-xs">{variable.role} · {variable.name}</h4>{linePickers.get(variable.id)}<AxisControls name={`${variable.name} 共用`} setting={axes[variable.id] ?? { mode: 'detail' }} domain={domains[variable.id]} onChange={setting => setAxes(current => ({ ...current, [variable.id]: setting }))} /></div>)}</div></details>
      <div className={`grid min-w-0 gap-3 ${groups.length > 1 ? 'md:grid-cols-2' : ''}`}>{groups.length ? groups.map(group => renderRoleStack(group)) : renderRoleStack()}</div>
    </div> : overlay ? <div className="space-y-3 rounded-lg border border-slate-700 p-3">{variables.map(variable => <div className="space-y-2" key={variable.id}>{linePickers.get(variable.id)}<AxisControls name={variable.name} setting={axes[variable.id] ?? { mode: 'detail' }} domain={domains[variable.id]} onChange={setting => setAxes(current => ({ ...current, [variable.id]: setting }))} /></div>)}{renderPanels(variables)}</div> : variables.map(variable => <section key={variable.id} className="min-w-0 space-y-3 rounded-lg border border-slate-700 p-3" aria-label={`${variable.name} 比較列`}>
      <div className="flex min-w-0 flex-wrap items-start gap-2"><h3 className="mr-auto break-words py-1 text-sm font-semibold">{variable.role} · {variable.name} · {variable.unit || '單位未知'}</h3><div className="min-w-0 max-w-full">{linePickers.get(variable.id)}</div></div>
      <details><summary className="cursor-pointer text-xs text-indigo-300">同變數共用 Y 軸 · {valueLabel(domains[variable.id][0])} → {valueLabel(domains[variable.id][1])} · 調整／鎖定</summary><div className="mt-2"><AxisControls name={`${variable.name} 共用`} setting={axes[variable.id] ?? { mode: 'detail' }} domain={domains[variable.id]} onChange={setting => setAxes(current => ({ ...current, [variable.id]: setting }))} /></div></details>
      {renderPanels([variable])}
    </section>)}
    <details className="rounded-lg border border-slate-700 p-3"><summary className="cursor-pointer text-sm">共用游標讀值 · {hasCursor ? timeLabel(cursorTime) : '未設定'}</summary><p className="my-2 text-xs text-slate-400">只讀取完全相同有效時間的原始記錄，不內插缺值。不良＝紅點、不確定＝空心點、未知＝灰點；缺值不連線。</p><div className="grid min-w-0 gap-2 sm:grid-cols-2 xl:grid-cols-3">{data.series.filter(series => selected.includes(series.variableId) && enabled(series)).map(series => {
      const point = hasCursor ? pointIndex.get(series.id)?.get(cursorTime) : undefined, variable = data.variables.find(item => item.id === series.variableId);
      return <div key={series.id} className="min-w-0 break-words rounded bg-slate-900 p-2 text-xs"><p style={{ color: seriesColors[series.id] }}>{variable?.name} · {groupLabel(groupId(series))} · {series.label} · {trendKindLabels[series.kind]}</p><p className="mt-1 font-mono">{series.available && point && finitePoint(point) ? `${valueLabel(point.value!)} ${variable?.unit || '單位未知'} · ${qualityLabels[point.quality]}` : '此時無可用記錄'}</p>{point?.issuedAt !== undefined && <p className="mt-1 text-slate-400">發佈：{timeLabel(point.issuedAt)}<br />有效：{timeLabel(point.time)}</p>}<p className="mt-1 break-all text-slate-500">{series.source}</p></div>;
    })}</div></details>
    <details className="rounded-lg border border-slate-700 p-3"><summary className="cursor-pointer text-sm">事件定位 · {data.events.length.toLocaleString()} 筆</summary><div className="mt-3 flex flex-wrap items-center gap-2"><label className="text-xs">事件 <select aria-label="事件類型篩選" className={control} value={eventKind} onChange={event => { setEventKind(event.target.value); setEventPage(0); }}><option value="all">全部</option>{Object.entries(eventKinds).map(([kind, label]) => <option key={kind} value={kind}>{label}</option>)}</select></label><label className="text-xs">組別／來源 <select aria-label="事件組別篩選" className={control} value={eventGroup} onChange={event => { setEventGroup(event.target.value); setEventPage(0); }}><option value="all">全部組別</option>{groups.map(group => <option key={group.id} value={group.id}>{group.label}</option>)}</select></label><span className="text-xs text-slate-400">只列已選變數；點選後聚焦同一個 6h 視窗。</span></div><div className="my-2 space-y-2">{events.slice(page * 12, page * 12 + 12).map((event, index) => <button key={`${event.time}:${event.groupId}:${index}`} type="button" className={`${button} block w-full whitespace-normal break-words text-left text-xs`} onClick={() => { setCursor(event.time); onCursorChange?.(event.time); focusWindow(event.time, 6); }}>{timeLabel(event.time)} · {eventKinds[event.kind]} · {groupLabel(event.groupId ?? 'live')} · {data.variables.find(variable => variable.id === event.variableId)?.name ?? '共用'} · {event.label}</button>)}</div>{!events.length && <p className="text-xs text-slate-400">此篩選沒有事件記錄。</p>}<div className="flex items-center gap-2"><button type="button" className={button} disabled={page === 0} onClick={() => setEventPage(page - 1)}>上一頁</button><span className="text-xs">{page + 1} / {Math.max(1, Math.ceil(events.length / 12))}</span><button type="button" className={button} disabled={(page + 1) * 12 >= events.length} onClick={() => setEventPage(page + 1)}>下一頁</button></div></details>
    <details className="text-xs text-slate-400"><summary className="cursor-pointer">來源、品質與資料狀態{data.warnings.length ? ` · ${data.warnings.length} 則提示` : ''}</summary><div className="mt-2 space-y-2 break-words"><p>{data.provenance || '來源未提供'} · 原始 {data.rawSamples.toLocaleString()} 筆／顯示 {data.displaySamples.toLocaleString()} 筆。</p><p>品質規格、操作限制、軟帶與有效量程不同；只填色來源實際提供的良好上下界，不補造缺少的限制。陰影不是信賴區間。預測／殘差不混用起算時間與有效時間。曲線僅供顯示，不寫入控制器。</p>{data.warnings.map((warning, index) => <p className="text-amber-200" key={index}>{warning}</p>)}</div></details>
  </section>;
}
