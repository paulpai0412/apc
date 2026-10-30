import { useEffect, useState } from 'react';
import { runtimeError, workspaceRequest } from '../lib/runtimeWorkspace';
import type { RuntimeEvent, RuntimeOperation } from '../types/RuntimeWorkspace';

const input = 'min-h-10 w-full rounded border border-slate-700 bg-slate-950 px-3 py-2 text-sm';
const button = 'min-h-10 rounded border border-slate-600 bg-slate-900 px-3 py-2 text-sm hover:border-indigo-400 disabled:opacity-40';

export function RuntimeEvents({ profileHash, active, operations, onRefresh }: { profileHash: string; active: boolean; operations: RuntimeOperation[]; onRefresh: () => void }) {
  const [events, setEvents] = useState<RuntimeEvent[]>([]);
  const [error, setError] = useState('');
  const [revision, setRevision] = useState(0);
  const [severity, setSeverity] = useState('all');
  const [unreadOnly, setUnreadOnly] = useState(false);
  const [note, setNote] = useState('');
  const [busy, setBusy] = useState(false);
  const [truncated, setTruncated] = useState(false);
  useEffect(() => {
    if (!active) return;
    const controller = new AbortController(); let timer: number | undefined;
    async function load() {
      try {
        const value = await workspaceRequest<{ events: RuntimeEvent[]; truncated: boolean }>('/api/runtime/events', undefined, controller.signal);
        if (controller.signal.aborted) return;
        setEvents(value.events); setTruncated(value.truncated); setError('');
      } catch (cause) { if (!controller.signal.aborted) setError(runtimeError(cause)); }
      if (!controller.signal.aborted) timer = window.setTimeout(() => void load(), 5000);
    }
    void load(); return () => { controller.abort(); window.clearTimeout(timer); };
  }, [active, revision]);
  async function markRead(id: number) {
    if (busy) return; setBusy(true); setError('');
    try { await workspaceRequest(`/api/runtime/events/${id}/read`, { note }); setRevision(value => value + 1); }
    catch (cause) { setError(runtimeError(cause)); } finally { setBusy(false); }
  }
  async function queryOperation(id: string) {
    if (busy) return; setBusy(true); setError('');
    try { await workspaceRequest(`/api/runtime/operations/${encodeURIComponent(id)}`); onRefresh(); }
    catch (cause) { setError(runtimeError(cause)); } finally { setBusy(false); }
  }
  const visible = events.filter(event => event.profile_hash === profileHash && (severity === 'all' || event.severity === severity) && (!unreadOnly || !event.read_at));
  return <div className="space-y-4">
    <p className="rounded border border-amber-800 p-3 text-sm text-amber-200">這是 HMI 操作與資料事件。外部 DCS 警報狀態仍須由 DCS 確認；標記已讀不會解除聯鎖、重啟或重新接管。</p>
    <div className="grid gap-3 sm:grid-cols-3"><label className="space-y-1 text-sm">嚴重度<select className={input} value={severity} onChange={event => setSeverity(event.target.value)}><option value="all">全部</option><option value="info">資訊</option><option value="warning">警告</option><option value="error">錯誤</option></select></label><label className="space-y-1 text-sm">已讀註記<input className={input} value={note} maxLength={2048} onChange={event => setNote(event.target.value)} /></label><label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={unreadOnly} onChange={event => setUnreadOnly(event.target.checked)} />只看未讀</label></div>
    {error && <p role="alert" className="text-sm text-rose-300">{error}</p>}
    {truncated && <p className="text-xs text-amber-200">僅顯示最新 500 筆可見查詢範圍；沒有列出不代表沒有歷史事件。</p>}
    {!error && !visible.length && <p className="text-sm text-slate-400">目前篩選沒有可見事件；這不表示外部 DCS 沒有警報。</p>}
    <div className="space-y-2">{visible.map(event => <article key={event.id} className="rounded border border-slate-700 p-3"><div className="flex flex-wrap items-center justify-between gap-2"><strong className={`text-xs ${event.severity === 'info' ? 'text-slate-300' : 'text-amber-200'}`}>{event.severity} · {event.source}</strong><span className="text-xs text-slate-500">{event.last_seen}</span></div><p className="my-2 whitespace-pre-wrap break-all text-sm">{event.condition}</p><p className="mb-2 text-xs text-slate-400">{event.condition_active === true ? '來源條件仍存在' : event.condition_active === false ? `來源條件已解除：${event.cleared_at ?? '時刻待確認'}` : '操作紀錄；外部條件由來源確認'}{event.last_evaluated ? ` · 最近評估 ${event.last_evaluated}` : ''}</p><div className="flex flex-wrap items-center gap-2"><button type="button" className={button} disabled={busy} onClick={() => void markRead(event.id)}>{event.read_at ? '更新已讀註記' : '標記已讀'}</button><span className="text-xs text-slate-400">{event.read_at ? `已讀：${event.read_at}` : '未讀'} · 已讀不改變條件狀態</span></div></article>)}</div>
    <section className="space-y-2"><h2 className="font-medium">持久操作紀錄</h2>{operations.filter(operation => operation.profile_hash === profileHash).map(operation => <details key={operation.operation_id} className="rounded border border-slate-700 p-3"><summary className="cursor-pointer break-all text-sm">{operation.state} · {operation.updated_at} · {operation.operation_id}</summary><pre className="my-2 max-h-64 overflow-auto whitespace-pre-wrap break-all text-xs">{JSON.stringify(operation, null, 2)}</pre><button type="button" className={button} disabled={busy} onClick={() => void queryOperation(operation.operation_id)}>查詢原生回條，不重送</button></details>)}</section>
  </div>;
}

type Metric = { variable_id: string; role: 'CV' | 'MV'; evidence: { valid_samples: number; unit: string; variance: number | null; bias: number | null; rmse: number | null; total_variation?: number | null; violation_fraction?: number | null; saturation_fraction?: number | null } };
type EvidenceWindow = { start: string; end: string; valid_samples: number; raw_samples: number; coverage: number | null; bad_samples: number; gaps: number; qualified: boolean; context_changed: boolean; metrics: Metric[]; warnings: string[] };
type Performance = { status: string; profile_hash: string; baseline: EvidenceWindow; candidate: EvidenceWindow; comparable: boolean; energy_intensity: null; causal_improvement: null; limitations: string[] };

export function RuntimePerformance({ profileHash }: { profileHash: string }) {
  const [baselineStart, setBaselineStart] = useState(''); const [baselineEnd, setBaselineEnd] = useState('');
  const [candidateStart, setCandidateStart] = useState(''); const [candidateEnd, setCandidateEnd] = useState('');
  const [report, setReport] = useState<Performance | null>(null); const [error, setError] = useState(''); const [busy, setBusy] = useState(false);
  const format = (value: number | null | undefined) => typeof value === 'number' && Number.isFinite(value) ? value.toLocaleString(undefined, { maximumSignificantDigits: 5 }) : '無資料';
  async function compare() {
    if (busy) return;
    const dates = [baselineStart, baselineEnd, candidateStart, candidateEnd].map(value => new Date(value));
    if (dates.some(date => !Number.isFinite(date.getTime())) || dates[0] >= dates[1] || dates[2] >= dates[3] || !(dates[1] <= dates[2] || dates[3] <= dates[0])) {
      setError('請填寫兩個不重疊且依時間遞增的完整區間。'); return;
    }
    setBusy(true); setError(''); setReport(null);
    try {
      const query = new URLSearchParams({ profile_hash: profileHash, baseline_start: dates[0].toISOString(), baseline_end: dates[1].toISOString(), candidate_start: dates[2].toISOString(), candidate_end: dates[3].toISOString() });
      const result = await workspaceRequest<Performance>(`/api/runtime/performance?${query}`);
      if (result.profile_hash !== profileHash || !result.baseline || !result.candidate) throw new Error('績效回應沒有可核對的目標與完整觀察區間。');
      setReport(result);
    } catch (cause) { setError(runtimeError(cause)); } finally { setBusy(false); }
  }
  const visible = report?.profile_hash === profileHash ? report : null;
  return <div className="space-y-4">
    <form className="space-y-3 rounded-xl border border-slate-700 p-4" onSubmit={event => { event.preventDefault(); void compare(); }}><h2 className="font-medium">比較相容工況，不混用不同產品或負荷</h2><p className="text-xs text-slate-400">時間以此瀏覽器時區輸入，送出前轉為 UTC。使用完整解析度、相同來源時刻的有效工程量；不以圖表抽點計算績效。</p><div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">{([{ label: '基準開始', value: baselineStart, set: setBaselineStart }, { label: '基準結束', value: baselineEnd, set: setBaselineEnd }, { label: '候選開始', value: candidateStart, set: setCandidateStart }, { label: '候選結束', value: candidateEnd, set: setCandidateEnd }]).map(field => <label key={field.label} className="space-y-1 text-sm">{field.label}<input type="datetime-local" step="1" className={input} value={field.value} onChange={event => field.set(event.target.value)} required /></label>)}</div><button type="submit" className={button} disabled={busy || !profileHash}>{busy ? '查詢原始資料並核對工況…' : '計算兩個區間'}</button></form>
    {error && <p role="alert" className="rounded border border-rose-800 p-3 text-sm text-rose-300">{error}</p>}
    {visible && <><p role="status" className={`rounded border p-3 text-sm ${visible.comparable ? 'border-emerald-800 text-emerald-300' : 'border-amber-800 text-amber-200'}`}>{visible.comparable ? '資料與工況可比較；數值差異仍不是因果改善證明。' : '資料不足或工況不相容，不計算改善百分比。'}</p><div className="grid gap-3 sm:grid-cols-2">{([{ label: '基準', window: visible.baseline }, { label: '候選', window: visible.candidate }]).map(({ label, window }) => <section key={label} className="rounded border border-slate-700 p-3 text-sm"><h3 className="font-medium">{label}</h3><p className="my-2 break-all text-xs text-slate-400">{window.start} ～ {window.end}</p><p>有效完整樣本 {window.valid_samples} · 覆蓋率 {window.coverage === null ? '無資料' : `${format(window.coverage * 100)}%`} · 壞樣本 {window.bad_samples} · 缺口 {window.gaps}</p><p className="mt-1 text-xs text-slate-400">{window.context_changed ? '工況在區間內改變' : '工況變動未被檢出'} · {window.qualified ? '通過資料資格' : '未通過資料資格'}</p>{window.warnings.map(warning => <p key={warning} className="mt-1 text-xs text-amber-200">{warning}</p>)}</section>)}</div><div className="overflow-x-auto rounded border border-slate-700"><table className="w-full min-w-[750px] text-left text-sm"><thead className="bg-slate-900 text-slate-400"><tr>{['變數', '基準變異數', '候選變異數', '變異數差異', '候選偏差／RMSE', '候選超限比例', 'MV 總移動量'].map(label => <th key={label} className="p-3 font-normal">{label}</th>)}</tr></thead><tbody>{visible.candidate.metrics.map(metric => {
      const baseline = visible.baseline.metrics.find(item => item.variable_id === metric.variable_id && item.role === metric.role && item.evidence.unit === metric.evidence.unit);
      const before = baseline?.evidence.variance; const after = metric.evidence.variance;
      const difference = visible.comparable && typeof before === 'number' && before > 0 && typeof after === 'number' ? (after - before) / before * 100 : null;
      return <tr key={`${metric.role}:${metric.variable_id}`} className="border-t border-slate-800"><th className="p-3 font-normal">{metric.role} · {metric.variable_id}<span className="block text-xs text-slate-500">{metric.evidence.unit}</span></th><td className="p-3">{format(before)} {metric.evidence.unit}²</td><td className="p-3">{format(after)} {metric.evidence.unit}²</td><td className={`p-3 ${difference !== null && difference < 0 ? 'text-emerald-300' : 'text-slate-300'}`}>{difference === null ? '不可比較' : `${difference > 0 ? '+' : ''}${format(difference)}%`}</td><td className="p-3">{metric.role === 'CV' ? `${format(metric.evidence.bias)} ／ ${format(metric.evidence.rmse)}` : '不適用'}</td><td className="p-3">{typeof metric.evidence.violation_fraction === 'number' ? `${format(metric.evidence.violation_fraction * 100)}%` : '無資料'}</td><td className="p-3">{metric.role === 'MV' ? `${format(metric.evidence.total_variation)} ${metric.evidence.unit}` : '不適用'}</td></tr>;
    })}</tbody></table></div><p className="text-xs text-amber-200">沒有獨立功率與產量計量時，不顯示節能率。MV 移動減少不等於能耗降低；工況配對也不能排除所有未量測擾動。</p><details className="rounded border border-slate-700 p-3"><summary className="cursor-pointer text-sm">完整績效證據</summary><pre className="mt-2 max-h-96 overflow-auto whitespace-pre-wrap break-all text-xs">{JSON.stringify(visible, null, 2)}</pre></details></>}
  </div>;
}
