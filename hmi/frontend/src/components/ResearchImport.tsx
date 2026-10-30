import { useEffect, useState } from 'react';
import type { FormEvent } from 'react';
import { researchError, researchRequest } from '../lib/research';
import type { Dataset, ResearchSource } from '../types/Research';
import { ResearchField, ResearchNotice, ResearchPanel, researchButton, researchControl, researchPrimary } from './ResearchControls';
import { ResearchSnapshotImport } from './ResearchSnapshotImport';

function DatasetOrigin({ dataset }: { dataset: Dataset }) {
  const source = dataset.source;
  return <div className="mt-3 space-y-2 break-words text-sm text-slate-400">
    <p>{dataset.row_count.toLocaleString()} 筆原始資料 · {dataset.columns.length} 個欄位 · 尚需對應與清理</p>
    {source?.kind === 'postgres' ? <>
      <p>PostgreSQL · {source.source_id} · {source.schema}.{source.table}</p>
      <p>擷取區間：{source.start}（含）至 {source.end}（不含）</p>
      <p>時間欄位：{source.timestamp_column} · 讀取時間：{source.read_at}</p>
    </> : source?.kind === 'captured_acquisition' ? <><p>唯讀採集快照 · {source.manifest.source_id}</p><p>採集時刻：{source.manifest.captured_at} · 區間 [{source.manifest.start}, {source.manifest.end})</p><p className="break-all">原始 SHA-256：{source.manifest.sha256}</p></> : source?.kind === 'generated_synthetic' ? <><p>伺服器核實合成開環歷史（非現場） · run {source.run_id}</p><p className="break-all">History SHA-256：{source.history_sha256}</p><details><summary className="cursor-pointer text-indigo-300">來源 receipt／config 身分</summary><p className="break-all">{source.receipt_sha256}<br />{source.config_sha256}</p></details></> : <p>{source?.kind === 'file' ? '來源：匯入檔案' : '來源：舊資料未記錄來源資訊'}</p>}
    <details><summary className="cursor-pointer text-indigo-300">查看原始欄位</summary><p className="mt-2">{dataset.columns.join('、')}</p></details>
  </div>;
}

export function ResearchImport({ datasets, datasetId, busy, onSelect, onStart, onFinish, onImported, onClean, onStudy }: {
  datasets: Dataset[]; datasetId: string; busy: boolean;
  onSelect: (id: string) => void; onStart: () => void; onFinish: () => void;
  onImported: (dataset: Dataset) => void; onClean: () => void; onStudy: () => void;
}) {
  const [mode, setMode] = useState<'file' | 'postgres'>('file');
  const [file, setFile] = useState<File | null>(null);
  const [name, setName] = useState('');
  const [format, setFormat] = useState<'csv' | 'history'>('csv');
  const [sources, setSources] = useState<ResearchSource[]>([]);
  const [sourceId, setSourceId] = useState('');
  const [sourcesLoading, setSourcesLoading] = useState(true);
  const [sourcesError, setSourcesError] = useState('');
  const [sourcesRevision, setSourcesRevision] = useState(0);
  const [start, setStart] = useState('');
  const [end, setEnd] = useState('');
  const [error, setError] = useState('');
  const source = sources.find(item => item.id === sourceId);
  const dataset = datasets.find(item => item.id === datasetId && item.kind === 'raw');

  useEffect(() => {
    const controller = new AbortController();
    setSourcesLoading(true); setSourcesError('');
    researchRequest<{ sources: ResearchSource[] }>('/sources', undefined, controller.signal).then(data => {
      if (controller.signal.aborted) return;
      setSources(data.sources);
      setSourceId(current => data.sources.some(item => item.id === current) ? current : data.sources.length === 1 ? data.sources[0].id : '');
    }).catch(failure => {
      if (!controller.signal.aborted) { setSources([]); setSourceId(''); setSourcesError(`無法讀取 PostgreSQL 來源：${researchError(failure)}`); }
    }).finally(() => { if (!controller.signal.aborted) setSourcesLoading(false); });
    return () => controller.abort();
  }, [sourcesRevision]);

  async function importData(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy) return;
    setError('');
    if (!name.trim()) { setError('請填寫資料集名稱，不能只包含空白。'); return; }
    if (mode === 'file' && !file) { setError('請選擇 CSV 或 history JSON 檔案。'); return; }
    let windowStart = '';
    let windowEnd = '';
    if (mode === 'postgres') {
      if (sourcesLoading || sourcesError || !source) { setError('請先選擇可用的 PostgreSQL 來源。'); return; }
      const startTime = Date.parse(`${start}Z`);
      const endTime = Date.parse(`${end}Z`);
      if (!start || !end || !Number.isFinite(startTime) || !Number.isFinite(endTime)) { setError('請填寫有效的 UTC 開始與結束時間。'); return; }
      if (startTime >= endTime) { setError('結束時間必須晚於開始時間；開始時間包含在內，結束時間不包含在內。'); return; }
      windowStart = new Date(startTime).toISOString();
      windowEnd = new Date(endTime).toISOString();
    }
    onStart();
    try {
      let imported: Dataset;
      if (mode === 'file' && file) {
        if (file.size > 32 * 1024 * 1024) throw new Error('檔案超過 32 MiB 請求上限，請先縮減匯入範圍。');
        const content = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(await file.arrayBuffer());
        const payload = { name: name.trim(), format, content };
        if (new Blob([JSON.stringify(payload)]).size > 32 * 1024 * 1024) throw new Error('JSON 編碼後超過 32 MiB 請求上限，請縮減匯入檔案。');
        imported = await researchRequest<Dataset>('/import', payload);
      } else {
        imported = await researchRequest<Dataset>('/import-postgres', { source_id: sourceId, name: name.trim(), start: windowStart, end: windowEnd });
      }
      onImported(imported);
    } catch (failure) { setError(`匯入未完成：${researchError(failure)}`); }
    finally { onFinish(); }
  }

  return <ResearchPanel title="1 · 帶入工程資料">
    <p className="mb-5 text-sm text-slate-400">匯入原始資料後，下一步設定 CV／MV／DV 欄位與清理規則。PostgreSQL 僅唯讀擷取，不會寫入資料庫或現場設備。</p>
    <form onSubmit={event => void importData(event)} aria-busy={busy}>
      <fieldset disabled={busy} className="min-w-0 space-y-4">
        <legend className="mb-3 text-sm font-medium text-slate-300">資料來源</legend>
        <div className="grid gap-3 sm:grid-cols-2">{[{ value: 'file', label: 'CSV / History JSON 檔案' }, { value: 'postgres', label: 'PostgreSQL 歷史資料' }].map(option => <label key={option.value} className={`flex cursor-pointer items-center gap-3 rounded-lg border p-3 text-sm ${mode === option.value ? 'border-indigo-500 bg-indigo-950/40 text-indigo-200' : 'border-slate-700 text-slate-300'}`}>
          <input type="radio" name="research-import-source" value={option.value} checked={mode === option.value} onChange={() => { setMode(option.value as 'file' | 'postgres'); setFile(null); setError(''); }} className="h-4 w-4 accent-indigo-500" />{option.label}
        </label>)}</div>
        {mode === 'file' ? <div className="grid gap-4 sm:grid-cols-2">
          <ResearchField label="匯入檔案"><input required type="file" accept=".csv,.json,text/csv,application/json" aria-describedby="research-file-help" className={`${researchControl} file:mr-3 file:rounded file:border-0 file:bg-indigo-600 file:px-3 file:py-1 file:text-white`} onChange={event => { const selected = event.target.files?.[0] || null; setFile(selected); if (selected) { setName(current => !current.trim() || current === file?.name ? selected.name : current); setFormat(selected.name.toLowerCase().endsWith('.json') ? 'history' : 'csv'); } }} /></ResearchField>
          <ResearchField label="檔案格式"><select className={researchControl} value={format} onChange={event => setFormat(event.target.value as 'csv' | 'history')}><option value="csv">CSV · 含欄位標題</option><option value="history">History JSON · canonical arrays</option></select></ResearchField>
          <p id="research-file-help" className="text-xs text-slate-400 sm:col-span-2">支援 UTF-8 CSV 與 canonical history JSON；不猜測編碼、不替換無效 bytes。CSV 分隔符由伺服器契約決定；JSON 編碼後請求上限為 32 MiB。</p>
        </div> : <div className="space-y-4">
          {sourcesLoading && <p role="status" className="text-sm text-slate-400">正在載入管理員設定的資料來源…</p>}
          {sourcesError && <div className="space-y-3"><ResearchNotice error={sourcesError} /><button type="button" className={researchButton} disabled={sourcesLoading} onClick={() => setSourcesRevision(value => value + 1)}>重新讀取來源</button></div>}
          {!sourcesLoading && !sourcesError && !sources.length && <p role="status" className="rounded-lg border border-slate-700 p-3 text-sm text-slate-300">尚未設定 PostgreSQL 來源。請管理員設定唯讀來源與可匯入欄位；目前仍可使用 CSV / History JSON 檔案。</p>}
          <ResearchField label="PostgreSQL 來源"><select required disabled={sourcesLoading || !sources.length} className={researchControl} value={sourceId} onChange={event => setSourceId(event.target.value)}><option value="">選擇已設定的來源</option>{sources.map(item => <option key={item.id} value={item.id}>{item.name}</option>)}</select></ResearchField>
          {source && <div className="space-y-2 rounded-lg border border-slate-700 bg-slate-950/50 p-3 text-sm text-slate-400">
            {source.description && <p className="break-words">{source.description}</p>}
            <p className="break-words">時間欄位：{source.timestamp_column} · 單次上限：{source.max_rows.toLocaleString()} 筆（超過將拒絕匯入，不會截斷）</p>
            <p className="break-words">匯入欄位：{source.columns.join('、')}</p>
          </div>}
          <div className="grid gap-4 sm:grid-cols-2">
            <ResearchField label="開始時間 · UTC（包含）"><input required type="datetime-local" step="1" className={researchControl} value={start} onChange={event => setStart(event.target.value)} aria-describedby="research-window-help" /></ResearchField>
            <ResearchField label="結束時間 · UTC（不包含）"><input required type="datetime-local" step="1" className={researchControl} value={end} onChange={event => setEnd(event.target.value)} aria-describedby="research-window-help" /></ResearchField>
          </div>
          <p id="research-window-help" className="text-xs text-slate-400">此處一律輸入 UTC，不會依瀏覽器時區轉換。例如台灣時間 08:00 請輸入 UTC 00:00。</p>
        </div>}
        <ResearchField label="資料集名稱"><input required className={researchControl} value={name} onChange={event => setName(event.target.value)} placeholder="例如：水泥磨一號線 · 穩態操作紀錄" /></ResearchField>
        {error && <ResearchNotice error={error} />}
        <div className="flex flex-wrap items-center gap-3"><button type="submit" disabled={mode === 'file' ? !file : sourcesLoading || !!sourcesError || !source} className={researchPrimary}>{busy ? '匯入原始資料中…' : '匯入並前往清理'}</button>{busy && <p role="status" className="text-sm text-slate-400">正在讀取並儲存原始版本，請稍候…</p>}</div>
      </fieldset>
    </form>
    <ResearchSnapshotImport busy={busy} onStart={onStart} onFinish={onFinish} onImported={onImported} />
    <fieldset disabled={busy} className="mt-6 min-w-0 border-t border-slate-800 pt-5">
      <ResearchField label="或選擇既有原始資料"><select className={researchControl} value={datasetId} onChange={event => onSelect(event.target.value)}><option value="">選擇原始資料集</option>{datasets.filter(item => item.kind === 'raw').map(item => <option key={item.id} value={item.id}>{item.name} · {item.row_count.toLocaleString()} 筆</option>)}</select></ResearchField>
      {dataset && <DatasetOrigin dataset={dataset} />}
      <div className="mt-3 flex flex-wrap gap-3"><button type="button" disabled={!dataset} className={researchButton} onClick={onClean}>對應與清理選定資料</button><button type="button" className={researchButton} onClick={onStudy}>使用清理資料或生成模擬資料</button></div>
    </fieldset>
  </ResearchPanel>;
}
