import { useEffect, useState } from 'react';
import { runtimeError, workspaceRequest } from '../lib/runtimeWorkspace';
import type { DeploymentProfile, RuntimeTarget } from '../types/RuntimeWorkspace';

const input = 'min-h-10 w-full rounded border border-slate-700 bg-slate-950 px-3 py-2 text-sm';
const button = 'min-h-10 rounded border border-slate-600 bg-slate-900 px-3 py-2 text-sm hover:border-indigo-400 disabled:opacity-40';
type Parameter = { pointer: string; label: string; value: number };
type SourcePolicy = { id: string; revision: number; definition_sha256: string; study_id: string; publication_template_version_id: string; name: string;
  profile_hash: string; baseline_model_sha256: string; tuning_parameters: Parameter[]; action: string; enabled: boolean; candidate_mode: string };
type Bounds = { pointer: string; minimum: number; maximum: number; max_delta: number };
type Envelope = { tag: string; minimum: number; maximum: number };
type PolicySpec = { profile_hash: string; target: RuntimeTarget; baseline_candidate_hash: string; baseline_model_sha256: string;
  source_policy_id: string; source_policy_revision: number; source_policy_sha256: string; source_study_id: string;
  allowlist: Bounds[]; operating_envelope: Envelope[]; minimum_observation_ms: number; minimum_good_samples: number;
  cooldown_ms: number; max_changes: number; expires_at: string };
type PolicyRecord = { policy_id: string; revision: number; state: 'enabled' | 'paused' | 'revoked'; spec: PolicySpec; created_by: number;
  spec_author_id: number; grantor_id: number | null; revision_by: number; created_at: string; reason: string };
type Progress = { policy_id: string; revision: number; good_samples: number; good_observation_ms: number; changes_reserved: number; budget_remaining: number; grant_current: boolean; last_observation: string | null };
type Snapshot = { policies: PolicyRecord[]; jobs: Record<string, unknown>[]; decisions: Record<string, unknown>[]; source_policies: SourcePolicy[];
  source_policy_status: { status: string; reason?: string }; progress: Progress[]; scheduler: { status: string; observed_at: string | null; fresh: boolean; interval_seconds: number } };
type BoundDraft = { enabled: boolean; minimum: string; maximum: string; delta: string };

export function RuntimePublisher({ profile, permissions, userId, active, candidateId }: { profile: DeploymentProfile; permissions: string[]; userId: number; active: boolean; candidateId: string }) {
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null); const [revision, setRevision] = useState(0);
  const [sourceId, setSourceId] = useState(''); const [editing, setEditing] = useState<PolicyRecord | null>(null);
  const [bounds, setBounds] = useState<Record<string, BoundDraft>>({});
  const [envelope, setEnvelope] = useState<Record<string, { minimum: string; maximum: string }>>({});
  const [observation, setObservation] = useState(''); const [samples, setSamples] = useState(''); const [cooldown, setCooldown] = useState(''); const [changes, setChanges] = useState(''); const [expires, setExpires] = useState('');
  const [reason, setReason] = useState(''); const [error, setError] = useState(''); const [busy, setBusy] = useState(false); const [notice, setNotice] = useState('');
  useEffect(() => {
    if (!active) return;
    const controller = new AbortController(); let timer: number | undefined;
    async function load() {
      try { const value = await workspaceRequest<Snapshot>('/api/runtime/publisher', undefined, controller.signal); if (controller.signal.aborted) return; setSnapshot(value); setError(''); }
      catch (cause) { if (!controller.signal.aborted) setError(runtimeError(cause)); }
      if (!controller.signal.aborted) timer = window.setTimeout(() => void load(), 5000);
    }
    void load(); return () => { controller.abort(); window.clearTimeout(timer); };
  }, [active, revision]);
  const sources = snapshot?.source_policies.filter(source => source.profile_hash === profile.profile_hash) ?? [];
  const source = sources.find(item => item.id === sourceId);
  const policies = snapshot?.policies.filter(policy => policy.spec.profile_hash === profile.profile_hash) ?? [];
  const canEdit = permissions.includes('runtime.publish'); const canGrant = permissions.includes('runtime.publish.grant');
  const envelopeFields = profile.variables.map(variable => ({ tag: variable.pv_tag, label: `${variable.role} · ${variable.id} [${variable.unit}]`, low: variable.hard_low, high: variable.hard_high }));
  function selectSource(id: string) { setSourceId(id); setEditing(null); setBounds({}); setEnvelope({}); setError(''); setNotice(''); }
  function editPolicy(policy: PolicyRecord) {
    setEditing(policy); setSourceId(policy.spec.source_policy_id);
    setBounds(Object.fromEntries(policy.spec.allowlist.map(bound => [bound.pointer, { enabled: true, minimum: String(bound.minimum), maximum: String(bound.maximum), delta: String(bound.max_delta) }])));
    setEnvelope(Object.fromEntries(policy.spec.operating_envelope.map(bound => [bound.tag, { minimum: String(bound.minimum), maximum: String(bound.maximum) }])));
    setObservation(String(policy.spec.minimum_observation_ms / 1000)); setSamples(String(policy.spec.minimum_good_samples)); setCooldown(String(policy.spec.cooldown_ms / 1000)); setChanges(String(policy.spec.max_changes));
    const date = new Date(policy.spec.expires_at); setExpires(new Date(date.getTime() - date.getTimezoneOffset() * 60000).toISOString().slice(0, 16)); setError(''); setNotice('修訂會產生新版本並回到暫停；需要獨立審查者重新啟用。');
  }
  function number(text: string, label: string, integer = false) {
    const value = text.trim() ? Number(text) : NaN;
    if (!Number.isFinite(value) || (integer && !Number.isSafeInteger(value))) throw new Error(`${label} 必須是${integer ? '整數' : '有限數值'}。`);
    return value;
  }
  async function request(path: string, body: unknown, message: string) {
    if (busy) return; setBusy(true); setError(''); setNotice('');
    try { await workspaceRequest(path, body); setNotice(message); setRevision(value => value + 1); }
    catch (cause) { setError(`${runtimeError(cause)} 請重新查詢政策版本；系統不自動重送。`); }
    finally { setBusy(false); }
  }
  async function save() {
    if (!source || !canEdit || !reason.trim()) return;
    try {
      const allowlist: Bounds[] = source.tuning_parameters.filter(parameter => bounds[parameter.pointer]?.enabled).map(parameter => {
        const draft = bounds[parameter.pointer]; const minimum = number(draft.minimum, parameter.label); const maximum = number(draft.maximum, parameter.label); const delta = number(draft.delta, '最大變更量');
        if (minimum > maximum || delta <= 0) throw new Error('參數範圍必須有序，最大單次變更量必須為正。');
        return { pointer: parameter.pointer, minimum, maximum, max_delta: delta };
      });
      if (!allowlist.length) throw new Error('至少明確授權一個調參欄位；其他模型欄位必須保持不變。');
      const operating: Envelope[] = envelopeFields.map(field => {
        const draft = envelope[field.tag]; const minimum = number(draft?.minimum ?? '', field.label); const maximum = number(draft?.maximum ?? '', field.label);
        if (minimum >= maximum || minimum < field.low || maximum > field.high) throw new Error(`${field.label} 的政策工況範圍不得超出核准工程硬界線。`);
        return { tag: field.tag, minimum, maximum };
      });
      const date = new Date(expires); if (!Number.isFinite(date.getTime()) || date.getTime() <= Date.now()) throw new Error('請設定未來的政策到期時間。');
      const spec: PolicySpec = { profile_hash: profile.profile_hash, target: profile.target, baseline_candidate_hash: source.publication_template_version_id,
        baseline_model_sha256: source.baseline_model_sha256, source_policy_id: source.id, source_policy_revision: source.revision,
        source_policy_sha256: source.definition_sha256, source_study_id: source.study_id, allowlist, operating_envelope: operating,
        minimum_observation_ms: number(observation, '有效觀察秒數') * 1000, minimum_good_samples: number(samples, '有效樣本數', true),
        cooldown_ms: number(cooldown, '冷卻秒數') * 1000, max_changes: number(changes, '最大發布次數', true), expires_at: date.toISOString() };
      if (spec.minimum_observation_ms <= 0 || spec.minimum_good_samples < 2 || spec.cooldown_ms <= 0 || spec.max_changes < 1) throw new Error('觀察時間、冷卻時間與次數必須為正，至少需要兩個真正有效樣本。');
      if (!window.confirm(`保存${editing ? '修訂' : '新'}政策為暫停狀態？\n只允許 ${allowlist.length} 個明確參數；最多 ${spec.max_changes} 次。\n不會自行啟動、Arm 或接管停止／Hold 的控制器。`)) return;
      if (editing) await request(`/api/runtime/publisher/policies/${encodeURIComponent(editing.policy_id)}/revise`, { expected_revision: editing.revision, spec, reason: reason.trim() }, '新政策版本已保存並暫停；請獨立審查。');
      else await request('/api/runtime/publisher/policies', { policy_id: crypto.randomUUID(), expected_revision: 0, spec, reason: reason.trim() }, '政策已保存並暫停；請獨立審查。');
    } catch (cause) { setError(runtimeError(cause)); }
  }
  async function changeState(policy: PolicyRecord, state: PolicyRecord['state']) {
    if (!reason.trim() || busy) return;
    if (!window.confirm(`${state === 'enabled' ? '獨立核准並啟用' : state === 'paused' ? '暫停' : '永久撤銷'}政策 ${policy.policy_id} 第 ${policy.revision} 版？\n啟用後，只能在現有健康、已 Arm 且已 Engage 的控制器上執行範圍內交接。`)) return;
    await request(`/api/runtime/publisher/policies/${encodeURIComponent(policy.policy_id)}/state`, { expected_revision: policy.revision, state, reason: reason.trim() }, '政策狀態已更新；現場控制權沒有因此被啟動或解鎖。');
  }
  return <section className="space-y-4 rounded-xl border border-indigo-900 p-4" aria-label="有界自動發布政策">
    <div><h2 className="font-medium text-indigo-200">持續最佳化 · 有界發布政策</h2><p className="mt-1 text-xs text-slate-400">M9 產生候選；此政策只授權已審核範圍內的參數變更。新模型動態、Node ID、Ts、工程硬限制與控制權不會隨搜尋結果自動改變。</p></div>
    <p className="text-xs text-slate-400">排程器：{snapshot?.scheduler.fresh ? snapshot.scheduler.status : '未取得新鮮心跳'} · {snapshot?.scheduler.observed_at ?? '未知'}</p>
    {snapshot?.source_policy_status.status !== 'available' && <p className="text-sm text-amber-200">{snapshot?.source_policy_status.reason ?? '正在查詢可用的研究政策來源。'}</p>}
    {error && <p role="alert" className="text-sm text-rose-300">{error}</p>}{notice && <p role="status" className="text-sm text-emerald-300">{notice}</p>}
    <label className="block space-y-1 text-sm">操作／授權原因<textarea className={input} value={reason} maxLength={1024} onChange={event => setReason(event.target.value)} /></label>
    <div className="space-y-3">{policies.map(policy => {
      const progress = snapshot?.progress.find(item => item.policy_id === policy.policy_id && item.revision === policy.revision);
      return <article key={policy.policy_id} className="space-y-2 rounded border border-slate-700 p-3"><div className="flex flex-wrap justify-between gap-2 text-sm"><strong>{policy.state} · 第 {policy.revision} 版</strong><span className="break-all font-mono text-xs">{policy.policy_id}</span></div><p className="text-xs text-slate-400">有效觀察 {progress?.good_samples ?? '未知'} 樣本 ／ {progress ? Math.round(progress.good_observation_ms / 1000) : '未知'} 秒 · 剩餘發布額度 {progress?.budget_remaining ?? '未知'} · 授權 {progress?.grant_current ? '有效' : '尚未確認／失效'}</p><div className="flex flex-wrap gap-2"><button type="button" className={button} disabled={!canEdit || busy || policy.state === 'revoked'} onClick={() => editPolicy(policy)}>載入為修訂草稿</button><button type="button" className={button} disabled={!canGrant || busy || !reason.trim() || policy.state === 'revoked' || policy.state === 'enabled' || policy.spec_author_id === userId || policy.created_by === userId} onClick={() => void changeState(policy, 'enabled')}>獨立核准並啟用</button><button type="button" className={button} disabled={(!canEdit && !canGrant) || busy || !reason.trim() || policy.state !== 'enabled'} onClick={() => void changeState(policy, 'paused')}>暫停後續發布</button><button type="button" className={button} disabled={!canGrant || busy || !reason.trim() || policy.state === 'revoked'} onClick={() => void changeState(policy, 'revoked')}>撤銷政策</button><button type="button" className={button} disabled={!canEdit || busy || !reason.trim() || policy.state !== 'enabled' || !/^[a-f0-9]{64}$/.test(candidateId)} onClick={() => { if (window.confirm('將上方明確選擇的候選排入此政策？伺服器仍會核對精確來源、限制與原生 instance／epoch，不會重試過期條件。')) void request(`/api/runtime/publisher/policies/${encodeURIComponent(policy.policy_id)}/queue`, { expected_revision: policy.revision, candidate_hash: candidateId, reason: reason.trim() }, '候選已提交政策核對；不代表已交接。'); }}>核對並排入上方候選</button></div><details><summary className="cursor-pointer text-xs text-slate-400">凍結政策與授權證據</summary><pre className="mt-2 max-h-80 overflow-auto whitespace-pre-wrap break-all text-xs">{JSON.stringify(policy, null, 2)}</pre></details></article>;
    })}</div>
    <details className="rounded border border-slate-700 p-3"><summary className="cursor-pointer text-sm">{editing ? `修訂政策 ${editing.policy_id}` : '建立新的有界發布政策'}</summary><form className="mt-4 space-y-4" onSubmit={event => { event.preventDefault(); void save(); }}>
      <label className="block space-y-1 text-sm">已綁定參考版本的 M9 研究政策<select className={input} value={sourceId} onChange={event => selectSource(event.target.value)}><option value="">明確選擇研究政策</option>{sources.map(item => <option key={item.id} value={item.id}>{item.name || item.id} · 第 {item.revision} 版 · {item.action}</option>)}</select></label>
      {source && <><p className="break-all text-xs text-slate-400">精確參考版本 {source.publication_template_version_id} · 研究 {source.study_id} · 政策第 {source.revision} 版</p><div className="overflow-x-auto"><table className="w-full min-w-[600px] text-left text-xs"><thead><tr>{['允許變更', '目前參考值', '最小值', '最大值', '單次最大變更'].map(label => <th key={label} className="p-2 font-normal text-slate-400">{label}</th>)}</tr></thead><tbody>{source.tuning_parameters.map(parameter => {
        const draft = bounds[parameter.pointer] ?? { enabled: false, minimum: '', maximum: '', delta: '' };
        return <tr key={parameter.pointer} className="border-t border-slate-800"><td className="p-2"><label className="flex gap-2"><input type="checkbox" checked={draft.enabled} onChange={event => setBounds(current => ({ ...current, [parameter.pointer]: { ...draft, enabled: event.target.checked } }))} />{parameter.label}</label></td><td className="p-2 tabular-nums">{parameter.value}</td>{(['minimum', 'maximum', 'delta'] as const).map(field => <td key={field} className="p-2"><input aria-label={`${parameter.label} ${field}`} className={input} type="number" step="any" disabled={!draft.enabled} value={draft[field]} onChange={event => setBounds(current => ({ ...current, [parameter.pointer]: { ...draft, [field]: event.target.value } }))} required={draft.enabled} /></td>)}</tr>;
      })}</tbody></table></div><fieldset className="space-y-2"><legend className="text-sm">允許自動交接的實際工況</legend>{envelopeFields.map(field => <div key={field.tag} className="grid items-center gap-2 sm:grid-cols-3"><span className="text-xs">{field.label}<span className="block text-slate-500">工程界線 {field.low} ～ {field.high}</span></span><input aria-label={`${field.label} 政策下限`} className={input} type="number" step="any" min={field.low} max={field.high} value={envelope[field.tag]?.minimum ?? ''} onChange={event => setEnvelope(current => ({ ...current, [field.tag]: { minimum: event.target.value, maximum: current[field.tag]?.maximum ?? '' } }))} required /><input aria-label={`${field.label} 政策上限`} className={input} type="number" step="any" min={field.low} max={field.high} value={envelope[field.tag]?.maximum ?? ''} onChange={event => setEnvelope(current => ({ ...current, [field.tag]: { minimum: current[field.tag]?.minimum ?? '', maximum: event.target.value } }))} required /></div>)}</fieldset></>}
      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">{([{ label: '至少有效觀察秒數', value: observation, set: setObservation }, { label: '至少有效樣本數', value: samples, set: setSamples }, { label: '發布冷卻秒數', value: cooldown, set: setCooldown }, { label: '最多發布次數', value: changes, set: setChanges }]).map(field => <label key={field.label} className="space-y-1 text-sm">{field.label}<input className={input} type="number" min="1" step="1" value={field.value} onChange={event => field.set(event.target.value)} required /></label>)}</div><label className="block space-y-1 text-sm">政策到期（本機時區）<input className={input} type="datetime-local" value={expires} onChange={event => setExpires(event.target.value)} required /></label><div className="flex gap-2"><button type="submit" className={button} disabled={!canEdit || !source || busy || !reason.trim()}>保存為暫停，等待獨立核准</button>{editing && <button type="button" className={button} onClick={() => selectSource('')}>取消修訂草稿</button>}</div>
    </form></details>
    <details className="rounded border border-slate-700 p-3"><summary className="cursor-pointer text-sm">排程決策與不可重送的交接工作</summary><pre className="mt-2 max-h-96 overflow-auto whitespace-pre-wrap break-all text-xs">{JSON.stringify({ jobs: snapshot?.jobs ?? [], decisions: snapshot?.decisions ?? [] }, null, 2)}</pre></details>
  </section>;
}
