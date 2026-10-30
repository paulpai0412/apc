import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { TwinManifest, TwinOperation, TwinSession, TwinVariableFocus } from '../types/DigitalTwin';
import type { TrendVariable } from '../types/ProcessTrend';
import { acceptsTwinSnapshot, recentTwinTrend, sessionCompatibility } from '../lib/digitalTwin';
import { simulatedEquipmentPoints } from '../lib/equipmentView';
import { unboundEquipmentTemplate } from '../lib/twinEquipmentTemplate';
import { researchError, researchRequest } from '../lib/research';
import { DigitalTwinRecordedScene } from './DigitalTwinRecordedScene';
import { DigitalTwinIdentity, DigitalTwinProcess } from './DigitalTwinModel';
import { DigitalTwinScenePanel } from './DigitalTwinScenePanel';
import { ProcessTrendChart } from './ProcessTrendChart';
import { ResearchField, ResearchNotice, ResearchPanel, researchButton, researchControl, researchPrimary } from './ResearchControls';

const executionNames = { starting: '啟動中', paused: '已暫停', running: '伺服器連續執行中', completed: '已完成', closed: '已關閉', failed: '失敗', interrupted: '已中斷' };
const operationNames = { start: '連續執行', pause: '暫停', step: '單步', close: '關閉' };
const operationStates = { accepted: '已接受', completed: '已完成', unknown: '結果未知', rejected: '已拒絕' };
function pendingOperation(key: string): string | null {
  try { return localStorage.getItem(key); } catch { return null; }
}

export function DigitalTwinSession({ id, manifest, onUpdate, sceneVisible = true, active = true }: {
  id: string; manifest: TwinManifest; onUpdate: (session: TwinSession) => void; sceneVisible?: boolean; active?: boolean;
}) {
  const [renderedSession, setSession] = useState<TwinSession | null>(null);
  const [busy, setBusy] = useState('snapshot');
  const [error, setError] = useState('');
  const [count, setCount] = useState(128);
  const operationKey = `authrity.twin.operation.${manifest.run_id}.${id}`;
  const [unknownOperation, setUnknownOperation] = useState<string | null>(() => pendingOperation(operationKey));
  const unknown = useRef(unknownOperation); unknown.current = unknownOperation;
  const [uncertain, setUncertain] = useState(true);
  const [comparison, setComparison] = useState('');
  const [chartMode, setChartMode] = useState<'recent' | 'archive'>('recent');
  const [focus, setFocus] = useState<TwinVariableFocus | null>(null);
  const [equipmentId, setEquipmentId] = useState<string | null>(null);
  const [bindingId, setBindingId] = useState<string | null>(null);
  const [clock, setClock] = useState(Date.now());
  const [frameAt, setFrameAt] = useState<number | null>(null);
  const frameReceivedAt = useRef<number | null>(null);
  const visualActive = useRef(active); visualActive.current = active;
  const displayedStatus = useRef<TwinSession['status'] | null>(null);
  const current = useRef<TwinSession | null>(null);
  const session = active ? current.current ?? renderedSession : renderedSession;
  const inFlight = useRef(false);
  const alive = useRef(false);
  const update = useRef(onUpdate); update.current = onUpdate;
  const path = `/twin/sessions/${encodeURIComponent(id)}`;
  const rememberUnknown = useCallback((operationId: string | null) => {
    unknown.current = operationId; setUnknownOperation(operationId);
    try { if (operationId) localStorage.setItem(operationKey, operationId); else localStorage.removeItem(operationKey); } catch { /* Server operation history remains authoritative. */ }
  }, [operationKey]);
  const accept = useCallback((next: TwinSession) => {
    if (!alive.current) return;
    if (!acceptsTwinSnapshot(current.current, next, id, manifest.run_id)) throw new Error('拒絕跨來源、版本不符或倒退的工作階段快照。');
    if (next.frame && (!current.current || next.samples !== current.current.samples)) frameReceivedAt.current = Date.now();
    current.current = next;
    if (visualActive.current) {
      if (next.status === 'completed' && displayedStatus.current !== 'completed') setChartMode('archive');
      displayedStatus.current = next.status;
      setFrameAt(frameReceivedAt.current); setSession(next); update.current(next);
    }
    setUncertain(false);
    if (!unknown.current && next.operation?.status === 'unknown') rememberUnknown(next.operation.id);
  }, [id, manifest.run_id, rememberUnknown]);
  useEffect(() => {
    alive.current = true;
    return () => { alive.current = false; };
  }, []);
  // This timer only reads. The SessionOwner owns all advancement, including after tab closure.
  useEffect(() => {
    const controller = new AbortController();
    let timer: number | undefined;
    async function poll() {
      if (!inFlight.current) {
        inFlight.current = true; setBusy('snapshot');
        try {
          const operationId = unknown.current;
          const next = await researchRequest<TwinSession>(operationId ? `${path}/operations/${encodeURIComponent(operationId)}` : path, undefined, controller.signal);
          if (!controller.signal.aborted) {
            accept(next);
            if (operationId) {
              if (next.operation?.id !== operationId) throw new Error('原操作查詢回傳不同 ID；保持結果未知。');
              if (next.operation.status !== 'unknown') {
                rememberUnknown(null);
                setError(next.operation.status === 'rejected' ? next.operation.reason || '原操作已拒絕。' : '');
              }
            }
          }
        } catch (failure) { if (!controller.signal.aborted) { setUncertain(true); setError(researchError(failure)); } }
        finally { inFlight.current = false; if (!controller.signal.aborted) setBusy(''); }
      }
      if (!controller.signal.aborted) timer = window.setTimeout(() => void poll(), active && document.visibilityState === 'visible' ? 1000 : 5000);
    }
    void poll();
    return () => { controller.abort(); window.clearTimeout(timer); };
  }, [path, accept, active, rememberUnknown]);
  useEffect(() => {
    if (!active) return;
    const timer = window.setInterval(() => { if (document.visibilityState === 'visible') setClock(Date.now()); }, 1000);
    return () => window.clearInterval(timer);
  }, [active]);
  async function reconcile() {
    if (inFlight.current) { setError('已有讀取或操作正在進行，請等待回覆後再查詢；未送出新命令。'); return; }
    inFlight.current = true; setBusy('snapshot'); setError('');
    try {
      const operationId = unknown.current;
      const next = await researchRequest<TwinSession>(operationId ? `${path}/operations/${encodeURIComponent(operationId)}` : path);
      if (!alive.current) return;
      accept(next);
      if (operationId) {
        if (next.operation?.id !== operationId) throw new Error('伺服器未回傳原操作 ID，保持結果未知。');
        if (next.operation.status === 'unknown') setError(next.operation.reason || '原操作結果仍未知；禁止自動重送。');
        else rememberUnknown(null);
      }
    } catch (failure) { if (alive.current) { setUncertain(true); setError(researchError(failure)); } }
    finally { inFlight.current = false; if (alive.current) setBusy(''); }
  }
  async function command(command: TwinOperation['command']) {
    if (inFlight.current) { setError('正在取得伺服器快照；本次命令尚未送出，請等待後再操作。'); return; }
    if (unknown.current || uncertain || !current.current?.execution_status) return;
    const operationId = crypto.randomUUID();
    // Persist before sending so a refresh can query the original operation, never replay it.
    rememberUnknown(operationId); inFlight.current = true; setBusy(command); setError('');
    try {
      const next = await researchRequest<TwinSession>(`${path}/${command}`, { operation_id: operationId, ...(command === 'start' ? { batch_steps: count } : {}) });
      if (!alive.current) return;
      accept(next);
      if (next.operation?.id !== operationId || next.operation.status === 'unknown') throw new Error(next.operation?.reason || '命令回應未確認原操作結果。');
      rememberUnknown(null);
      if (next.operation.status === 'rejected') setError(next.operation.reason || '伺服器拒絕操作。');
    } catch (failure) {
      if (alive.current) { setUncertain(true); setError(`操作結果未知：${researchError(failure)}。請查詢原操作 ${operationId}；不會自動重送或聲稱已停止伺服器。`); }
    } finally { inFlight.current = false; if (alive.current) setBusy(''); }
  }
  const recent = useMemo(() => session ? recentTwinTrend(manifest, session) : null, [manifest, session]);
  const view = useMemo(() => session?.equipment_view?.view ?? unboundEquipmentTemplate(), [session?.equipment_view?.view]);
  const effectiveFrameAt = active ? frameReceivedAt.current : frameAt;
  const age = effectiveFrameAt === null ? null : Math.max(0, Math.max(clock, Date.now()) - effectiveFrameAt);
  const stale = uncertain || age === null || ['failed', 'interrupted', 'closed'].includes(session?.execution_status ?? session?.status ?? '') || (session?.execution_status === 'running' && age > 5000);
  const origin = session?.time_basis === 'relative' ? 0 : Date.parse(session?.start ?? '');
  const points = useMemo(() => simulatedEquipmentPoints(view, session?.variables ?? manifest.variables, session?.frame, origin, stale, 'simulation'), [view, session?.variables, manifest.variables, session?.frame, origin, stale]);
  const selectBinding = (id: string) => {
    const binding = view.bindings.find(item => item.id === id); setBindingId(id); setEquipmentId(binding?.equipment_id ?? null);
    if (binding && binding.role !== 'telemetry') setFocus({ role: binding.role.toUpperCase() as TwinVariableFocus['role'], name: binding.variable_name, unit: binding.unit });
  };
  const selectEquipment = (id: string) => {
    const binding = view.bindings.find(item => item.equipment_id === id && item.role !== 'telemetry' && item.channel === 'actual');
    if (binding) selectBinding(binding.id);
    else { setEquipmentId(id); setBindingId(null); setFocus(null); }
  };
  const selectVariable = (variable: TrendVariable | TwinVariableFocus) => {
    if (variable.role === 'OTHER') return;
    setFocus({ role: variable.role, name: variable.name, unit: variable.unit });
    const binding = view.bindings.find(item => item.role.toUpperCase() === variable.role && item.variable_name === variable.name && item.unit === variable.unit);
    setBindingId(binding?.id ?? null); setEquipmentId(binding?.equipment_id ?? null);
  };
  const other = manifest.sessions.find(item => item.id === comparison);
  const incompatibility = session && other ? sessionCompatibility(session, other) : null;
  const archives = session ? [session, ...(other && !incompatibility ? [other] : [])].map(item => ({ id: item.id, label: `${item.trial_id} · ${item.id}`, path: `/twin/sessions/${encodeURIComponent(item.id)}/series` })) : [];
  const execution = session?.execution_status;
  const disabled = !!busy || uncertain || !!unknownOperation || !execution;
  return <div className="min-w-0 space-y-3">
    <ResearchPanel title="凍結模型工作階段">
      <div className="space-y-3">
        <div className="flex flex-wrap gap-2"><span className="break-all font-mono text-xs">{id}</span><span>{execution ? executionNames[execution] : session ? `${executionNames[session.status]}（歷史紀錄，無執行命令）` : '讀取快照'}</span></div>
        {session && <><p className="text-sm">{session.samples.toLocaleString()} / {session.requested_samples.toLocaleString()} 掃描 · {session.scenario === 'full' ? '完整情境 · 探索性重跑' : '選參驗證情境'} · {session.transport}</p><progress aria-label="孿生完成掃描進度" className="h-2 w-full accent-indigo-500" value={session.samples} max={Math.max(1, session.requested_samples)} /><DigitalTwinIdentity session={session} /><p className="break-all text-xs text-slate-400">序列 {session.sequence ?? '舊紀錄未提供'} · 原生序列 {session.native_sequence ?? '未提供'} · 最後新影格年齡 {age === null ? '未知' : `${(age / 1000).toFixed(0)}s`} · {stale ? '過期／未確認，動畫停止' : '已確認快照'} · 設備版本 {session.equipment_view?.id ?? '未綁定'} · 設定版本 {session.settings_version_id ?? '原試驗'}</p></>}
        <div className="flex flex-wrap items-end gap-2"><div className="w-44"><ResearchField label="伺服器每批掃描數"><select className={researchControl} value={count} disabled={!!busy || execution === 'running'} onChange={event => setCount(Number(event.target.value))}>{[1, 16, 64, 128, 256, 512].map(value => <option key={value} value={value}>{value} 掃描</option>)}</select></ResearchField></div>
          <button type="button" className={researchButton} disabled={!!busy} onClick={() => void reconcile()}>{unknownOperation ? '查詢原操作結果' : '取得孿生快照'}</button>
          <button type="button" className={researchButton} disabled={disabled || execution !== 'paused'} onClick={() => void command('step')}>單步推進</button>
          <button type="button" className={researchPrimary} disabled={disabled || execution !== 'paused'} onClick={() => void command('start')}>啟動伺服器連續執行</button>
          <button type="button" className={researchButton} disabled={disabled || execution !== 'running'} onClick={() => void command('pause')}>本批完成後暫停</button>
          <button type="button" className={researchButton} disabled={disabled || !execution || !['running', 'paused', 'starting'].includes(execution)} onClick={() => void command('close')}>關閉工作階段</button>
        </div>
        <p className="text-xs text-slate-400">連續工作由伺服器擁有；離頁、收合與關閉瀏覽器不停止執行。暫停等待已接受批次完成；重新開啟只讀最新確認快照，不補播舊影格，也不自動重送任何命令。</p>
        {session?.operation && <p className="break-all text-xs">原操作 {session.operation.id} · {operationNames[session.operation.command]} · {operationStates[session.operation.status]} {session.operation.reason}</p>}
        {unknownOperation && <p role="alert" className="break-all text-xs text-amber-200">待核對操作：{unknownOperation}。讀取一般快照不會解除此未知結果。</p>}
        {busy && <p role="status" className="text-xs text-indigo-300">{busy === 'snapshot' ? '讀取狀態（不推進）…' : '等待伺服器確認操作…'}</p>}
        {error && <ResearchNotice error={error} />}{session?.reason && <ResearchNotice error={session.reason} />}
      </div>
    </ResearchPanel>
    {session && <div hidden={chartMode !== 'recent'}><DigitalTwinScenePanel key={session.equipment_view?.id ?? 'unbound'} view={view} points={points} selectedEquipmentId={equipmentId} selectedBindingId={bindingId} onSelectEquipment={selectEquipment} onSelectBinding={selectBinding} motion={!session.frame ? 'unavailable' : stale ? 'stale' : execution === 'running' && active ? 'running' : 'paused'} source="simulation" visible={sceneVisible && active && chartMode === 'recent'} />
      <DigitalTwinProcess manifest={manifest} session={session} advancing={execution === 'running'} onFocus={selectVariable} /></div>}
    {session && <ResearchPanel title="工作階段曲線"><div className="space-y-3">
      <div role="group" aria-label="孿生曲線資料範圍" className="flex flex-wrap gap-2"><button type="button" className={researchButton} aria-pressed={chartMode === 'recent'} onClick={() => setChartMode('recent')}>最近掃描 · 最多 256 點</button><button type="button" className={researchButton} disabled={session.status !== 'completed'} aria-pressed={chartMode === 'archive'} onClick={() => setChartMode('archive')}>完成工作階段 · 完整封存</button></div>
      {focus && <div className="flex flex-wrap items-center gap-2 text-sm text-indigo-200"><span>設備／曲線聚焦：{focus.role} {focus.name}</span><button type="button" className={researchButton} onClick={() => setFocus(null)}>清除聚焦</button></div>}
      <div hidden={chartMode !== 'recent'}>{recent ? <ProcessTrendChart data={recent} storageKey={`authrity.twin.recent.${id}`} focusedVariableId={focus ? recent.variables.find(variable => variable.role === focus.role && variable.name === focus.name && variable.unit === focus.unit)?.id : null} onVariableFocus={selectVariable} initialView="role_blocks" /> : <p className="text-sm text-slate-400">尚無可繪製的完成掃描；不補造數值。</p>}</div>
      {session.status === 'completed' && <div hidden={chartMode !== 'archive'}><ResearchField label="比較已完成工作階段"><select className={researchControl} value={comparison} onChange={event => setComparison(event.target.value)}><option value="">不比較</option>{manifest.sessions.filter(item => item.id !== id && item.status === 'completed').map(item => { const why = sessionCompatibility(session, item); return <option key={item.id} value={item.id} disabled={!!why}>{item.trial_id} · {item.id}{why ? ` · 不相容：${why}` : ' · 可比較'}</option>; })}</select></ResearchField>
        {incompatibility ? <ResearchNotice error={`比較已阻止：${incompatibility}`} /> : <DigitalTwinRecordedScene manifest={manifest} trialId={session.trial_id} sources={archives} session={session} viewVersion={session.equipment_view} storageKey={`authrity.twin.archive.${id}`} sceneVisible={sceneVisible && active && chartMode === 'archive'} />}
        {other && !incompatibility && <DigitalTwinIdentity session={other} />}
      </div>}
    </div></ResearchPanel>}
  </div>;
}
