import { useState } from 'react';
import type { FormEvent } from 'react';
import { Brush, CartesianGrid, Legend, Line, LineChart, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts';
import { researchError, researchRequest } from '../lib/research';
import type { Campaign, Cleaning, CleaningPreview, Dataset, SignalMapping } from '../types/Research';
import { ResearchDetails, ResearchField, ResearchNotice, ResearchNumber, ResearchPanel, researchButton, researchControl, researchPrimary } from './ResearchControls';

export function ResearchCleaning({ dataset, campaign, onClean }: { dataset: Dataset; campaign: Campaign; onClean: (dataset: Dataset) => void }) {
  const [cleaning, setCleaning] = useState<Cleaning>(() => {
    const start = new Date(campaign.start);
    const boundary = (months: number) => {
      const value = new Date(start);
      const day = start.getUTCDate();
      value.setUTCDate(1);
      value.setUTCMonth(start.getUTCMonth() + months);
      const last = new Date(Date.UTC(value.getUTCFullYear(), value.getUTCMonth() + 1, 0)).getUTCDate();
      value.setUTCDate(Math.min(day, last));
      return (value.getTime() - start.getTime()) / 1000;
    };
    const window = campaign.data_window;
    return {
      time_column: dataset.source?.kind === 'postgres' ? dataset.source.timestamp_column : dataset.columns.find(column => /^(time_seconds|timestamp|time|date)$/i.test(column)) || dataset.columns[0] || '',
      time_format: dataset.source?.kind === 'postgres' ? 'iso' : dataset.columns.includes('time_seconds') ? 'seconds' : 'iso', sample_time: dataset.sample_time || campaign.sample_time,
      train_end: typeof window?.train_end === 'number' ? window.train_end * campaign.sample_time : boundary(1),
      validation_end: typeof window?.validation_end === 'number' ? window.validation_end * campaign.sample_time : boundary(2),
      duplicate_policy: 'last', missing_policy: 'reject', max_gap_seconds: campaign.sample_time * 3, outlier_policy: 'none', sigma: 4,
      signals: (['cv', 'mv', 'dv'] as const).flatMap(role => campaign.variables[role].map((variable, index) => ({
        column: dataset.columns.includes(variable.name) ? variable.name : dataset.suggested_signals?.filter(item => item.role === role)[index]?.column || (dataset.columns.includes(`${role}_${index}`) ? `${role}_${index}` : ''),
        name: variable.name, role, unit: variable.unit, scale: 1, offset: 0,
      }))),
    };
  });
  const [preview, setPreview] = useState<CleaningPreview | null>(null);
  const [signal, setSignal] = useState('');
  const [focusChange, setFocusChange] = useState(false);
  const [busy, setBusy] = useState('');
  const [error, setError] = useState('');
  const [savedCleaning, setSavedCleaning] = useState('');
  const [selectedRange, setSelectedRange] = useState<{ start: number; end: number } | null>(null);
  const update = (patch: Partial<Cleaning>) => { setCleaning(current => ({ ...current, ...patch })); setPreview(null); setSelectedRange(null); };
  const updateSignal = (index: number, patch: Partial<SignalMapping>) => update({ signals: cleaning.signals.map((value, i) => i === index ? { ...value, ...patch } : value) });
  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const operation = (event.nativeEvent as SubmitEvent).submitter?.getAttribute('value') === 'clean' ? 'clean' : 'preview';
    setError('');
    if (cleaning.train_end >= cleaning.validation_end) { setError('驗證結束時間必須晚於訓練結束時間。'); return; }
    if (!cleaning.signals.some(item => item.role === 'cv') || !cleaning.signals.some(item => item.role === 'mv')) { setError('至少需要一個 CV 與一個 MV 對應。'); return; }
    if (new Set(cleaning.signals.map(item => item.name)).size !== cleaning.signals.length) { setError('訊號名稱不可重複。'); return; }
    if (cleaning.signals.some(item => item.min !== undefined && item.max !== undefined && item.min >= item.max)) { setError('訊號下限必須小於上限。'); return; }
    if (cleaning.regimes?.some(item => !Number.isFinite(item.start) || !Number.isFinite(item.end) || item.start < 0 || item.end <= item.start || !item.label.trim() || (item.exclude && !item.reason.trim()))) { setError('工況區間需要有效的半開起訖、標籤；排除必須填寫原因。'); return; }
    setBusy(operation);
    try {
      const result = await researchRequest<CleaningPreview>(`/${operation}`, { dataset_id: dataset.id, cleaning });
      setPreview(result);
      setSignal(current => result.signals.some(item => item.name === current) ? current : result.signals[0]?.name || '');
      if (result.dataset) { setSavedCleaning(JSON.stringify(cleaning)); onClean(result.dataset); }
    } catch (failure) { setError(researchError(failure)); } finally { setBusy(''); }
  }
  const allPoints = preview?.series.map(point => ({ time: point.time, raw: point.raw[signal], clean: point.clean[signal] })) || [];
  const firstChange = allPoints.find(point => point.raw !== point.clean);
  const points = focusChange && firstChange
    ? allPoints.filter(point => Math.abs(point.time - firstChange.time) <= cleaning.sample_time * 10)
    : allPoints;
  const selectedStats = Array.isArray(preview?.summary.signal_stats)
    ? preview.summary.signal_stats.find(item => item && typeof item === 'object' && !Array.isArray(item) && item.name === signal)
    : undefined;
  const counts = selectedStats && typeof selectedStats === 'object' && !Array.isArray(selectedStats) ? selectedStats : {};
  return <ResearchPanel title="清洗規則、工況遮罩與前後比較">
    <p className="mb-4 text-sm text-slate-400">原始 bytes 保留。切分終點不包含在前一區間；預覽為顯示取樣，正式清洗使用全部資料。{savedCleaning === JSON.stringify(cleaning) ? '目前規則已凍結。' : '清洗草稿尚未儲存；換頁保留。'}</p>
    <form onSubmit={event => void submit(event)}>
      <fieldset disabled={!!busy} className="min-w-0 space-y-5">
        <legend className="mb-3 text-sm text-indigo-300">{dataset.name} · {dataset.row_count.toLocaleString()} 筆</legend>
        <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
          <ResearchField label="時間欄位"><select required className={researchControl} value={cleaning.time_column} onChange={event => update({ time_column: event.target.value })}><option value="">選擇欄位</option>{dataset.columns.map(column => <option key={column}>{column}</option>)}</select></ResearchField>
          <ResearchField label="時間格式"><select className={researchControl} value={cleaning.time_format} onChange={event => update({ time_format: event.target.value as Cleaning['time_format'] })}><option value="iso">ISO 日期時間</option><option value="seconds">相對秒數</option></select></ResearchField>
          <ResearchNumber label="重採樣間隔 (秒)" value={cleaning.sample_time} min={0.001} onChange={value => update({ sample_time: value ?? NaN })} />
          <ResearchNumber label="最大可填補間隔 (秒)" value={cleaning.max_gap_seconds} min={0} onChange={value => update({ max_gap_seconds: value ?? NaN })} />
          <ResearchNumber label="訓練結束 (秒，不含)" value={cleaning.train_end} min={0.001} onChange={value => update({ train_end: value ?? NaN })} />
          <ResearchNumber label="驗證結束 (秒，不含)" value={cleaning.validation_end} min={0.001} onChange={value => update({ validation_end: value ?? NaN })} />
          <ResearchField label="重複時間"><select className={researchControl} value={cleaning.duplicate_policy} onChange={event => update({ duplicate_policy: event.target.value as Cleaning['duplicate_policy'] })}><option value="first">保留第一筆</option><option value="last">保留最後一筆</option><option value="mean">平均值</option></select></ResearchField>
          <ResearchField label="缺值處理"><select className={researchControl} value={cleaning.missing_policy} onChange={event => update({ missing_policy: event.target.value as Cleaning['missing_policy'] })}><option value="reject">保留為無效（不填補）</option><option value="forward_fill">向前延續（因果）</option><option value="linear">區間內線性插值</option></select></ResearchField>
          <ResearchField label="離群值處理"><select className={researchControl} value={cleaning.outlier_policy} onChange={event => update({ outlier_policy: event.target.value as Cleaning['outlier_policy'] })}><option value="none">不處理</option><option value="bounds">依工程上下限</option><option value="train_sigma">依訓練資料標準差</option></select></ResearchField>
          {cleaning.outlier_policy === 'train_sigma' && <ResearchNumber label="標準差倍數" value={cleaning.sigma} min={0.001} onChange={value => update({ sigma: value ?? NaN })} />}
        </div>
        <p className="text-xs text-slate-400">品質欄位需指定編碼，僅保留 Good 資料。填補不跨訓練／驗證／測試邊界；過長缺口保留無效。工程值 = 原始值 × scale + offset。</p>
        <div className="space-y-3">{cleaning.signals.map((mapping, index) => <fieldset key={index} className="min-w-0 rounded-lg border border-slate-700 p-4"><legend className="px-2 text-sm text-indigo-300">訊號 {index + 1} · {mapping.name}</legend><div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
          <ResearchField label={`訊號 ${index + 1} 原始欄位`}><select required className={researchControl} value={mapping.column} onChange={event => updateSignal(index, { column: event.target.value })}><option value="">選擇資料欄位</option>{dataset.columns.map(column => <option key={column}>{column}</option>)}</select></ResearchField>
          <ResearchField label={`訊號 ${index + 1} 名稱`}><input required className={researchControl} value={mapping.name} onChange={event => updateSignal(index, { name: event.target.value })} /></ResearchField>
          <ResearchField label={`訊號 ${index + 1} 角色`}><select className={researchControl} value={mapping.role} onChange={event => updateSignal(index, { role: event.target.value as SignalMapping['role'] })}><option value="cv">CV · 受控變數</option><option value="mv">MV · 操控變數</option><option value="dv">DV · 擾動變數</option></select></ResearchField>
          <ResearchField label={`訊號 ${index + 1} 單位`}><input className={researchControl} value={mapping.unit} onChange={event => updateSignal(index, { unit: event.target.value })} /></ResearchField>
          <ResearchNumber label={`訊號 ${index + 1} scale`} value={mapping.scale} onChange={value => updateSignal(index, { scale: value ?? NaN })} />
          <ResearchNumber label={`訊號 ${index + 1} offset`} value={mapping.offset} onChange={value => updateSignal(index, { offset: value ?? NaN })} />
          <ResearchNumber label={`訊號 ${index + 1} 下限（選填）`} required={false} value={mapping.min} onChange={value => updateSignal(index, { min: value })} />
          <ResearchNumber label={`訊號 ${index + 1} 上限（選填）`} required={false} value={mapping.max} onChange={value => updateSignal(index, { max: value })} />
          <ResearchField label={`訊號 ${index + 1} 品質欄位（選填）`}><select className={researchControl} value={mapping.quality_column || ''} onChange={event => updateSignal(index, { quality_column: event.target.value || undefined })}><option value="">未指定</option>{dataset.columns.map(column => <option key={column}>{column}</option>)}</select></ResearchField>
          {mapping.quality_column && <ResearchField label={`訊號 ${index + 1} 品質編碼`}><select className={researchControl} value={mapping.quality_format || 'da'} onChange={event => updateSignal(index, { quality_format: event.target.value as SignalMapping['quality_format'] })}><option value="da">OPC DA · 192 為 Good</option><option value="ua">OPC UA · 0 為 Good</option><option value="boolean">布林 · true／1 為 Good</option></select></ResearchField>}
        </div><button type="button" className={`${researchButton} mt-3`} onClick={() => update({ signals: cleaning.signals.filter((_, i) => i !== index) })}>移除訊號 {index + 1}</button></fieldset>)}</div>
        <button type="button" className={researchButton} onClick={() => update({ signals: [...cleaning.signals, { column: '', name: '', role: 'cv', unit: '', scale: 1, offset: 0 }] })}>新增訊號對應</button>
        <fieldset className="space-y-3"><legend className="font-medium">工況標籤與排除區間（相對秒數）</legend>
          {(cleaning.regimes || []).map((regime, index) => <div key={index} className="grid gap-3 rounded border border-slate-700 p-3 sm:grid-cols-2">
            <ResearchNumber label={`區間 ${index + 1} 開始（含）`} value={regime.start} min={0} onChange={value => update({ regimes: cleaning.regimes?.map((item, i) => i === index ? { ...item, start: value ?? NaN } : item) })} />
            <ResearchNumber label={`區間 ${index + 1} 結束（不含）`} value={regime.end} min={0} onChange={value => update({ regimes: cleaning.regimes?.map((item, i) => i === index ? { ...item, end: value ?? NaN } : item) })} />
            <ResearchField label={`區間 ${index + 1} 工況／產品標籤`}><input required className={researchControl} value={regime.label} onChange={event => update({ regimes: cleaning.regimes?.map((item, i) => i === index ? { ...item, label: event.target.value } : item) })} /></ResearchField>
            <ResearchField label={`區間 ${index + 1} 原因`}><input required={regime.exclude} className={researchControl} value={regime.reason} onChange={event => update({ regimes: cleaning.regimes?.map((item, i) => i === index ? { ...item, reason: event.target.value } : item) })} /></ResearchField>
            <label className="flex gap-2 text-sm"><input type="checkbox" checked={regime.exclude} onChange={event => update({ regimes: cleaning.regimes?.map((item, i) => i === index ? { ...item, exclude: event.target.checked } : item) })} />排除且保留缺口（不得填補）</label>
            <button type="button" className={researchButton} onClick={() => update({ regimes: cleaning.regimes?.filter((_, i) => i !== index) })}>移除區間 {index + 1}</button>
          </div>)}
          <button type="button" className={researchButton} disabled={(cleaning.regimes?.length || 0) >= 128} onClick={() => update({ regimes: [...(cleaning.regimes || []), { start: 0, end: cleaning.sample_time, label: '', exclude: false, reason: '' }] })}>新增工況區間</button>
        </fieldset>
        <div className="flex flex-wrap gap-3"><button type="submit" value="preview" className={researchButton}>{busy === 'preview' ? '正在預覽…' : '預覽清理結果'}</button><button type="submit" value="clean" disabled={!preview || !preview.usable} className={researchPrimary}>{busy === 'clean' ? '正在儲存…' : '儲存清理資料並用於研究'}</button></div>
      </fieldset>
    </form>
    {error && <div className="mt-4"><ResearchNotice error={error} /></div>}
    {preview && <div className="mt-6 space-y-4">
      <p role="status" className={preview.usable ? 'text-emerald-300' : 'text-amber-300'}>{preview.usable ? '資料可用。確認曲線與摘要後，儲存清理版本。' : '仍有無效資料，尚不能用於研究。請調整清理設定。'}</p>
      {preview.exposure && <p className="rounded border border-amber-800 p-3 text-sm text-amber-200">留出資格：{preview.exposure.independent ? '伺服器聲明獨立' : '探索性／未驗證獨立性'} · {preview.exposure.status} · {preview.exposure.reason}</p>}
      {!!preview.warnings.length && <details><summary className="cursor-pointer text-sm text-amber-200">檢視 {preview.warnings.length} 項清洗提醒與技術細節</summary><ul className="mt-2 list-disc space-y-1 pl-5 text-sm text-amber-200">{preview.warnings.map((warning, index) => <li key={index}>{warning}</li>)}</ul></details>}
      <ResearchField label="清理前後比較訊號"><select className={researchControl} value={signal} onChange={event => setSignal(event.target.value)}>{preview.signals.map(item => <option key={item.name} value={item.name}>{item.name} ({item.unit})</option>)}</select></ResearchField>
      <dl className="grid grid-cols-2 gap-3 sm:grid-cols-4">{[
        ['無效品質', counts.bad_quality_count], ['離群值', counts.outlier_count],
        ['已填補', counts.filled_count], ['未解決缺口', counts.unresolved_count],
      ].map(([label, value]) => <div key={String(label)} className="rounded-lg border border-slate-700 p-3"><dt className="text-xs text-slate-400">{String(label)}</dt><dd className="mt-1 text-xl text-slate-100">{typeof value === 'number' ? value.toLocaleString() : '—'}</dd></div>)}</dl>
      <div className="flex flex-wrap gap-3"><button type="button" className={researchButton} aria-pressed={!focusChange} onClick={() => setFocusChange(false)}>完整期間</button><button type="button" className={researchButton} disabled={!firstChange} aria-pressed={focusChange} onClick={() => setFocusChange(true)}>聚焦第一處清洗差異</button></div>
      {points.length ? <div className="h-80 min-w-0" role="img" aria-label={`${signal} 清理前後曲線，橫軸為相對天數；可拖選區間`}><ResponsiveContainer width="100%" height="100%"><LineChart data={points}><CartesianGrid stroke="#334155" strokeDasharray="3 3" /><XAxis dataKey="time" type="number" domain={['dataMin', 'dataMax']} tickFormatter={value => `${(Number(value) / 86400).toFixed(focusChange ? 3 : 1)} 天`} tick={{ fill: '#94a3b8', fontSize: 11 }} /><YAxis tick={{ fill: '#94a3b8', fontSize: 11 }} domain={['auto', 'auto']} /><Tooltip labelFormatter={value => `第 ${(Number(value) / 86400).toFixed(4)} 天`} contentStyle={{ background: '#0f172a', borderColor: '#475569' }} /><Legend /><Line dataKey="raw" name="原始工程值" stroke="#94a3b8" dot={focusChange} connectNulls={false} isAnimationActive={false} /><Line dataKey="clean" name="清理後" stroke="#818cf8" dot={focusChange} connectNulls={false} isAnimationActive={false} /><Brush dataKey="time" height={24} stroke="#818cf8" tickFormatter={value => `${Math.round(Number(value))}s`} onChange={range => { if (range.startIndex !== undefined && range.endIndex !== undefined && points[range.startIndex] && points[range.endIndex]) setSelectedRange({ start: points[range.startIndex].time, end: points[range.endIndex].time + cleaning.sample_time }); }} /></LineChart></ResponsiveContainer></div> : <p className="text-sm text-slate-400">目前範圍沒有顯示樣本，請切回完整期間。</p>}
      {selectedRange && <div className="flex flex-wrap items-center gap-3 text-sm"><span>已選區間 [{selectedRange.start}, {selectedRange.end}) 秒</span><button type="button" className={researchButton} disabled={(cleaning.regimes?.length || 0) >= 128} onClick={() => update({ regimes: [...(cleaning.regimes || []), { ...selectedRange, label: '', reason: '', exclude: false }] })}>帶入工況草稿並重新預覽</button></div>}
      <p className="text-xs text-slate-400">圖表僅使用伺服器回傳的顯示取樣；研究使用完整清理資料，不以圖表取樣取代。</p>
      <details><summary className="cursor-pointer text-sm text-indigo-300">資料品質與切分摘要</summary><div className="mt-3"><ResearchDetails value={preview.summary} /></div></details>
    </div>}
  </ResearchPanel>;
}
