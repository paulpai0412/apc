import { memo, useMemo, useState } from 'react';
import { Area, Brush, CartesianGrid, ComposedChart, Line, ReferenceArea, ReferenceLine, ResponsiveContainer, XAxis, YAxis } from 'recharts';
import type { ProcessTrendData, TrendRange } from '../types/ProcessTrend';

const control = 'min-w-0 max-w-full rounded border border-slate-600 bg-slate-900 px-2 py-1.5 text-xs text-slate-100';
const number = (value: number | null) => value === null || !Number.isFinite(value) ? '未提供' : value.toLocaleString('zh-TW', { maximumSignificantDigits: 5 });
const phaseNames: Record<string, string> = { train: '訓練', training: '訓練', validation: '驗證', test: '測試' };

export function ProcessTrendOverview({ data, range, cursor, timeLabel, onWindowChange }: {
  data: ProcessTrendData; range: TrendRange; cursor: number; timeLabel: (time: number, compact?: boolean) => string; onWindowChange: (range: TrendRange) => void;
}) {
  const [selected, setSelected] = useState('');
  const [draft, setDraft] = useState<{ seriesId: string; start: number; end: number } | null>(null);
  const overview = data.overview;
  const series = overview?.series.find(item => item.seriesId === selected) ?? overview?.series[0];
  const rows = useMemo(() => series?.points.map(point => ({ ...point, envelope: point.min !== null && point.max !== null ? [point.min, point.max] : null })) ?? [], [series]);
  if (!overview || !series || !rows.length) return <p className="text-xs text-slate-400">來源未提供完整期間總覽；下方仍可使用歷史曲線與時間選取。</p>;
  const first = rows.findIndex(point => point.end >= range.start);
  const last = rows.findIndex(point => point.start > range.end);
  const startIndex = draft?.seriesId === series.seriesId ? draft.start : Math.max(0, first);
  const endIndex = draft?.seriesId === series.seriesId ? draft.end : last < 0 ? rows.length - 1 : Math.max(0, last - 1);
  return <section className="min-w-0 rounded-lg border border-slate-700 bg-slate-950/40 p-3" aria-label="完整期間總覽">
    <div className="flex flex-wrap items-center justify-between gap-2"><h3 className="text-sm font-semibold">完整期間總覽</h3><label className="flex min-w-0 max-w-full items-center gap-2 text-xs">變數／組別<select className={control} aria-label="總覽變數與組別" value={series.seriesId} onChange={event => { setSelected(event.target.value); setDraft(null); }}>{overview.series.map(item => <option key={item.seriesId} value={item.seriesId}>{data.variables.find(variable => variable.id === item.variableId)?.name ?? item.variableId} · {data.groups?.find(group => group.id === item.groupId)?.label ?? item.groupId ?? '歷史'} · {item.label}</option>)}</select></label></div>
    <p className="my-2 text-xs text-slate-400">實線＝桶內中位數；陰影＝實際觀測最小～最大值，並非信賴區間。拖曳下方選框後按「套用總覽選取」；亦可使用下方開始／結束秒數鍵盤選取。縮放不改變研究評分。</p>
    <div className="h-44 min-w-0"><ResponsiveContainer width="100%" height="100%"><ComposedChart data={rows} margin={{ top: 18, right: 12, left: 0, bottom: 0 }}>
      <CartesianGrid stroke="#334155" strokeDasharray="3 3" />
      <XAxis type="number" dataKey="time" domain={[overview.range.start, overview.range.end]} allowDataOverflow tickFormatter={time => timeLabel(Number(time), true)} tick={{ fill: '#94a3b8', fontSize: 10 }} minTickGap={50} />
      <YAxis width={62} domain={['auto', 'auto']} tickFormatter={value => number(Number(value))} tick={{ fill: '#94a3b8', fontSize: 10 }} />
      {overview.splits.map((split, index) => <ReferenceArea key={`${split.name}:${split.start}`} x1={split.start} x2={split.end} fill={['#818cf8', '#34d399', '#fbbf24'][index % 3]} fillOpacity={0.09} label={{ value: phaseNames[split.name] ?? split.name, fill: '#cbd5e1', fontSize: 10, position: 'insideTop' }} />)}
      <Area type="linear" dataKey="envelope" stroke="none" fill="#38bdf8" fillOpacity={0.2} connectNulls={false} isAnimationActive={false} />
      <Line type="linear" dataKey="median" stroke="#38bdf8" strokeWidth={1.5} dot={false} connectNulls={false} isAnimationActive={false} />
      <ReferenceArea x1={range.start} x2={range.end} fill="#e2e8f0" fillOpacity={0.08} stroke="#94a3b8" />
      {cursor >= overview.range.start && cursor <= overview.range.end && <ReferenceLine x={cursor} stroke="#e2e8f0" strokeDasharray="2 3" />}
    </ComposedChart></ResponsiveContainer></div>
    <div className="h-12 min-w-0" aria-label="總覽時間選框"><ResponsiveContainer width="100%" height="100%"><ComposedChart data={rows}><Brush dataKey="time" ariaLabel="總覽時間邊界；用左右方向鍵調整，再按套用總覽選取" height={28} stroke="#818cf8" fill="#0f172a" startIndex={Math.min(startIndex, endIndex)} endIndex={Math.max(startIndex, endIndex)} tickFormatter={time => timeLabel(Number(time), true)} onChange={selection => { if (selection.startIndex !== undefined && selection.endIndex !== undefined) setDraft({ seriesId: series.seriesId, start: selection.startIndex, end: selection.endIndex }); }} /></ComposedChart></ResponsiveContainer></div>
    <button type="button" className={control} disabled={!draft || draft.seriesId !== series.seriesId} onClick={() => {
      if (!draft) return;
      const start = rows[draft.start]?.start, end = rows[draft.end]?.end;
      if (start !== undefined && end !== undefined && start < end) onWindowChange({ start: Math.max(start, overview.range.start), end: Math.min(end, overview.range.end) });
      setDraft(null);
    }}>套用總覽選取</button>
  </section>;
}

export const ProcessTrendSummary = memo(function ProcessTrendSummary({ data, range, variables }: { data: ProcessTrendData; range: TrendRange; variables: string[] }) {
  const summary = data.windowSummary;
  if (!summary || summary.range.start !== range.start || summary.range.end !== range.end) return <p role="status" className="rounded border border-slate-700 p-3 text-xs text-slate-400">選定視窗完整解析度統計：{summary ? '所需視窗尚未載入，等待符合區間的資料。' : '來源未提供／尚未載入。'} 不使用減點曲線推算正式統計。</p>;
  const selectedVariables = data.variables.filter(variable => variables.includes(variable.id));
  const groupLabel = (id?: string) => data.groups?.find(group => group.id === id)?.label ?? id ?? '歷史';
  const card = (variable: ProcessTrendData['variables'][number]) => {
    const rows = summary.rows.filter(row => row.variableId === variable.id);
    const unit = variable.unit || '單位未知';
    if (!rows.length) return null;
    return <article key={variable.id} className="min-w-0 space-y-2 rounded border border-slate-700 bg-slate-900/50 p-2 text-xs">
      <h4 className="break-words font-semibold">{variable.role} · {variable.name} ({unit})</h4>
      {variable.role === 'MV' ? <div className="grid grid-cols-3 gap-x-2 gap-y-1 break-words">
        <span className="text-slate-400">組別</span><span className="text-slate-400">命令 Σ|ΔMV|</span><span className="text-slate-400">良好相鄰配對</span>
        {rows.map(row => <div key={row.seriesId} className="contents"><span>{groupLabel(row.groupId)}</span><span>{number(row.commandMovement)}</span><span>{row.commandPairs.toLocaleString()}</span></div>)}
      </div> : <>
        <div className={`grid ${variable.role === 'CV' ? 'grid-cols-4' : 'grid-cols-3'} gap-x-2 gap-y-1 break-words`}>
          <span className="text-slate-400">組別</span>{variable.role === 'CV' ? <><span className="text-slate-400">物理變異數 ({unit})²</span><span className="text-slate-400">SD</span><span className="text-slate-400">偏差（實際−目標）</span></> : <><span className="text-slate-400">平均</span><span className="text-slate-400">SD</span></>}
          {rows.map(row => <div key={row.seriesId} className="contents"><span>{groupLabel(row.groupId)}</span>{variable.role === 'CV' ? <><span>{number(row.variance)}</span><span>{number(row.stddev)}</span><span>{number(row.bias)}</span></> : <><span>{number(row.mean)}</span><span>{number(row.stddev)}</span></>}</div>)}
        </div>
        {variable.role === 'CV' && <div className="grid grid-cols-3 gap-x-2 gap-y-1 border-t border-slate-800 pt-2 break-words">
          <span className="text-slate-400">違規／涵蓋秒</span><span className="text-slate-400">品質規格</span><span className="text-slate-400">操作限制</span>
          {rows.map(row => <div key={row.seriesId} className="contents"><span>{groupLabel(row.groupId)}</span><span>{row.specViolationSeconds === null ? '無可比較資料' : `${number(row.specViolationSeconds)} / ${number(row.specObservedSeconds)}`}</span><span>{row.operatingViolationSeconds === null ? '無可比較資料' : `${number(row.operatingViolationSeconds)} / ${number(row.operatingObservedSeconds)}`}</span></div>)}
        </div>}
      </>}
    </article>;
  };
  return <section className="min-w-0 space-y-2" aria-label="選定視窗完整解析度統計">
    <h3 className="text-sm font-medium">選定視窗 · 完整解析度統計</h3>
    <div className="grid min-w-0 gap-2 lg:grid-cols-2">{selectedVariables.filter(variable => variable.role === 'CV' || variable.role === 'MV').map(card)}</div>
    {selectedVariables.some(variable => variable.role !== 'CV' && variable.role !== 'MV') && <details className="text-xs text-slate-400"><summary className="cursor-pointer">擾動／其他變數的平均與標準差</summary><div className="mt-2 grid min-w-0 gap-2 lg:grid-cols-2">{selectedVariables.filter(variable => variable.role !== 'CV' && variable.role !== 'MV').map(card)}</div></details>}
    <details className="text-xs text-slate-400"><summary className="cursor-pointer">樣本數、涵蓋時長與計算範圍</summary>
      <p className="mt-2">違規／涵蓋秒數＝可比較的良好樣本數 × 掃描週期（含末筆），不是連續插值時長。命令移動僅累計此視窗內相隔一個掃描週期且品質良好的相鄰命令；不等於能源。</p>
      <ul className="my-2 space-y-1">{summary.rows.filter(row => variables.includes(row.variableId)).map(row => <li key={row.seriesId}>{data.variables.find(variable => variable.id === row.variableId)?.name} · {groupLabel(row.groupId)}：原始 {row.samples.toLocaleString()}／良好 {row.goodSamples.toLocaleString()}；偏差配對 {row.biasSamples.toLocaleString()}；命令配對 {row.commandPairs.toLocaleString()}</li>)}</ul>
      <p className="break-words">{summary.basis}</p>
    </details>
  </section>;
});
