import { useEffect, useState } from 'react';
import type { RuntimeOperationTracker } from '../hooks/useRuntimeOperation';
import { researchRequest } from '../lib/research';
import { runtimeError, targetLabel, workspaceRequest } from '../lib/runtimeWorkspace';
import type { ControlVersion } from '../types/ControlVersion';
import type { DeploymentProfile, RuntimeProfileSnapshot, RuntimeTarget } from '../types/RuntimeWorkspace';
import { RuntimeOperationStatus } from './RuntimeControl';
import { RuntimePublisher } from './RuntimePublisher';

const input = 'min-h-10 w-full rounded border border-slate-700 bg-slate-950 px-3 py-2 text-sm';
const button = 'min-h-10 rounded border border-slate-600 bg-slate-900 px-3 py-2 text-sm hover:border-indigo-400 disabled:opacity-40';
type BoundVersion = ControlVersion & { author_user_id?: number; author_identity_realm?: string; deployment_profile_hash?: string; target?: RuntimeTarget; equipment_instance_ids?: string[]; operating_envelope?: string };
type Approval = { approval_id: string; revoked_at: string | null; binding: { profile_hash: string; candidate_hash: string; model_sha256: string; author_id: number; approver_id: number; expires_at: string } };

type Props = { profile: DeploymentProfile; snapshot?: RuntimeProfileSnapshot; permissions: string[]; userId: number;
  tracker: RuntimeOperationTracker; active: boolean; connected: boolean };

export function RuntimeVersions({ profile, snapshot, permissions, userId, tracker, active, connected }: Props) {
  const [versions, setVersions] = useState<BoundVersion[]>([]); const [selectedId, setSelectedId] = useState('');
  const [approvals, setApprovals] = useState<Approval[]>([]); const [approvalId, setApprovalId] = useState('');
  const [expiresAt, setExpiresAt] = useState(''); const [reason, setReason] = useState('');
  const [error, setError] = useState(''); const [busy, setBusy] = useState(false); const [revision, setRevision] = useState(0);
  useEffect(() => {
    if (!active) return;
    const controller = new AbortController(); setError('');
    void Promise.all([researchRequest<{ versions: BoundVersion[] }>('/versions', undefined, controller.signal),
      workspaceRequest<{ approvals: Approval[]; truncated: boolean }>('/api/runtime/approvals', undefined, controller.signal)])
      .then(([library, grants]) => { if (controller.signal.aborted) return; setVersions(library.versions); setApprovals(grants.approvals); })
      .catch(cause => { if (!controller.signal.aborted) setError(runtimeError(cause)); });
    return () => controller.abort();
  }, [active, revision]);
  const selected = versions.find(version => version.id === selectedId);
  const matchesTarget = !!selected && selected.deployment_profile_hash === profile.profile_hash
    && selected.target?.plant === profile.target.plant && selected.target.line === profile.target.line
    && selected.target.area === profile.target.area && selected.target.group === profile.target.group
    && selected.operating_envelope === profile.operating_envelope
    && Array.isArray(selected.equipment_instance_ids) && selected.equipment_instance_ids.length === profile.equipment_instance_ids.length
    && profile.equipment_instance_ids.every(id => selected.equipment_instance_ids!.includes(id));
  const eligibleApprovals = approvals.filter(approval => approval.binding.profile_hash === profile.profile_hash && approval.binding.candidate_hash === selectedId
    && approval.binding.model_sha256 === selected?.model_sha256 && !approval.revoked_at && Date.parse(approval.binding.expires_at) > Date.now());
  const selectedApproval = eligibleApprovals.find(approval => approval.approval_id === approvalId);
  const canDeploy = connected && permissions.includes('runtime.deploy') && matchesTarget && !!selectedApproval && !tracker.unresolved && !tracker.busy;
  async function approve() {
    if (busy || !selected || !matchesTarget || !permissions.includes('runtime.approve')) return;
    const expires = new Date(expiresAt);
    if (!Number.isFinite(expires.getTime()) || expires.getTime() <= Date.now()) { setError('請設定未來的核准到期時間。'); return; }
    if (!window.confirm(`核准此不可變候選？\n${targetLabel(profile.target)}\n版本 ${selected.id}\n只記錄核准，不安裝、不啟動、不解鎖。`)) return;
    setBusy(true); setError('');
    try { const result = await workspaceRequest<{ approval_id: string }>('/api/runtime/approvals', { profile_hash: profile.profile_hash, candidate_hash: selected.id, expires_at: expires.toISOString() }); setApprovalId(result.approval_id); setRevision(value => value + 1); }
    catch (cause) { setError(runtimeError(cause)); } finally { setBusy(false); }
  }
  async function revoke(id: string) {
    if (busy || !reason.trim() || !permissions.includes('runtime.approve')) return;
    if (!window.confirm('撤銷此核准？新的安裝與交接將被禁止；不會自動停止已運轉的控制器。')) return;
    setBusy(true); setError('');
    try { await workspaceRequest(`/api/runtime/approvals/${encodeURIComponent(id)}/revoke`, { reason: reason.trim() }); setRevision(value => value + 1); }
    catch (cause) { setError(runtimeError(cause)); } finally { setBusy(false); }
  }
  async function deploy(kind: 'install' | 'switch_version' | 'start_candidate') {
    if (!canDeploy || !selected || !selectedApproval || !snapshot || !reason.trim()) return;
    if (kind === 'switch_version' && (!snapshot.instance_id || snapshot.health !== 'good' || !permissions.includes('runtime.operate'))) return;
    if (kind === 'start_candidate' && (snapshot.instance_id || !permissions.includes('runtime.operate'))) return;
    const explanation = kind === 'install' ? '只安裝不可變模型；不啟動、不改變現場控制權。'
      : kind === 'start_candidate' ? '以已安裝且獨立核准的候選啟動未解鎖的 Monitor。必須累積足夠真實歷史；不會自動 Arm 或 Engage。'
        : '要求原生安全交接；舊控制器將先凍結移動，核對歷史狀態重建與第一步移動，失敗不會自動重新接管。';
    if (!window.confirm(`${explanation}\n${targetLabel(profile.target)}\n目前 ${snapshot.version}\n候選 ${selected.model_sha256}\n原因：${reason.trim()}`)) return;
    try {
      const common = { operation_id: crypto.randomUUID(), profile_hash: profile.profile_hash,
        expires_at: new Date(Date.now() + (kind === 'install' ? 55_000 : 300_000)).toISOString(), reason: reason.trim() };
      if (kind === 'install') await tracker.submit(`/api/control/versions/${encodeURIComponent(selected.id)}/install`, { ...common, approval_id: selectedApproval.approval_id, expected_epoch: snapshot.registry_epoch });
      else await tracker.submit('/api/runtime/operations', { ...common,
        expected_version: kind === 'start_candidate' ? profile.model_sha256 : snapshot.version,
        expected_epoch: kind === 'start_candidate' ? snapshot.registry_epoch : snapshot.epoch,
        expected_instance: kind === 'start_candidate' ? null : snapshot.instance_id,
        action: { kind, candidate_hash: selected.id, approval_id: selectedApproval.approval_id } });
    } catch (cause) { setError(runtimeError(cause)); }
  }
  return <div className="space-y-4">
    <section className="space-y-3 rounded-xl border border-slate-700 p-4"><h2 className="font-medium">不可變候選 · 精確綁定設備與控制群組</h2><p className="text-xs text-slate-400">安裝與安全交接是兩個獨立動作。研究成功、核准、安裝或 OPC 訂閱值，都不等於現場已接管。</p><label className="block space-y-1 text-sm">候選版本<select className={input} value={selectedId} onChange={event => { setSelectedId(event.target.value); setApprovalId(''); }}><option value="">明確選擇已送審版本</option>{versions.map(version => <option key={version.id} value={version.id}>{version.process_area} · {version.method} · {version.trial_id} · {version.created_at}</option>)}</select></label><button type="button" className={button} disabled={busy} onClick={() => setRevision(value => value + 1)}>重新讀取版本與核准</button>
      {selected && <><dl className="grid gap-3 text-xs sm:grid-cols-2"><div><dt className="text-slate-400">候選 ID</dt><dd className="break-all font-mono">{selected.id}</dd></div><div><dt className="text-slate-400">模型 SHA-256</dt><dd className="break-all font-mono">{selected.model_sha256}</dd></div><div><dt className="text-slate-400">目前活動模型</dt><dd className="break-all font-mono">{snapshot?.version ?? '未知'}</dd></div><div><dt className="text-slate-400">實體目標</dt><dd>{selected.target ? targetLabel(selected.target) : '此舊版本沒有可核對的部署綁定'}</dd></div></dl><p className="text-xs text-amber-200">{selected.evidence_scope}</p>{!matchesTarget && <p role="alert" className="text-sm text-amber-200">候選沒有完全匹配目前 profile、設備或操作範圍，不能核准或發布。不得自動改名、補 Node ID 或更改模型位元組。</p>}<details><summary className="cursor-pointer text-sm">比較證據、參數與來源指紋</summary><pre className="mt-2 max-h-80 overflow-auto whitespace-pre-wrap break-all text-xs">{JSON.stringify({ parameters: selected.parameters, metrics: selected.metrics, model_test: selected.model_test, assessment: selected.assessment, source_hashes: selected.source_hashes }, null, 2)}</pre></details></>}
    </section>
    {selected && <section className="space-y-3 rounded-xl border border-slate-700 p-4"><h2 className="font-medium">獨立核准</h2>{selected.author_user_id === userId && <p className="text-sm text-amber-200">你是此候選的建立者，不能自行核准。請使用具此群組核准權的獨立審查者。</p>}<div className="flex flex-wrap items-end gap-3"><label className="space-y-1 text-sm">核准到期（本機時區）<input className={input} type="datetime-local" value={expiresAt} onChange={event => setExpiresAt(event.target.value)} /></label><button type="button" className={button} disabled={busy || !matchesTarget || !permissions.includes('runtime.approve') || selected.author_user_id === userId || selected.author_user_id === undefined} onClick={() => void approve()}>核准此版本，不安裝</button></div><label className="block space-y-1 text-sm">選擇未撤銷且未到期的核准<select className={input} value={approvalId} onChange={event => setApprovalId(event.target.value)}><option value="">明確選擇核准</option>{eligibleApprovals.map(approval => <option key={approval.approval_id} value={approval.approval_id}>審查者 {approval.binding.approver_id} · 到期 {approval.binding.expires_at} · {approval.approval_id.slice(0, 12)}</option>)}</select></label><label className="block space-y-1 text-sm">發布／撤銷原因<textarea className={input} value={reason} maxLength={1024} onChange={event => setReason(event.target.value)} /></label><div className="flex flex-wrap gap-2"><button type="button" className={button} disabled={!canDeploy || !snapshot || !reason.trim()} onClick={() => void deploy('install')}>安裝至核准主機，不啟動</button><button type="button" className={`${button} border-amber-700 text-amber-200`} disabled={!canDeploy || !snapshot?.instance_id || snapshot.health !== 'good' || !permissions.includes('runtime.operate') || !reason.trim()} onClick={() => void deploy('switch_version')}>預覽並要求安全交接</button>{selectedApproval && permissions.includes('runtime.approve') && <button type="button" className={button} disabled={busy || !reason.trim()} onClick={() => void revoke(selectedApproval.approval_id)}>撤銷選定核准</button>}</div></section>}
    {selected && <section className="space-y-2 rounded border border-slate-700 p-3">
      <button type="button" className={button} disabled={!canDeploy || !snapshot || !!snapshot.instance_id || !permissions.includes('runtime.operate') || !reason.trim()}
        onClick={() => void deploy('start_candidate')}>以已安裝候選啟動 Monitor</button>
      <p className="text-xs text-slate-400">適用於目前沒有引擎程序的控制群組。伺服器會核對安裝回條、獨立核准與原始投運 profile；啟動不等於接管。已有程序時請使用安全交接，不另啟第二個寫入者。</p>
    </section>}
    {error && <p role="alert" className="rounded border border-rose-800 p-3 text-sm text-rose-300">{error}</p>}
    <RuntimeOperationStatus tracker={tracker} />
    <RuntimePublisher key={profile.profile_hash} profile={profile} permissions={permissions} userId={userId} active={active} candidateId={selectedId} />
  </div>;
}
