import { useEffect, useRef, useState } from 'react';
import type { HistoryEnvelope } from '../lib/historianTrends';
import { runtimeHistoryData } from '../lib/runtimeView';
import { runtimeError, workspaceRequest } from '../lib/runtimeWorkspace';
import type { ProcessTrendData, TrendVariable } from '../types/ProcessTrend';
import type { DeploymentProfile } from '../types/RuntimeWorkspace';
import { ProcessTrendChart } from './ProcessTrendChart';

const input = 'min-h-10 w-full rounded border border-slate-700 bg-slate-950 px-3 py-2 text-sm';
const button = 'min-h-10 rounded border border-slate-700 px-3 py-2 text-sm hover:border-indigo-400 disabled:opacity-40';
type RecordedHistory = HistoryEnvelope & {
  profile_hash: string; sourceEndpoint: string | null; window: { start: string; end: string };
  reduction: 'full_resolution' | 'extrema_preserving'; sampled_tags: Record<string, string> | null;
  historical_forecast_available: false; forecast_unavailable_reason: string;
};
type Props = { profile: DeploymentProfile; now: number; storageKey: string;
  focusedVariableId: string | null; onVariableFocus: (variable: TrendVariable) => void };

function localInput(time: number): string {
  const value = new Date(time);
  return new Date(time - value.getTimezoneOffset() * 60_000).toISOString().slice(0, 19);
}

export function RuntimeHistory({ profile, now, storageKey, focusedVariableId, onVariableFocus }: Props) {
  const [start, setStart] = useState(() => localInput(now - 3_600_000));
  const [end, setEnd] = useState(() => localInput(now - 5_000));
  const [busy, setBusy] = useState(false); const [error, setError] = useState('');
  const [data, setData] = useState<ProcessTrendData | null>(null);
  const [evidence, setEvidence] = useState<RecordedHistory | null>(null);
  const pending = useRef<AbortController | null>(null);
  useEffect(() => () => pending.current?.abort(), []);

  async function query() {
    const from = Date.parse(start), to = Date.parse(end);
    if (!Number.isFinite(from) || !Number.isFinite(to) || from >= to || to > now || to - from > 31 * 86_400_000) {
      setError('請設定已發生、先後順序正確且不超過 31 天的時間範圍。時間輸入使用本機時區。'); return;
    }
    pending.current?.abort(); const controller = new AbortController(); pending.current = controller;
    setBusy(true); setError('');
    try {
      const params = new URLSearchParams({ profile_hash: profile.profile_hash,
        start: new Date(from).toISOString(), end: new Date(to).toISOString(), max_points: '2000' });
      const response = await workspaceRequest<RecordedHistory>(`/api/runtime/trends?${params}`, undefined, controller.signal);
      if (controller.signal.aborted) return;
      const emptySource = response.sourceEndpoint === null && response.rawSamples === 0 && Array.isArray(response.points) && response.points.length === 0;
      if (response.profile_hash !== profile.profile_hash || (!emptySource && response.sourceEndpoint !== profile.endpoint)
        || Date.parse(response.window?.start) !== from || Date.parse(response.window?.end) !== to
        || !Array.isArray(response.points) || !Array.isArray(response.warnings)
        || !['full_resolution', 'extrema_preserving'].includes(response.reduction)) {
        throw new Error('歷史資料的來源、profile 或時間範圍不符；不混入其他來源或舊查詢。');
      }
      const result = runtimeHistoryData(profile, response, { start: from, end: to }, response.sampled_tags ?? {});
      result.warnings.push(response.forecast_unavailable_reason || '此歷史來源沒有可核對的完整預測快照；不以目前預測回填過去。');
      setData(result); setEvidence(response);
    } catch (cause) { if (!controller.signal.aborted) setError(runtimeError(cause)); }
    finally { if (pending.current === controller) { pending.current = null; setBusy(false); } }
  }

  return <section className="space-y-4 rounded-xl border border-slate-700 p-4" aria-label="核准來源歷史趨勢">
    <div className="flex flex-wrap items-center justify-between gap-3"><h2 className="font-medium">歷史查詢 · 原始來源時間</h2>
      <div className="flex flex-wrap gap-2">{[1, 8, 24].map(hours => <button key={hours} type="button" className={button}
        onClick={() => { setStart(localInput(now - hours * 3_600_000)); setEnd(localInput(now - 5_000)); }}>最近 {hours} 小時</button>)}</div>
    </div>
    <form className="flex flex-wrap items-end gap-3" onSubmit={event => { event.preventDefault(); void query(); }}>
      <label className="min-w-0 flex-1 space-y-1 text-sm">開始（本機時區）<input className={input} type="datetime-local" step="1" value={start} onChange={event => setStart(event.target.value)} required /></label>
      <label className="min-w-0 flex-1 space-y-1 text-sm">結束（本機時區）<input className={input} type="datetime-local" step="1" value={end} onChange={event => setEnd(event.target.value)} required /></label>
      <button className={button} disabled={busy} type="submit">{busy ? '查詢指定來源…' : '查詢歷史'}</button>
      {busy && <button className={button} type="button" onClick={() => { pending.current?.abort(); pending.current = null; setBusy(false); }}>取消查詢</button>}
    </form>
    <p className="text-xs text-slate-400">僅查詢此核准群組的 CV／MV／DV 與已取樣目標／界線。圖表保留極值與品質缺口；正式變異與超限統計請使用「績效與漂移」。過去的預測不會用目前預測冒充。</p>
    {error && <p role="alert" className="text-sm text-rose-300">{error}{data ? ' 下方仍是上一次成功查詢，並非目前輸入範圍。' : ''}</p>}
    {data && evidence && <>
      <p className="break-all text-xs text-slate-400">已載入：{new Date(data.range.start).toLocaleString()} ～ {new Date(data.range.end).toLocaleString()} · 原始 {evidence.rawSamples.toLocaleString()} 筆 · {evidence.reduction === 'full_resolution' ? '完整解析度' : '極值保留顯示縮減'} · {evidence.sourceEndpoint ?? '本窗口沒有來源樣本'}</p>
      <ProcessTrendChart data={data} storageKey={storageKey} initialView="role_blocks" focusedVariableId={focusedVariableId} onVariableFocus={onVariableFocus} />
    </>}
  </section>;
}
