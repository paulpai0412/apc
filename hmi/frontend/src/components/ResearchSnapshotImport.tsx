import { useState } from 'react';
import { researchError, researchRequest } from '../lib/research';
import type { Dataset } from '../types/Research';
import { ResearchDetails, ResearchField, ResearchNotice, researchButton, researchControl } from './ResearchControls';

type Manifest = { schema_version: 1; source_id: string; captured_at: string; start: string; end: string; format: 'csv' | 'history'; sha256: string; read_only: true };
export function ResearchSnapshotImport({ busy, onStart, onFinish, onImported }: { busy: boolean; onStart: () => void; onFinish: () => void; onImported: (dataset: Dataset) => void }) {
  const [file, setFile] = useState<File | null>(null);
  const [manifest, setManifest] = useState<Manifest | null>(null);
  const [error, setError] = useState('');
  async function readManifest(file: File | undefined) {
    setManifest(null); setError('');
    if (!file) return;
    try {
      if (file.size > 64 * 1024) throw new Error('來源 manifest 超過 64 KiB 上限。');
      const value = JSON.parse(await file.text()) as Manifest;
      if (value.schema_version !== 1 || value.read_only !== true || !value.source_id || !/^[a-f0-9]{64}$/.test(value.sha256) || !['csv', 'history'].includes(value.format)) throw new Error('manifest 須包含 schema_version=1、read_only=true、source_id、format 與 sha256。');
      if (![value.captured_at, value.start, value.end].every(time => typeof time === 'string' && /(?:Z|[+-]\d\d:\d\d)$/.test(time) && Number.isFinite(Date.parse(time))) || Date.parse(value.start) >= Date.parse(value.end)) throw new Error('快照時刻與半開區間須具有時區且有效。');
      setManifest(value);
    } catch (failure) { setError(researchError(failure)); }
  }
  async function submit() {
    if (!file || !manifest || busy) return;
    onStart(); setError('');
    try {
      if (file.size > 32 * 1024 * 1024) throw new Error('快照超過 32 MiB 上限。');
      const bytes = await file.arrayBuffer();
      const digest = Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', bytes)), value => value.toString(16).padStart(2, '0')).join('');
      if (digest !== manifest.sha256) throw new Error('快照 bytes 與來源 manifest SHA-256 不相符。');
      const content = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes);
      const payload = { name: file.name, content, manifest };
      if (new Blob([JSON.stringify(payload)]).size > 32 * 1024 * 1024) throw new Error('編碼後請求超過 32 MiB 上限。');
      onImported(await researchRequest<Dataset>('/import-snapshot', payload));
    } catch (failure) { setError(researchError(failure)); } finally { onFinish(); }
  }
  return <details className="mt-5 rounded border border-slate-700 p-3"><summary className="cursor-pointer font-medium text-indigo-300">匯入唯讀採集快照與來源 manifest</summary><fieldset disabled={busy} className="mt-3 space-y-3"><p className="text-xs text-slate-400">只接收採集端已輸出的 UTF-8 檔案及雜湊 manifest，不連線 OPC、不接收 DSN，也不取得寫入通道。</p><ResearchField label="採集快照檔案"><input className={researchControl} type="file" accept=".csv,.json" onChange={event => setFile(event.target.files?.[0] || null)} /></ResearchField><ResearchField label="來源 manifest JSON"><input className={researchControl} type="file" accept=".json" onChange={event => void readManifest(event.target.files?.[0])} /></ResearchField>{manifest && <ResearchDetails value={manifest} />}{error && <ResearchNotice error={error} />}<button type="button" className={researchButton} disabled={!file || !manifest} onClick={() => void submit()}>核對原始雜湊並匯入快照</button></fieldset></details>;
}
