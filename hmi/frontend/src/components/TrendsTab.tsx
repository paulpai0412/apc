import { useEffect, useMemo, useState } from 'react';
import { apiFetch } from '../lib/api';
import { historianTrendData, isDiagnosticField, isLimitField } from '../lib/historianTrends';
import type { HistoryEnvelope, HistoryField } from '../lib/historianTrends';
import { ProcessTrendChart } from './ProcessTrendChart';
import { runtimeNodeId } from '../lib/modelBinding';
import type { RuntimeVariable } from '../lib/modelBinding';

interface TrendsTabProps { cvList: RuntimeVariable[]; mvList: RuntimeVariable[]; dvList: RuntimeVariable[] }
type Role = HistoryField['role'];
type FieldPreferences = Pick<HistoryField, 'alias' | 'unit' | 'enabled' | 'pinned'>;
const storageKey = 'authrity.trends.independent.v1';
const control = 'rounded border border-slate-700 bg-slate-950 px-3 py-2 text-sm text-slate-200';
const button = 'rounded border border-slate-700 bg-slate-800 px-3 py-2 text-sm text-slate-200 hover:border-indigo-400 disabled:opacity-40';
const durations: Record<string, number> = { '15m': 900000, '1h': 3600000, '4h': 14400000, '12h': 43200000, '24h': 86400000 };
const emptyHistory: HistoryEnvelope = { points: [], rawSamples: 0, gapThresholdMs: 180000, warnings: [] };
const labels: Record<string, string> = { PV: '實際值', Target: '目標', Prediction: '預測', SP: '已下達命令', OP: 'PID 輸出（獨立軸）', FuturePlan: '未來計畫', Mode: '模式', ModeTarget: '目標模式', LowLimit: '操作下限', HighLimit: '操作上限', SpecLow: '品質規格下限', SpecHigh: '品質規格上限', Error: '追蹤誤差', ConstraintDistance: '操作界限距離', SaturationFlag: '命令接近限制' };
const fieldsByRole: Record<Role, string[]> = {
  CV: ['PV', 'Target', 'Prediction', 'LowLimit', 'HighLimit', 'Error', 'ConstraintDistance'],
  MV: ['PV', 'SP', 'AppliedSP', 'OP', 'FuturePlan', 'Mode', 'ModeTarget', 'LowLimit', 'HighLimit', 'SaturationFlag'],
  DV: ['PV', 'LowLimit', 'HighLimit'],
};
function loadPreferences(): Record<string, FieldPreferences> {
  try { const value = JSON.parse(localStorage.getItem(storageKey) || '{}'); return value && typeof value === 'object' && !Array.isArray(value) ? value : {}; }
  catch { return {}; }
}
const isoInput = (time: number) => new Date(time).toISOString().slice(0, 19);
const parseUtc = (value: string) => Date.parse(value.endsWith('Z') ? value : `${value}Z`);

export function TrendsTab({ cvList, mvList, dvList }: TrendsTabProps) {
  const [preferences, setPreferences] = useState(loadPreferences);
  const [duration, setDuration] = useState('1h');
  const [range, setRange] = useState(() => { const end = Date.now(); return { start: end - durations['1h'], end }; });
  const [chartRange, setChartRange] = useState<{ start: number; end: number } | null>(null);
  const effectiveRange = chartRange ?? range;
  const [draft, setDraft] = useState(() => ({ start: isoInput(range.start), end: isoInput(range.end) }));
  const [previousRange, setPreviousRange] = useState(range);
  if (previousRange !== range) {
    setPreviousRange(range);
    setDraft({ start: isoInput(range.start), end: isoInput(range.end) });
    setChartRange(null);
  }
  const [live, setLive] = useState(true);
  const [revision, setRevision] = useState(0);
  const [sampling, setSampling] = useState<'extrema' | 'all'>('extrema');
  const [budget, setBudget] = useState(1200);
  const [result, setResult] = useState<{ key: string; history: HistoryEnvelope; error: string } | null>(null);
  const [inputError, setInputError] = useState<{ key: string; message: string } | null>(null);
  const [roleFilter, setRoleFilter] = useState<'ALL' | Role>('ALL');
  const [view, setView] = useState<'core' | 'diagnostics' | 'limits' | 'all'>('core');
  const [search, setSearch] = useState('');
  const [page, setPage] = useState(0);
  const fields = useMemo(() => [...cvList, ...mvList, ...dvList].flatMap(variable =>
    fieldsByRole[variable.role].flatMap(field => {
      const key = isDiagnosticField(field) ? JSON.stringify(['derived', variable.role, variable.name, field]) : runtimeNodeId(variable, field);
      if (!key) return [];
      const saved = preferences[key];
      return [{ key, base: variable.name, field, role: variable.role,
        alias: typeof saved?.alias === 'string' ? saved.alias : `${variable.description || variable.name}${field === 'PV' ? '' : ` · ${labels[field] || field}`}`,
        unit: field === 'OP' ? '' : ['Mode', 'ModeTarget', 'SaturationFlag'].includes(field) ? 'code' : variable.units,
        pinned: saved?.pinned === true,
        enabled: typeof saved?.enabled === 'boolean' ? saved.enabled : ['PV', 'SP', 'AppliedSP', 'Target', 'LowLimit', 'HighLimit'].includes(field) } satisfies HistoryField];
    })), [cvList, mvList, dvList, preferences]);
  const enabled = fields.filter(field => field.enabled);
  const queryNodes = useMemo(() => {
    const tags = new Set<string>();
    for (const field of fields.filter(field => field.enabled)) {
      if (!isDiagnosticField(field.field)) tags.add(field.key);
      else {
        const names = field.field === 'Error' ? ['PV', 'Target'] : [field.field === 'SaturationFlag' ? 'SP' : 'PV', 'LowLimit', 'HighLimit'];
        for (const name of names) {
          const source = fields.find(candidate => candidate.base === field.base && candidate.role === field.role && candidate.field === name);
          if (source) tags.add(source.key);
        }
      }
    }
    return [...tags].sort();
  }, [fields]);
  const settingNodes = useMemo(() => fields.filter(field => queryNodes.includes(field.key)
    && (isLimitField(field.field) || ['Target', 'SP', 'AppliedSP', 'Mode', 'ModeTarget'].includes(field.field))).map(field => field.key), [fields, queryNodes]);
  const sourceKey = JSON.stringify([...cvList, ...mvList, ...dvList].map(variable => [variable.role, variable.name, variable.units, variable.nodes]));
  const query = new URLSearchParams({ nodes: JSON.stringify(queryNodes), settings: JSON.stringify(settingNodes), start: new Date(effectiveRange.start).toISOString(), end: new Date(effectiveRange.end).toISOString() });
  if (sampling === 'extrema') query.set('max_points', String(budget));
  const requestUrl = queryNodes.length ? `/api/trends?${query}` : null;
  const requestKey = JSON.stringify([sourceKey, requestUrl, revision]);
  const currentResult = result?.key === requestKey ? result : null;
  const history = currentResult?.history ?? emptyHistory;
  const loading = requestUrl !== null && currentResult === null;
  const error = inputError?.key === requestKey ? inputError.message : currentResult?.error ?? '';
  const chart = useMemo(() => ({ ...historianTrendData(history, fields, effectiveRange), originTime: range.start }), [history, fields, effectiveRange, range.start]);
  const visibleRows = useMemo(() => fields.filter(field => (roleFilter === 'ALL' || field.role === roleFilter)
    && (view === 'all' || (view === 'limits' ? isLimitField(field.field) : view === 'diagnostics' ? isDiagnosticField(field.field) : !isLimitField(field.field) && !isDiagnosticField(field.field)))
    && `${field.key} ${field.alias}`.toLowerCase().includes(search.toLowerCase())).sort((a,b) => Number(b.pinned)-Number(a.pinned) || a.key.localeCompare(b.key)), [fields, roleFilter, view, search]);
  const pageCount = Math.max(1, Math.ceil(visibleRows.length / 40));
  const visiblePage = Math.min(page, pageCount - 1);

  useEffect(() => { try { localStorage.setItem(storageKey, JSON.stringify(preferences)); } catch { /* Display settings remain usable without storage. */ } }, [preferences]);
  useEffect(() => {
    if (!live) return;
    const timer = window.setInterval(() => { const end = Date.now(); setRange({ start: end - durations[duration], end }); }, 30000);
    return () => window.clearInterval(timer);
  }, [live, duration]);
  useEffect(() => {
    if (!requestUrl) return;
    const controller = new AbortController();
    void apiFetch(requestUrl, { signal: controller.signal }).then(async response => {
      if (!response.ok) throw new Error(response.status === 401 ? '登入已過期，請重新登入。' : await response.text());
      const data: HistoryEnvelope = await response.json();
      if (!Array.isArray(data.points) || data.points.some(point => typeof point.node_id !== 'string')) throw new Error('歷史資料缺少明確的 node_id。');
      if (!controller.signal.aborted) setResult({ key: requestKey, history: data, error: '' });
    }).catch((cause: unknown) => {
      if (!controller.signal.aborted) {
        setResult({ key: requestKey, history: emptyHistory, error: cause instanceof Error ? cause.message : '歷史查詢失敗。' });
      }
    });
    return () => controller.abort();
  }, [requestUrl, requestKey]);

  function update(field: HistoryField, patch: Partial<FieldPreferences>) {
    setPreferences(previous => ({ ...previous, [field.key]: { alias: field.alias, unit: field.unit, enabled: field.enabled, pinned: field.pinned, ...patch } }));
  }
  function preset(kind: 'ALL' | 'NONE' | 'APC' | Role) {
    setPreferences(previous => ({ ...previous, ...Object.fromEntries(fields.map(field => [field.key, {
      alias: field.alias, unit: field.unit, pinned: field.pinned,
      enabled: kind === 'ALL' || (kind !== 'NONE' && ['PV', 'SP', 'Target', 'LowLimit', 'HighLimit'].includes(field.field) && (kind === 'APC' || field.role === kind)),
    }])) }));
  }
  function applyWindow(next: { start: number; end: number }) {
    if (!Number.isFinite(next.start) || !Number.isFinite(next.end) || next.start >= next.end) { setInputError({ key: requestKey, message: '結束時間必須晚於開始時間，請使用有效的 UTC 日期。' }); return; }
    setInputError(null); setLive(false); setRange(next);
  }
  function shift(direction: number) { const delta = (range.end - range.start) / 10 * direction; applyWindow({ start: range.start + delta, end: range.end + delta }); }

  return <div className="min-w-0 space-y-4">
    <section className="space-y-3 rounded-xl border border-slate-800 bg-slate-900/50 p-4">
      <div className="flex flex-wrap items-center gap-2"><h2 className="mr-3 font-medium text-slate-200">多變數同步趨勢</h2>{Object.keys(durations).map(key => <button key={key} className={button} aria-pressed={duration === key} onClick={() => { setDuration(key); const end = live ? Date.now() : range.end; setRange({ start: end - durations[key], end }); }}>{key}</button>)}
        <button className={button} onClick={() => shift(-1)}>往前</button><button className={button} onClick={() => shift(1)}>往後</button>
        <button className={button} aria-pressed={live} onClick={() => { const end = Date.now(); setRange({ start: end-durations[duration], end }); setLive(true); }}>即時追蹤</button>
        <button className={button} disabled={loading} onClick={() => setRevision(value => value + 1)}>重新查詢</button>
      </div>
      <form className="flex flex-wrap items-end gap-3" onSubmit={event => { event.preventDefault(); applyWindow({ start: parseUtc(draft.start), end: parseUtc(draft.end) }); }}>
        <label className="grid gap-1 text-xs text-slate-400">開始時間（UTC）<input className={control} type="datetime-local" step="1" value={draft.start} onChange={event => setDraft(previous => ({ ...previous, start: event.target.value }))} required /></label>
        <label className="grid gap-1 text-xs text-slate-400">結束時間（UTC）<input className={control} type="datetime-local" step="1" value={draft.end} onChange={event => setDraft(previous => ({ ...previous, end: event.target.value }))} required /></label>
        <button className={button} type="submit">套用時間窗</button>
        <label className="grid gap-1 text-xs text-slate-400">顯示取樣<select className={control} value={sampling} onChange={event => setSampling(event.target.value as typeof sampling)}><option value="extrema">保留尖峰、缺口與設定變更</option><option value="all">全部原始紀錄</option></select></label>
        {sampling === 'extrema' && <label className="grid gap-1 text-xs text-slate-400">每訊號目標點數<input className={`${control} w-28`} type="number" min={100} max={10000} step={100} value={budget} onChange={event => setBudget(Math.max(100, Math.min(10000, Number(event.target.value) || 1200)))} /></label>}
      </form>
      <p className="text-xs text-slate-400">{live ? '每 30 秒更新' : '已暫停即時追蹤'} · {enabled.length} 個訊號 · {history.rawSamples.toLocaleString()} 筆原始紀錄。設定變更與品質轉換不受目標點數裁切。不平均、不延續填補實際量測。</p>
      {loading && <p role="status" className="text-sm text-indigo-300">讀取已記錄的歷史資料…</p>}
      {error && <p role="alert" className="break-words text-sm text-rose-300">{error}</p>}
    </section>
    {queryNodes.length > 0 ? <ProcessTrendChart data={chart} storageKey={`authrity.historian-chart.${JSON.stringify([...cvList, ...mvList, ...dvList].map(variable => [variable.role, variable.name, variable.nodes]))}`} initialSelected={chart.variables.map(variable => variable.id)} fullRange={range} onWindowChange={next => { setLive(false); setChartRange(next); }} /> : <p className="rounded border border-slate-700 p-5 text-slate-400">請在下方選擇要觀察的訊號。</p>}
    <section className="min-w-0 rounded-xl border border-slate-800 bg-slate-900/50 p-4">
      <h2 className="font-medium text-slate-200">訊號設定</h2><p className="my-2 text-xs text-slate-400">工程單位來自模型，不換算數值；每個變數獨立軸，OP 不套用 PV 單位。未設定 applied_sp 時，SP 僅是命令暫存器回讀，並非已套用回授。模型未提供品質規格，不把操作限制冒稱品質規格。</p>
      <div className="mb-3 flex flex-wrap items-center gap-2">{(['APC', 'CV', 'MV', 'DV', 'ALL', 'NONE'] as const).map(kind => <button className={button} key={kind} onClick={() => preset(kind)}>{kind === 'ALL' ? '全選' : kind === 'NONE' ? '清除' : kind}</button>)}
        <label className="text-xs text-slate-400">類型 <select className={control} value={roleFilter} onChange={event => { setRoleFilter(event.target.value as typeof roleFilter); setPage(0); }}>{['ALL', 'CV', 'MV', 'DV'].map(role => <option key={role}>{role}</option>)}</select></label>
        <label className="text-xs text-slate-400">訊號種類 <select className={control} value={view} onChange={event => { setView(event.target.value as typeof view); setPage(0); }}><option value="core">主要訊號</option><option value="limits">限制與規格</option><option value="diagnostics">診斷值</option><option value="all">全部</option></select></label>
        <input className={`${control} max-w-full`} aria-label="搜尋訊號或名稱" placeholder="搜尋訊號或名稱" value={search} onChange={event => { setSearch(event.target.value); setPage(0); }} />
      </div>
      <div className="overflow-x-auto"><table className="w-full text-left text-sm"><caption className="sr-only">歷史訊號與工程單位設定</caption><thead className="text-slate-400"><tr><th className="p-2">啟用</th><th className="p-2">OPC 訊號</th><th className="p-2">顯示名稱</th><th className="p-2">工程單位</th><th className="p-2">優先排列</th></tr></thead><tbody>{visibleRows.slice(visiblePage*40,(visiblePage+1)*40).map(field => <tr className="border-t border-slate-800" key={field.key}>
        <td className="p-2"><input type="checkbox" aria-label={`啟用 ${field.key}`} checked={field.enabled} onChange={event => update(field, { enabled: event.target.checked })} /></td>
        <td className="whitespace-nowrap p-2"><span className="mr-2 text-xs text-indigo-300">{field.role}</span>{field.key}</td>
        <td className="p-2"><input className={`${control} w-56`} aria-label={`${field.key} 顯示名稱`} value={field.alias} onChange={event => update(field, { alias: event.target.value })} /></td>
        <td className="p-2"><span aria-label={`${field.key} 工程單位`}>{field.unit || '未提供／未知'}</span></td>
        <td className="p-2"><input type="checkbox" aria-label={`優先排列 ${field.key}`} checked={field.pinned} onChange={event => update(field, { pinned: event.target.checked })} /></td>
      </tr>)}</tbody></table></div>
      <div className="mt-3 flex flex-wrap items-center gap-3 text-xs text-slate-400"><span>{visibleRows.length} 個訊號 · 第 {visiblePage + 1} / {pageCount} 頁</span><button className={button} disabled={visiblePage === 0} onClick={() => setPage(visiblePage - 1)}>上一頁</button><button className={button} disabled={visiblePage + 1 >= pageCount} onClick={() => setPage(visiblePage + 1)}>下一頁</button></div>
    </section>
  </div>;
}
