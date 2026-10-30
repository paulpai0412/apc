import { useEffect, useRef, useState } from 'react';
import type { RuntimeOperationTracker } from '../hooks/useRuntimeOperation';
import { runtimeError, runtimeReadinessAllows, targetLabel, workspaceRequest } from '../lib/runtimeWorkspace';
import type { DeploymentProfile, RuntimeOperation, RuntimeOperationAction, RuntimeOperationRequest, RuntimeProfileSnapshot } from '../types/RuntimeWorkspace';

const input = 'min-h-10 w-full rounded border border-slate-700 bg-slate-950 px-3 py-2 text-sm';
const button = 'min-h-10 rounded border border-slate-600 bg-slate-900 px-3 py-2 text-sm hover:border-indigo-400 disabled:cursor-not-allowed disabled:opacity-40';
const labels = { start: '啟動基準程序', recover_monitor: '事故後重建監測程序', monitor: '開始監測 Monitor', arm: '明確解鎖 Arm', engage: '接管 Engage', hold: '暫停新控制移動 Hold', exit: '退出並交還 DCS', stop: '交接後停止引擎' };
const states = { executing: '執行中', accepted: '此操作結果已確認', rejected: '已拒絕', unknown: '結果未知 · 禁止重送' };

type Props = { profile: DeploymentProfile; snapshot?: RuntimeProfileSnapshot; permissions: string[]; connected: boolean;
  lifecycle: boolean; active: boolean; tracker: RuntimeOperationTracker; operations: RuntimeOperation[] };

export function RuntimeOperationStatus({ tracker }: { tracker: RuntimeOperationTracker }) {
  if (!tracker.intent && !tracker.error) return null;
  return <section aria-label="操作回條" className="space-y-2 rounded border border-slate-700 p-3 text-sm">
    <p className={tracker.unresolved ? 'text-amber-200' : 'text-slate-200'}>{tracker.record ? tracker.record.state === 'unknown' && tracker.record.blocking === false ? '原結果仍未知 · 目前安全基線已獨立確認；原意圖不可重送' : states[tracker.record.state] : '尚未確認操作結果'}</p>
    {tracker.intent && <p className="break-all font-mono text-xs">Operation ID：{tracker.intent.body.operation_id}</p>}
    {tracker.error && <p role="alert" className="text-rose-300">{tracker.error}</p>}
    {tracker.record && <details><summary className="cursor-pointer text-xs text-slate-400">伺服器證據與原始回條</summary><pre className="mt-2 max-h-60 overflow-auto whitespace-pre-wrap break-all text-xs">{tracker.record.detail}</pre></details>}
    <button type="button" className={button} disabled={tracker.busy || !tracker.intent} onClick={tracker.refresh}>查詢原操作，不重送</button>
    {tracker.intent && !tracker.record && <button type="button" className={button} disabled={tracker.busy} onClick={() => void tracker.reconcile()}>到期後核對是否曾派送</button>}
  </section>;
}

export function RuntimeControl({ profile, snapshot, permissions, connected, lifecycle, active, tracker, operations }: Props) {
  const [kind, setKind] = useState<'cv_target' | 'operating_low' | 'operating_high'>('cv_target');
  const [variableId, setVariableId] = useState('');
  const [value, setValue] = useState('');
  const [reason, setReason] = useState('');
  const [preview, setPreview] = useState<RuntimeOperationRequest | null>(null);
  const [acknowledged, setAcknowledged] = useState(false);
  const [error, setError] = useState('');
  const [clearing, setClearing] = useState(false);
  const [clearance, setClearance] = useState<unknown>(null);
  const dialog = useRef<HTMLDialogElement>(null);
  const [clock, setClock] = useState(Date.now());
  const canOperate = connected && permissions.includes('runtime.operate');
  const healthy = snapshot?.health === 'good';
  const unknownOperations = operations.filter(operation => operation.profile_hash === profile.profile_hash && operation.state === 'unknown' && operation.blocking !== false);
  const unresolved = tracker.unresolved || operations.some(operation => operation.profile_hash === profile.profile_hash && (operation.state === 'executing' || (operation.state === 'unknown' && operation.blocking !== false)));
  const variables = profile.variables.filter(variable => kind === 'cv_target' ? variable.role === 'CV' : variable.role !== 'DV');
  const selected = variables.find(variable => variable.id === variableId);
  const previewStartsProcess = preview?.action.kind === 'start' || preview?.action.kind === 'recover_monitor';
  const identityChanged = !snapshot || (preview && (preview.profile_hash !== profile.profile_hash
    || preview.expected_version !== (previewStartsProcess ? profile.model_sha256 : snapshot.version)
    || preview.expected_epoch !== (previewStartsProcess ? snapshot.registry_epoch : snapshot.epoch)
    || preview.expected_instance !== (previewStartsProcess ? null : snapshot.instance_id)
    || (previewStartsProcess && snapshot.instance_id !== null)));
  useEffect(() => {
    if (active && preview) { if (!dialog.current?.open) dialog.current?.showModal(); }
    else if (dialog.current?.open) dialog.current.close();
  }, [active, preview]);
  useEffect(() => {
    if (!preview || !active) return;
    const timer = window.setInterval(() => setClock(Date.now()), 250);
    return () => window.clearInterval(timer);
  }, [preview, active]);

  function prepare(action: RuntimeOperationAction) {
    setError(''); setAcknowledged(false);
    if (!snapshot || !canOperate || !reason.trim()) { setError('需要明確的核准目標、有效狀態與操作原因。'); return; }
    if (!runtimeReadinessAllows(action.kind, healthy, unresolved)) { setError('一般操作需要健康的原生控制狀態，且不能有未釐清的操作。'); return; }
    try {
      const request: RuntimeOperationRequest = { operation_id: crypto.randomUUID(), profile_hash: profile.profile_hash,
        expected_version: action.kind === 'start' || action.kind === 'recover_monitor' ? profile.model_sha256 : snapshot.version,
        expected_epoch: action.kind === 'start' || action.kind === 'recover_monitor' ? snapshot.registry_epoch : snapshot.epoch,
        expected_instance: action.kind === 'start' || action.kind === 'recover_monitor' ? null : snapshot.instance_id,
        expires_at: new Date(Date.now() + 300_000).toISOString(), reason: reason.trim(), action };
      setClock(Date.now()); setPreview(request);
    } catch { setError('瀏覽器無法建立安全的操作識別，尚未送出。請使用受信任的 HTTPS 或 loopback 介面。'); }
  }
  function prepareValue() {
    const numeric = value.trim() ? Number(value) : NaN;
    if (!selected || !Number.isFinite(numeric) || numeric < selected.hard_low || numeric > selected.hard_high) {
      setError('請選擇變數並輸入工程硬限制內的有限數值。'); return;
    }
    prepare({ kind, variable_id: selected.id, value: numeric });
  }
  async function confirm() {
    if (!preview || !acknowledged || tracker.busy || identityChanged || Date.parse(preview.expires_at) <= Date.now() || !active) return;
    const safetyRelease = preview.action.kind === 'hold' || preview.action.kind === 'exit' || preview.action.kind === 'stop' || preview.action.kind === 'recover_monitor';
    const request = preview; setPreview(null);
    await tracker.submit('/api/runtime/operations', { ...request }, safetyRelease);
  }

  async function clearInterlock() {
    if (clearing || !snapshot || !canOperate || !reason.trim() || !unknownOperations.length
      || snapshot.native_status?.safe_baseline !== true || snapshot.native_status.writer_owner !== true
      || snapshot.native_status.pending_application !== false || snapshot.arming || snapshot.dcs_handoff !== true) return;
    if (!window.confirm(`核對並記錄目前安全基線？\n${targetLabel(profile.target)}\n涉及 ${unknownOperations.length} 個未知操作。\n這不會把原結果改成成功或失敗，也不會重新派送、Arm 或 Engage。`)) return;
    setClearing(true); setError(''); setClearance(null);
    try {
      const result = await workspaceRequest<unknown>('/api/runtime/clearances', { clearance_id: crypto.randomUUID(),
        profile_hash: profile.profile_hash, operation_ids: unknownOperations.map(operation => operation.operation_id),
        expected_instance: snapshot.instance_id, expected_epoch: snapshot.epoch, expected_version: snapshot.version,
        expires_at: new Date(Date.now() + 300_000).toISOString(), reason: reason.trim() });
      setClearance(result); tracker.refresh();
    } catch (cause) { setError(`${runtimeError(cause)} 請查詢原操作的 clearance 證據，不自動重送。`); }
    finally { setClearing(false); }
  }

  return <div className="space-y-4">
    <section className="grid gap-3 rounded-xl border border-slate-700 p-4 sm:grid-cols-2 lg:grid-cols-4" aria-label="控制權與接管條件">
      <div><p className="text-xs text-slate-400">原生控制權</p><p className="mt-1 font-semibold">{snapshot?.authority ?? '未知'}</p></div>
      <div><p className="text-xs text-slate-400">Arming</p><p className="mt-1">{snapshot ? snapshot.arming ? '已明確解鎖' : '未解鎖' : '未知'}</p></div>
      <div><p className="text-xs text-slate-400">DCS 接回證據</p><p className="mt-1">{snapshot?.dcs_handoff === true ? '原生引擎已確認' : snapshot?.dcs_handoff === false ? '尚未確認' : '未知'}</p></div>
      <div><p className="text-xs text-slate-400">採集／求解／應用健康</p><p className={healthy ? 'mt-1 text-emerald-300' : 'mt-1 text-amber-200'}>{snapshot?.health ?? '未知'}</p></div>
    </section>
    {snapshot?.blockers.length ? <ul role="alert" className="list-inside list-disc rounded border border-amber-800 p-3 text-sm text-amber-200">{snapshot.blockers.map(blocker => <li key={blocker}>{blocker}</li>)}</ul> : null}
    {!permissions.includes('runtime.operate') && <p className="rounded border border-amber-800 p-3 text-sm text-amber-200">此帳號沒有這個控制群組的操作授權。角色名稱不會取代 profile 範圍權限。</p>}
    <label className="block space-y-1 text-sm">操作原因<textarea className={input} value={reason} maxLength={1024} onChange={event => setReason(event.target.value)} placeholder="例如：依已核准產品配方調整目標" /></label>
    <section className="space-y-3 rounded-xl border border-slate-700 p-4" aria-label="控制生命週期">
      <h2 className="font-medium">生命週期 · 每次操作都需要確認</h2>
      <div className="flex flex-wrap gap-2">{(Object.keys(labels) as (keyof typeof labels)[]).map(action => {
        const release = action === 'hold' || action === 'exit' || action === 'stop' || action === 'recover_monitor';
        const processStart = action === 'start' || action === 'recover_monitor';
        const disabled = !canOperate || !lifecycle || tracker.busy || clearing || !reason.trim() || !snapshot
          || (processStart ? !!snapshot.instance_id : !snapshot.instance_id) || !runtimeReadinessAllows(action, healthy, unresolved);
        return <button type="button" key={action} className={`${button} ${release ? 'border-amber-800 text-amber-200' : ''}`} disabled={disabled} onClick={() => prepare({ kind: action })}>{labels[action]}</button>;
      })}</div>
      <p className="text-xs text-slate-400">Hold 只暫停新 MPC 移動，不代表 DCS 已接回。退出／停止必須取得獨立交接證據；所有安全條件由原生引擎再次驗證。</p>
    </section>
    {unknownOperations.length > 0 && <section className="space-y-3 rounded border border-amber-800 p-4" aria-label="未知操作安全基線核對">
      <h2 className="font-medium text-amber-200">結果未知 · {unknownOperations.length} 個操作仍形成聯鎖</h2>
      <p className="text-xs text-slate-400">先查詢原操作；若無法恢復回條，必須先退出／交還 DCS。原程序已退出時可明確重建未 Arm 的監測程序。只有新的原生安全基線與獨立 DCS 接回證據都有效，才可解除新操作聯鎖；原歷史結果仍保留未知。</p>
      <button type="button" className={button} disabled={clearing || tracker.busy || !canOperate || !reason.trim()
        || snapshot?.native_status?.safe_baseline !== true || snapshot.native_status.writer_owner !== true
        || snapshot.native_status.pending_application !== false || snapshot.arming || snapshot.dcs_handoff !== true}
        onClick={() => void clearInterlock()}>{clearing ? '核對獨立安全證據…' : '確認目前安全基線，不重送原操作'}</button>
    </section>}
    {clearance !== null && <details className="rounded border border-slate-700 p-3"><summary className="cursor-pointer text-sm">安全基線核對回條</summary><pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap break-all text-xs">{JSON.stringify(clearance, null, 2)}</pre></details>}
    <form className="space-y-3 rounded-xl border border-slate-700 p-4" onSubmit={event => { event.preventDefault(); prepareValue(); }}>
      <h2 className="font-medium">CV 目標與操作界線</h2>
      <div className="grid gap-3 md:grid-cols-3"><label className="space-y-1 text-sm">操作<select className={input} value={kind} onChange={event => { setKind(event.target.value as typeof kind); setVariableId(''); setValue(''); }}><option value="cv_target">CV 目標</option><option value="operating_low">操作下限</option><option value="operating_high">操作上限</option></select></label><label className="space-y-1 text-sm">變數<select className={input} value={variableId} onChange={event => { setVariableId(event.target.value); setValue(''); }}><option value="">明確選擇變數</option>{variables.map(variable => <option key={variable.id} value={variable.id}>{variable.role} · {variable.id} [{variable.unit}]</option>)}</select></label><label className="space-y-1 text-sm">新設定值<input className={input} type="number" step="any" value={value} onChange={event => setValue(event.target.value)} min={selected?.hard_low} max={selected?.hard_high} required /></label></div>
      {selected && <p className="text-xs text-slate-400">工程硬界線：{selected.hard_low} ～ {selected.hard_high} {selected.unit}。不可用操作界線覆寫工程硬限制；系統不提供任意 MV SP 寫入。</p>}
      <button type="submit" className={button} disabled={!canOperate || !healthy || unresolved || tracker.busy || !reason.trim()}>預覽變更，不立即寫入</button>
    </form>
    {error && <p role="alert" className="text-sm text-rose-300">{error}</p>}
    <RuntimeOperationStatus tracker={tracker} />
    <details className="rounded border border-slate-700 p-3"><summary className="cursor-pointer text-sm">完整原生狀態與操作紀錄</summary><pre className="mt-3 max-h-80 overflow-auto whitespace-pre-wrap break-all text-xs">{JSON.stringify({ native: snapshot?.native_status ?? null, operations: operations.filter(operation => operation.profile_hash === profile.profile_hash) }, null, 2)}</pre></details>
    <dialog ref={dialog} onCancel={() => setPreview(null)} className="m-auto max-h-[90dvh] w-[min(38rem,calc(100vw-2rem))] overflow-auto rounded-xl border border-slate-600 bg-slate-900 p-5 text-slate-100 backdrop:bg-black/70">
      {preview && <div className="space-y-4"><h2 className="text-lg font-semibold">確認現場操作</h2><dl className="space-y-2 text-sm"><div><dt className="text-slate-400">實體目標</dt><dd>{targetLabel(profile.target)}</dd></div><div><dt className="text-slate-400">設備</dt><dd>{profile.equipment_instance_ids.join('、')}</dd></div><div><dt className="text-slate-400">活動版本／Epoch</dt><dd className="break-all font-mono text-xs">{preview.expected_version} ／ {preview.expected_epoch}</dd></div><div><dt className="text-slate-400">操作</dt><dd>{'variable_id' in preview.action ? `${preview.action.variable_id} · ${preview.action.kind} → ${preview.action.value}` : preview.action.kind in labels ? labels[preview.action.kind as keyof typeof labels] : preview.action.kind}</dd></div><div><dt className="text-slate-400">原因</dt><dd>{preview.reason}</dd></div></dl><label className="flex items-start gap-2 text-sm"><input type="checkbox" className="mt-1" checked={acknowledged} onChange={event => setAcknowledged(event.target.checked)} />我確認這是上述控制群組及版本；了解其對現場控制權或設定值的影響。</label>{(identityChanged || Date.parse(preview.expires_at) <= clock) && <p role="alert" className="text-sm text-amber-200">狀態已改變或預覽已到期。請取消並重新核對，不會自動更新這份意圖。</p>}<div className="flex flex-wrap justify-end gap-2"><button className={button} type="button" onClick={() => setPreview(null)}>取消</button><button className={`${button} border-amber-600 text-amber-200`} type="button" disabled={!acknowledged || tracker.busy || !!identityChanged || Date.parse(preview.expires_at) <= clock} onClick={() => void confirm()}>確認送出一次</button></div></div>}
    </dialog>
  </div>;
}
