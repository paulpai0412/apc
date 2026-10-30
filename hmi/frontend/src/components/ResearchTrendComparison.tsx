import { useEffect, useMemo, useState } from 'react';
import { researchError, researchRequest } from '../lib/research';
import { mergeTrendData } from '../lib/processTrend';
import type { ProcessTrendData, TrendRange } from '../types/ProcessTrend';
import type { ResearchRun } from '../types/Research';
import { ProcessTrendChart } from './ProcessTrendChart';
import { ResearchField, ResearchNotice, ResearchPanel, researchButton, researchControl } from './ResearchControls';

type Comparison = 'improvement' | 'methods' | 'advanced';
type SecondsRange = { start?: number; end?: number };
const methodLabel = (method: string) => ({ standard_dmc: '標準 DMC', soft_band: 'Soft-band MPC' })[method] ?? method;
const variants = [{ id: 'baseline', label: '基準 native' }, { id: 'optimized', label: '優化 native' }, { id: 'transport', label: '優化 COM DA' }];

export function ResearchTrendComparison({ run }: { run: ResearchRun }) {
  const methodsKey = run.results?.methods.map(item => item.method).join(',') ?? '';
  const methods = useMemo(() => methodsKey ? methodsKey.split(',') : [], [methodsKey]);
  const [comparison, setComparison] = useState<Comparison>('improvement');
  const [method, setMethod] = useState(methods[0] ?? '');
  const [advancedMethods, setAdvancedMethods] = useState(methods);
  const [advancedVariants, setAdvancedVariants] = useState(['baseline', 'optimized']);
  const activeMethod = methods.includes(method) ? method : methods[0];
  const jobsKey = JSON.stringify((comparison === 'improvement' ? activeMethod ? [activeMethod] : [] : comparison === 'methods' ? methods : methods.filter(item => advancedMethods.includes(item)))
    .flatMap(method => (comparison === 'improvement' ? ['baseline', 'optimized'] : comparison === 'methods' ? ['optimized'] : advancedVariants).map(variant => ({ method, variant }))));
  const [forecastMode, setForecastMode] = useState<'history' | 'snapshot'>('history');
  const [lead, setLead] = useState(1);
  const [leadDraft, setLeadDraft] = useState('1');
  const [snapshot, setSnapshot] = useState(0);
  const [snapshotDraft, setSnapshotDraft] = useState('0');
  const [cursor, setCursor] = useState<number | null>(null);
  const [range, setRange] = useState<SecondsRange>({});
  const [draft, setDraft] = useState({ start: '', end: '' });
  const [revision, setRevision] = useState(0);
  const [loaded, setLoaded] = useState<{ key: string; data: ProcessTrendData[] }>({ key: '', data: [] });
  const [loading, setLoading] = useState(false);
  const [requestError, setRequestError] = useState('');
  const [inputError, setInputError] = useState('');
  const groupKey = `${run.id}:${jobsKey}:${forecastMode}`;
  useEffect(() => {
    const controller = new AbortController();
    const jobs: { method: string; variant: string }[] = JSON.parse(jobsKey);
    setRequestError('');
    if (!jobs.length) { setLoaded({ key: groupKey, data: [] }); setLoading(false); return () => controller.abort(); }
    setLoading(true);
    void Promise.all(jobs.map(async job => {
      const query = new URLSearchParams(job);
      if (forecastMode === 'snapshot') query.set('at', String(snapshot));
      else {
        query.set('lead_steps', String(lead));
        if (range.start !== undefined) query.set('start', String(range.start));
        if (range.end !== undefined) query.set('end', String(range.end));
      }
      try {
        const result = await researchRequest<ProcessTrendData>(`/runs/${encodeURIComponent(run.id)}/${forecastMode === 'snapshot' ? 'forecast' : 'series'}?${query}`, undefined, controller.signal);
        if (result.schemaVersion !== 1 || !Array.isArray(result.series) || !Array.isArray(result.variables)) throw new Error('工程曲線資料格式不正確。');
        return { data: result, error: '' };
      } catch (error) {
        return { data: null, error: `${methodLabel(job.method)} · ${variants.find(item => item.id === job.variant)?.label ?? job.variant}：${researchError(error)}` };
      }
    })).then(results => {
      if (controller.signal.aborted) return;
      setLoaded({ key: groupKey, data: results.flatMap(result => result.data ? [result.data] : []) });
      setRequestError(results.map(result => result.error).filter(Boolean).join('\n'));
      setLoading(false);
    });
    return () => controller.abort();
  }, [run.id, jobsKey, groupKey, forecastMode, snapshot, lead, range.start, range.end, revision]);
  const data = useMemo(() => loaded.key === groupKey ? mergeTrendData(loaded.data) : null, [loaded, groupKey]);
  const anchor = useMemo(() => {
    if (forecastMode !== 'snapshot' || !data) return undefined;
    let first = Infinity;
    for (const series of data.series) for (const point of series.points) if (point.issuedAt !== undefined) first = Math.min(first, point.issuedAt);
    return Number.isFinite(first) ? first : undefined;
  }, [data, forecastMode]);
  const fullRange = useMemo(() => {
    if (!data || forecastMode !== 'history') return undefined;
    if (data.overview) return data.overview.range;
    const splits = run.results?.splits;
    return data.sampleTimeMs !== null && splits?.length ? {
      start: data.originTime + splits[0].first * data.sampleTimeMs,
      end: data.originTime + (splits[splits.length - 1].last - 1) * data.sampleTimeMs,
    } : undefined;
  }, [data, forecastMode, run.results?.splits]);
  function changeWindow(next: TrendRange) {
    if (!data) return;
    const selected = { start: Math.max(0, (next.start - data.originTime) / 1000), end: Math.max(0, (next.end - data.originTime) / 1000) };
    setRange(selected); setDraft({ start: String(selected.start), end: String(selected.end) }); setInputError('');
  }
  function loadCursorForecast() {
    if (cursor === null || !data) return;
    const at = Math.max(0, (cursor - data.originTime) / 1000);
    setSnapshot(at); setSnapshotDraft(String(at)); setForecastMode('snapshot'); setInputError('');
  }
  return <ResearchPanel title="閉迴路比較">
    <div className="min-w-0 space-y-4">
      <div className="flex flex-wrap items-center gap-2" role="group" aria-label="曲線比較方式">
        {([{ id: 'improvement', label: '基準 ↔ 優化' }, { id: 'methods', label: '方法比較' }, { id: 'advanced', label: '進階疊圖' }] as const).map(item => <button key={item.id} type="button" className={`${researchButton} ${comparison === item.id ? 'border-indigo-400 bg-indigo-950 text-indigo-100' : ''}`} aria-pressed={comparison === item.id} onClick={() => setComparison(item.id)}>{item.label}</button>)}
        {comparison === 'improvement' && <label className="flex min-w-0 items-center gap-2 whitespace-nowrap text-sm text-slate-300">方法<select aria-label="基準優化比較方法" className={researchControl} value={activeMethod ?? ''} onChange={event => setMethod(event.target.value)}>{methods.map(item => <option key={item} value={item}>{methodLabel(item)}</option>)}</select></label>}
      </div>
      <p className="text-xs text-slate-400">{comparison === 'improvement' ? '同一方法的基準與優化分開看。' : comparison === 'methods' ? '只比較各方法的優化結果，不重複疊入基準。' : '自行勾選方法、來源與每條線；可改回分面。'}封存模擬資料，非工廠實測。</p>
      {comparison === 'advanced' && <div className="space-y-3 rounded-lg border border-slate-700 p-3">
        <fieldset className="flex flex-wrap gap-3"><legend className="mb-2 text-xs text-slate-400">顯示方法</legend>{methods.map(item => <label key={item} className="flex items-center gap-2 text-sm"><input type="checkbox" checked={advancedMethods.includes(item)} onChange={event => setAdvancedMethods(current => event.target.checked ? [...current, item] : current.filter(value => value !== item))} />{methodLabel(item)}</label>)}</fieldset>
        <fieldset className="flex flex-wrap gap-3"><legend className="mb-2 text-xs text-slate-400">結果來源</legend>{variants.map(item => <label key={item.id} className="flex items-center gap-2 text-sm"><input type="checkbox" checked={advancedVariants.includes(item.id)} onChange={event => setAdvancedVariants(current => event.target.checked ? [...current, item.id] : current.filter(value => value !== item.id))} />{item.label}</label>)}</fieldset>
        <p className="text-xs text-slate-400">native 與 COM DA 是執行／傳輸路徑，不是兩種控制演算法。</p>
      </div>}
      <details className="rounded-lg border border-slate-700 p-3">
        <summary className="cursor-pointer text-sm text-indigo-300">預測資料與精確時間範圍</summary>
        <div className="mt-3 space-y-3">
          <ResearchField label="預測資料"><select className={researchControl} value={forecastMode} onChange={event => { setForecastMode(event.target.value as typeof forecastMode); setInputError(''); }}><option value="history">固定提前預測 · 與實際時間對齊</option><option value="snapshot">單次求解 · 完整未來預測／MV 計畫</option></select></ResearchField>
          {forecastMode === 'history' ? <>
            <form className="flex flex-wrap items-end gap-2" onSubmit={event => {
              event.preventDefault(); const value = Number(leadDraft);
              if (!Number.isSafeInteger(value) || value < 1) { setInputError('提前步數須為大於零的整數。'); return; }
              setLead(value); setInputError('');
            }}><label className="grid gap-1 text-xs text-slate-400">固定提前步數<input aria-label="固定提前步數" type="number" min={1} step={1} className={`${researchControl} w-28`} value={leadDraft} onChange={event => setLeadDraft(event.target.value)} /></label><button className={researchButton} type="submit">套用提前步數</button><button className={researchButton} type="button" disabled={cursor === null || !data || loading} onClick={loadCursorForecast}>以游標載入完整預測</button></form>
            <form className="flex flex-wrap items-end gap-2" onSubmit={event => {
              event.preventDefault();
              const start = draft.start.trim() ? Number(draft.start) : undefined, end = draft.end.trim() ? Number(draft.end) : undefined;
              if ([start, end].some(value => value !== undefined && (!Number.isFinite(value) || value < 0)) || (start !== undefined && end !== undefined && start >= end)) { setInputError('請輸入非負秒數，且結束晚於開始；空白表示完整邊界。'); return; }
              setRange({ start, end }); setInputError('');
            }}>
              <label className="grid gap-1 text-xs text-slate-400">開始（相對秒）<input aria-label="研究開始秒數" type="number" min={0} step="any" className={`${researchControl} w-36`} value={draft.start} placeholder="資料起點" onChange={event => setDraft(current => ({ ...current, start: event.target.value }))} /></label>
              <label className="grid gap-1 text-xs text-slate-400">結束（相對秒）<input aria-label="研究結束秒數" type="number" min={0} step="any" className={`${researchControl} w-36`} value={draft.end} placeholder="資料終點" onChange={event => setDraft(current => ({ ...current, end: event.target.value }))} /></label>
              <button className={researchButton} type="submit">載入指定區間</button><button className={researchButton} type="button" onClick={() => { setRange({}); setDraft({ start: '', end: '' }); setInputError(''); }}>載入完整歷史</button>
            </form>
          </> : <form className="flex flex-wrap items-end gap-2" onSubmit={event => {
            event.preventDefault(); const value = Number(snapshotDraft);
            if (!snapshotDraft.trim() || !Number.isFinite(value) || value < 0) { setInputError('預測起算時刻須為非負秒數。'); return; }
            setSnapshot(value); setRevision(current => current + 1); setInputError('');
          }}><label className="grid gap-1 text-xs text-slate-400">起算時刻（相對秒）<input aria-label="預測起算秒數" type="number" min={0} step="any" className={`${researchControl} w-40`} value={snapshotDraft} onChange={event => setSnapshotDraft(event.target.value)} /></label><button type="submit" className={researchButton}>載入單次完整預測</button><button type="button" className={researchButton} onClick={() => setForecastMode('history')}>返回歷史比較</button></form>}
          <p className="text-xs text-slate-400">預測與殘差在「預測診斷」查看，亦可逐線勾選。未封存的預測明示不可用；觀察區間不改變模型評分、留出驗證或選參。</p>
        </div>
      </details>
      {inputError && <ResearchNotice error={inputError} />}
      {loading && <p role="status" aria-live="polite" className="text-sm text-indigo-300">載入完整樣本統計與顯示曲線…</p>}
      {requestError && <div className="space-y-2"><ResearchNotice error={requestError} /><p className="text-xs text-amber-200">部分比較來源未載入；不可把缺少的比較組視為已通過。</p><button className={researchButton} type="button" onClick={() => setRevision(current => current + 1)}>重新載入曲線</button></div>}
      {data && <ProcessTrendChart key={`${run.id}:${comparison}:${forecastMode}`} data={data} storageKey={`authrity.research.chart.v2.${run.id}.${comparison}.${forecastMode}`} initialView={comparison === 'advanced' ? 'lanes' : 'compare'} initialDiagnostic={forecastMode === 'snapshot'} loading={loading} fullRange={fullRange} anchorTime={anchor} onCursorChange={setCursor} onWindowChange={forecastMode === 'history' ? changeWindow : undefined} />}
      {!loading && !data && !requestError && <p role="status" className="text-sm text-amber-200">請至少選擇一種方法與結果來源。</p>}
    </div>
  </ResearchPanel>;
}
