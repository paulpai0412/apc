import { useEffect, useState } from 'react';
import { runtimeError, workspaceRequest } from '../lib/runtimeWorkspace';

const button = 'min-h-10 rounded border border-slate-700 px-3 py-2 text-sm hover:border-indigo-400 disabled:opacity-40';
type Capacity = { status: string; total_bytes: number | null; available_bytes: number | null; scope?: string; reason?: string | null };
type Maintenance = {
  observed_at: string; can_reconnect: boolean; blockers: string[];
  connection: { configured_endpoint: string | null; worker_configured_enabled: boolean; worker_observation_available: boolean;
    worker: { reported_connected: boolean | null; generation: number; observed_at: string | null; reason: string | null };
    observed_endpoints: { endpoint: string; last_received_at: string; last_source_timestamp: string }[]; reconnect_delay_seconds: number };
  controllers: { profile_hash: string; engine_id: string; endpoint: string; native_status: unknown; blockers: string[] }[];
  storage: { database: { status: string; allocated_bytes: number | null; free_page_bytes: number | null; journal_mode: string | null; error?: string | null };
    paths: { name: string; configured_path: string; kind: string; file_bytes: number | null; modified_at: string | null; filesystem: Capacity }[]; recursive_scan: false };
  retention: { status: string; reason: string };
  limits: unknown;
};
const bytes = (value: number | null) => value === null || !Number.isFinite(value) ? '未知' : `${value.toLocaleString()} bytes`;

export function RuntimeMaintenance({ active, permitted }: { active: boolean; permitted: boolean }) {
  const [state, setState] = useState<Maintenance | null>(null); const [error, setError] = useState('');
  const [busy, setBusy] = useState(false); const [loading, setLoading] = useState(false);
  const [revision, setRevision] = useState(0); const [receipt, setReceipt] = useState<unknown>(null);
  useEffect(() => {
    if (!active || !permitted) return;
    const controller = new AbortController(); setLoading(true);
    void workspaceRequest<Maintenance>('/api/runtime/maintenance', undefined, controller.signal)
      .then(value => { if (!controller.signal.aborted) { setState(value); setError(''); } })
      .catch(cause => { if (!controller.signal.aborted) { setState(null); setError(runtimeError(cause)); } })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [active, permitted, revision]);

  async function reconnect() {
    if (!active || !permitted || busy || !state?.can_reconnect || !state.connection.configured_endpoint) return;
    if (!window.confirm(`要求重連共用 OPC worker？\n${state.connection.configured_endpoint}\n此動作影響全部已核准控制群組。伺服器會重新核對各群組已安全交還 DCS、沒有未解操作，且仍具維護權限。排隊成功不代表已連線；不會自動 Arm 或 Engage。`)) return;
    setBusy(true); setReceipt(null); setError('');
    try {
      const result = await workspaceRequest<unknown>('/api/infrastructure/opc-reconnect', {});
      setReceipt(result); setRevision(value => value + 1);
    } catch (cause) { setError(`${runtimeError(cause)} 維護要求結果未確認；請查詢狀態與稽核，不自動重送。`); }
    finally { setBusy(false); }
  }

  return <section className="space-y-4 rounded-xl border border-slate-700 p-4" aria-label="連線容量與安全維護">
    <div className="flex flex-wrap items-center justify-between gap-3"><h2 className="font-medium">連線、容量與安全維護</h2><button type="button" className={button} disabled={!permitted || loading || busy} onClick={() => setRevision(value => value + 1)}>重新查詢，不重連</button></div>
    {!permitted && <p className="text-sm text-amber-200">需要 Admin 與全部已配置群組的明確 runtime.maintenance 授權。管理員角色本身不能取代此範圍權限。</p>}
    {loading && <p role="status" className="text-sm text-slate-400">讀取實際維護狀態…</p>}
    {error && <p role="alert" className="text-sm text-rose-300">{error}</p>}
    {permitted && state && <>
      <dl className="grid gap-3 text-xs sm:grid-cols-2"><div><dt className="text-slate-400">核對時刻</dt><dd>{state.observed_at}</dd></div><div><dt className="text-slate-400">固定來源</dt><dd className="break-all">{state.connection.configured_endpoint ?? '未配置'}</dd></div><div><dt className="text-slate-400">Worker 組態</dt><dd>{state.connection.worker_configured_enabled ? '已配置啟用' : '已配置停用'} · {state.connection.worker_observation_available ? '曾收到觀測，最新品質請看資料健康' : '尚無觀測證據'}</dd></div><div><dt className="text-slate-400">重連延遲設定</dt><dd>{state.connection.reconnect_delay_seconds} 秒</dd></div></dl>
      <p className="text-xs text-slate-400">Worker 最近回報：{state.connection.worker.reported_connected === true ? '連線' : state.connection.worker.reported_connected === false ? '斷線' : '未知'} · generation {state.connection.worker.generation} · {state.connection.worker.observed_at ?? '尚無回報時刻'}{state.connection.worker.reason ? ` · ${state.connection.worker.reason}` : ''}。這不是 PID 應用或控制接管證據。</p>
      {state.connection.observed_endpoints.map(endpoint => <p key={endpoint.endpoint} className="break-all text-xs text-slate-400">{endpoint.endpoint} · 最近來源 {endpoint.last_source_timestamp || '未知'} · 最近接收 {endpoint.last_received_at || '未知'}</p>)}
      {!!state.blockers.length && <ul className="list-inside list-disc text-xs text-amber-200">{state.blockers.map(blocker => <li key={blocker}>{blocker}</li>)}</ul>}
      <button type="button" className={button} disabled={!state.can_reconnect || !state.connection.configured_endpoint || busy || loading} onClick={() => void reconnect()}>{busy ? '等待維護要求回覆…' : '確認後要求重連共用 OPC worker'}</button>
      <section className="space-y-2"><h3 className="text-sm font-medium">資料庫與檔案系統容量</h3><p className="text-xs text-slate-400">資料庫配置 {bytes(state.storage.database.allocated_bytes)} · 內部可重用頁 {bytes(state.storage.database.free_page_bytes)} · Journal {state.storage.database.journal_mode ?? '未知'}。內部空頁不是檔案系統可用容量。</p>
        {state.storage.database.error && <p className="text-xs text-amber-200">{state.storage.database.error}</p>}
        <div className="overflow-x-auto"><table className="w-full text-left text-xs"><thead><tr className="border-b border-slate-700"><th className="p-2">儲存位置</th><th className="p-2">檔案大小</th><th className="p-2">所在檔案系統總量</th><th className="p-2">所在檔案系統可用量</th></tr></thead><tbody>{state.storage.paths.map(path => <tr key={path.name} className="border-b border-slate-800"><td className="p-2">{path.name}<span className="block max-w-64 break-all text-slate-400">{path.configured_path}</span></td><td className="p-2">{path.kind === 'directory' ? '未遞迴加總' : bytes(path.file_bytes)}</td><td className="p-2">{bytes(path.filesystem.total_bytes)}</td><td className="p-2">{bytes(path.filesystem.available_bytes)}{path.filesystem.reason && <span className="block text-amber-200">{path.filesystem.reason}</span>}</td></tr>)}</tbody></table></div>
        <p className="text-xs text-slate-400">同一檔案系統可能重複列在不同位置，不可相加；不以目錄 metadata 大小冒充產物總量。</p>
      </section>
      <p className="text-xs text-amber-200">保留策略：{state.retention.status} · {state.retention.reason}</p>
      <details><summary className="cursor-pointer text-sm">實際容量限制與全部群組安全證據</summary><pre className="mt-2 max-h-80 overflow-auto whitespace-pre-wrap break-all text-xs">{JSON.stringify({ limits: state.limits, controllers: state.controllers, retention: state.retention }, null, 2)}</pre></details>
    </>}
    {receipt !== null && <div role="status" className="space-y-2 text-xs"><p className="text-amber-200">已取得維護要求回覆；仍須核對新的來源觀測，不代表連線或控制接管成功。</p><pre className="max-h-48 overflow-auto whitespace-pre-wrap break-all">{JSON.stringify(receipt, null, 2)}</pre></div>}
  </section>;
}
