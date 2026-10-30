import { useEffect, useMemo, useState } from 'react';
import { researchError, researchRequest } from '../lib/research';
import { mergeTrendData } from '../lib/processTrend';
import type { ProcessTrendData, TrendRange, TrendVariable } from '../types/ProcessTrend';
import type { TwinVariableFocus } from '../types/DigitalTwin';
import { ProcessTrendChart } from './ProcessTrendChart';
import { ResearchNotice, researchButton, researchControl } from './ResearchControls';

export type TwinArchiveSource = { id: string; label: string; path: string };
export type DigitalTwinArchiveProps = {
  sources: TwinArchiveSource[]; storageKey: string; focus?: TwinVariableFocus | null;
  onVariableFocus?: (variable: TrendVariable) => void;
  cursorTime?: number | null; onCursorTimeChange?: (time: number | null) => void; currentTime?: number;
  onData?: (data: ProcessTrendData) => void; forecastData?: ProcessTrendData | null;
};

function identified(data: ProcessTrendData, source: TwinArchiveSource): ProcessTrendData {
  const seriesId = (id: string) => `${source.id}:${id}`;
  return { ...data, groups: [{ id: source.id, label: source.label }],
    series: data.series.map(series => ({ ...series, id: seriesId(series.id), groupId: source.id })),
    events: data.events.map(event => ({ ...event, groupId: source.id })),
    overview: data.overview ? { ...data.overview, series: data.overview.series.map(series => ({ ...series, seriesId: seriesId(series.seriesId), groupId: source.id })) } : undefined,
    windowSummary: data.windowSummary ? { ...data.windowSummary, rows: data.windowSummary.rows.map(row => ({ ...row, seriesId: seriesId(row.seriesId), groupId: source.id })) } : undefined,
  };
}

export function DigitalTwinArchive({ sources, storageKey, focus, onVariableFocus, cursorTime, onCursorTimeChange, currentTime, onData, forecastData }: DigitalTwinArchiveProps) {
  const sourcesKey = JSON.stringify(sources);
  const [range, setRange] = useState<{ start?: number; end?: number }>({});
  const [draft, setDraft] = useState({ start: '', end: '' });
  const [revision, setRevision] = useState(0);
  const [loaded, setLoaded] = useState<{ key: string; sourcesKey: string; data: ProcessTrendData[] } | null>(null);
  const [result, setResult] = useState<{ key: string; error: string } | null>(null);
  const [inputError, setInputError] = useState('');
  const requestKey = `${sourcesKey}:${range.start}:${range.end}:${revision}`;
  const loading = result?.key !== requestKey;
  const error = result?.key === requestKey ? result.error : '';
  useEffect(() => {
    const controller = new AbortController();
    const selected: TwinArchiveSource[] = JSON.parse(sourcesKey);
    const query = new URLSearchParams();
    if (range.start !== undefined) query.set('start', String(range.start));
    if (range.end !== undefined) query.set('end', String(range.end));
    void Promise.all(selected.map(async source => {
      const data = await researchRequest<ProcessTrendData>(`${source.path}?${query}`, undefined, controller.signal);
      if (data.schemaVersion !== 1 || !Array.isArray(data.series) || !Array.isArray(data.variables)) throw new Error('封存曲線格式不相容。');
      return identified(data, source);
    })).then(data => {
      if (controller.signal.aborted) return;
      const first = data[0];
      const signature = (item: ProcessTrendData) => JSON.stringify(item.variables.map(({ id, role, name, unit }) => ({ id, role, name, unit })));
      if (first && data.some(item => item.originTime !== first.originTime || item.timeBasis !== first.timeBasis || item.sampleTimeMs !== first.sampleTimeMs || signature(item) !== signature(first))) throw new Error('比較已阻止：變數契約或時間基準不相容。');
      setLoaded({ key: requestKey, sourcesKey, data });
      setResult({ key: requestKey, error: '' });
    }).catch(failure => { if (!controller.signal.aborted) setResult({ key: requestKey, error: researchError(failure) }); });
    return () => controller.abort();
  }, [sourcesKey, range.start, range.end, revision, requestKey]);
  const data = useMemo(() => loaded?.sourcesKey === sourcesKey ? mergeTrendData(loaded.data) : null, [loaded, sourcesKey]);
  useEffect(() => { if (data && loaded?.key === requestKey) onData?.(data); }, [data, loaded?.key, requestKey, onData]);
  const displayData = useMemo(() => {
    if (!data || !forecastData || forecastData.originTime !== data.originTime ||
        forecastData.timeBasis !== data.timeBasis || forecastData.sampleTimeMs !== data.sampleTimeMs ||
        forecastData.variables.length !== data.variables.length || forecastData.variables.some((variable, index) => {
          const archived = data.variables[index];
          return variable.id !== archived.id || variable.name !== archived.name || variable.role !== archived.role || variable.unit !== archived.unit;
        })) return data;
    const latest = forecastData.series.filter(series => series.forecastScope === 'latest' &&
      sources.some(source => source.id === series.groupId));
    if (!latest.length) return data;
    return { ...data, currentTime: forecastData.currentTime, series: [...data.series.filter(series => series.forecastScope !== 'latest'), ...latest],
      range: { start: data.range.start, end: Math.max(data.range.end, forecastData.range.end) } };
  }, [data, forecastData, sources]);
  function changeWindow(next: TrendRange) {
    if (!data) return;
    const value = { start: Math.max(0, (next.start - data.originTime) / 1000), end: Math.max(0, (next.end - data.originTime) / 1000) };
    setRange(value); setDraft({ start: String(value.start), end: String(value.end) }); setInputError('');
  }
  return <div className="min-w-0 space-y-3">
    <details className="rounded-lg border border-slate-700 p-3"><summary className="cursor-pointer text-sm text-indigo-300">封存區間（完整資料來源，不是最近掃描）</summary><form className="mt-3 flex flex-wrap items-end gap-2" onSubmit={event => {
      event.preventDefault();
      const start = draft.start.trim() ? Number(draft.start) : undefined, end = draft.end.trim() ? Number(draft.end) : undefined;
      if ([start, end].some(value => value !== undefined && (!Number.isFinite(value) || value < 0)) || (start !== undefined && end !== undefined && start >= end)) { setInputError('開始與結束須為非負秒數，且結束晚於開始。'); return; }
      setRange({ start, end }); setInputError('');
    }}><label className="min-w-0 text-xs text-slate-400">開始（相對秒）<input aria-label="孿生封存開始秒數" className={`${researchControl} w-36`} type="number" min={0} step="any" value={draft.start} onChange={event => setDraft(current => ({ ...current, start: event.target.value }))} /></label><label className="min-w-0 text-xs text-slate-400">結束（相對秒）<input aria-label="孿生封存結束秒數" className={`${researchControl} w-36`} type="number" min={0} step="any" value={draft.end} onChange={event => setDraft(current => ({ ...current, end: event.target.value }))} /></label><button type="submit" className={researchButton}>載入孿生指定區間</button><button type="button" className={researchButton} onClick={() => { setRange({}); setDraft({ start: '', end: '' }); setInputError(''); }}>載入孿生完整封存</button></form></details>
    {inputError && <ResearchNotice error={inputError} />}
    {loading && <p role="status" className="text-sm text-indigo-300">載入真實封存曲線…</p>}
    {error && <><ResearchNotice error={error} /><button type="button" className={researchButton} onClick={() => setRevision(value => value + 1)}>重新載入孿生封存</button></>}
    {displayData && <ProcessTrendChart key={sourcesKey} data={displayData} storageKey={storageKey}
      focusedVariableId={focus ? data?.variables.find(variable => variable.role === focus.role && variable.name === focus.name && variable.unit === focus.unit)?.id : null}
      onVariableFocus={onVariableFocus} cursorTime={cursorTime} onCursorTimeChange={onCursorTimeChange}
      currentTime={currentTime ?? displayData.currentTime} initialView="role_blocks"
      fullRange={data?.overview ? { start: data.overview.range.start, end: Math.max(data.overview.range.end, displayData.range.end) } : undefined}
      onWindowChange={changeWindow} loading={loading || loaded?.key !== requestKey} />}
  </div>;
}
