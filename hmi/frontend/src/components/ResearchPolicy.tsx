import { useEffect, useState } from 'react';
import { researchError, researchRequest } from '../lib/research';
import type { ResearchValue } from '../types/Research';
import type { ControlVersion } from '../types/ControlVersion';
import { ResearchDetails, ResearchField, ResearchNotice, ResearchNumber, ResearchPanel, researchButton, researchControl, researchPrimary } from './ResearchControls';

type PolicyAction = 'frozen_model_tuning' | 'reidentify_and_tune';
type PolicyTrigger = { kind: 'new_data' } | { kind: 'persistent_drift'; normalized_rmse: number; minimum_windows: number; minimum_seconds: number };
type Policy = { id: string; revision: number; name: string; study_id: string; model_id?: string; action?: PolicyAction; trigger?: PolicyTrigger; publication_template_version_id?: string; candidate_mode?: string; enabled: boolean; minimum_rows: number; max_filled_fraction: number; cooldown_seconds: number; max_runs: number; max_trials_total: number; created_at: string; state: { runs_started: number; trials_reserved: number; last_started_at?: string; last_decision?: ResearchValue; audit: ResearchValue[] } };
type Draft = Pick<Policy, 'name' | 'enabled' | 'minimum_rows' | 'max_filled_fraction' | 'cooldown_seconds' | 'max_runs' | 'max_trials_total'> & { action: PolicyAction; trigger: PolicyTrigger };
export function ResearchPolicy({ studyId, assignedModelId, supported, automaticReidentification = false }: { studyId: string; assignedModelId?: string; supported: boolean; automaticReidentification?: boolean }) {
  const [policies, setPolicies] = useState<Policy[]>([]);
  const [models, setModels] = useState<{ id: string; study_id: string }[]>([]);
  const [modelId, setModelId] = useState('');
  const [versions, setVersions] = useState<ControlVersion[]>([]);
  const [templateId, setTemplateId] = useState('');
  const [scheduler, setScheduler] = useState<ResearchValue>(null);
  const [draft, setDraft] = useState<Draft>({ name: '', action: 'frozen_model_tuning', trigger: { kind: 'new_data' }, enabled: false, minimum_rows: 1000, max_filled_fraction: 0, cooldown_seconds: 86400, max_runs: 1, max_trials_total: 128 });
  const [forkId, setForkId] = useState('');
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState('');
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    if (!supported) return;
    const controller = new AbortController();
    Promise.all([researchRequest<{ policies: Policy[]; scheduler: ResearchValue }>('/policies', undefined, controller.signal), researchRequest<{ models: { id: string; study_id: string }[] }>('/models', undefined, controller.signal), researchRequest<{ versions: ControlVersion[] }>('/versions', undefined, controller.signal)]).then(([data, artifacts, library]) => {
      if (!controller.signal.aborted) { setPolicies(data.policies); setScheduler(data.scheduler); setModels(artifacts.models); setVersions(library.versions); setError(''); }
    }).catch(failure => { if (!controller.signal.aborted) setError(researchError(failure)); });
    return () => controller.abort();
  }, [revision, supported]);
  async function save() {
    if (!studyId || !modelId || !supported) return;
    if (draft.trigger.kind === 'persistent_drift' && (!(draft.trigger.normalized_rmse > 0 && draft.trigger.normalized_rmse <= 1) || !Number.isInteger(draft.trigger.minimum_windows) || draft.trigger.minimum_windows < 2 || draft.trigger.minimum_windows > 20 || !Number.isInteger(draft.trigger.minimum_seconds) || draft.trigger.minimum_seconds < 1)) { setError('持續漂移需設定有效 NRMSE 門檻 (0,1]、2–20 個非重疊視窗與至少 1 秒持續時間。'); return; }
    setBusy('save'); setError(''); setNotice('');
    try {
      const result = await researchRequest<Policy>('/policies', { ...draft, ...(forkId ? { id: forkId } : {}), study_id: studyId, model_id: modelId, ...(templateId && draft.action === 'frozen_model_tuning' ? { publication_template_version_id: templateId } : {}) });
      setPolicies(current => [result, ...current.filter(item => item.id !== result.id)]); setForkId(result.id); setDirty(false); setNotice(`政策版本 ${result.id} 已保存；${result.enabled ? '允許新研究觸發' : '尚未啟用'}。`);
      // Saving a new version also pauses the superseded policy on the server.
      setRevision(value => value + 1);
    } catch (failure) { setError(researchError(failure)); } finally { setBusy(''); }
  }
  async function changeState(policy: Policy) {
    setBusy(policy.id); setError(''); setNotice('');
    try {
      const result = await researchRequest<Policy>(`/policies/${encodeURIComponent(policy.id)}/state`, { enabled: !policy.enabled, expected_revision: policy.revision });
      setPolicies(current => current.map(item => item.id === result.id ? result : item)); setNotice(result.enabled ? '政策已啟用，等待符合條件的新資料。' : '已暫停新研究觸發；不停止既有 run 或現場控制。');
    } catch (failure) { setError(`政策狀態未確認；請重新讀取再操作。${researchError(failure)}`); } finally { setBusy(''); }
  }
  return <div className="space-y-3"><ResearchPanel title="持續研究政策 · 只觸發隔離工作">
    {!supported && <p className="mb-3 text-sm text-amber-200">伺服器未宣告持續研究能力，政策操作不可用。</p>}
    {error && <ResearchNotice error={error} />}{notice && <p role="status" className="mb-3 text-sm text-indigo-200">{notice}</p>}
    <form onSubmit={event => { event.preventDefault(); void save(); }}><fieldset disabled={!!busy || !supported} className="space-y-4">
      <p className="break-all text-xs text-slate-400">凍結研究：{studyId || '請先儲存研究版本'} · {dirty ? '政策草稿尚未儲存' : '未修改政策草稿'}</p>
      <div className="grid gap-3 sm:grid-cols-2">
        <ResearchField label="政策名稱"><input required maxLength={120} className={researchControl} value={draft.name} onChange={event => { setDraft({ ...draft, name: event.target.value }); setDirty(true); }} /></ResearchField>
        <ResearchField label="獲准的隔離研究動作"><select className={researchControl} value={draft.action} onChange={event => { setDraft({ ...draft, action: event.target.value as PolicyAction }); if (event.target.value !== 'frozen_model_tuning') setTemplateId(''); setDirty(true); }}><option value="frozen_model_tuning">固定模型與 plant，只重新調參</option><option value="reidentify_and_tune" disabled={!automaticReidentification}>重新辨識並調參（僅伺服器驗證合成開環資料）</option></select></ResearchField>
        <ResearchField label="固定模型與 evaluation plant"><select required className={researchControl} value={modelId} onChange={event => { setModelId(event.target.value); setDirty(true); }}><option value="">選擇目前研究的凍結模型</option>{models.filter(item => item.study_id === studyId || item.id === assignedModelId).map(item => <option key={item.id} value={item.id}>{item.id}{item.id === assignedModelId ? '（研究已固定）' : ''}</option>)}</select></ResearchField>
        <ResearchField label="合格後自動封存候選的參考版本"><select className={researchControl} value={templateId} disabled={draft.action !== 'frozen_model_tuning'} onChange={event => { setTemplateId(event.target.value); setDirty(true); }}><option value="">不自動封存，由 M8 手動送審</option>{versions.map(version => <option key={version.id} value={version.id}>{version.process_area} · {version.method} · {version.trial_id} · {version.id.slice(0, 12)}</option>)}</select></ResearchField>
        <p className="text-xs text-slate-400 sm:col-span-2">自動封存只沿用同作者、同身分域且匹配凍結模型的已驗證設備綁定；通過資格才產生不可變候選。參考版本不授予現場權限，仍須在 Runtime 另行獨立核准有界發布政策。</p>
        <ResearchField label="研究觸發條件"><select className={researchControl} value={draft.trigger.kind} onChange={event => { setDraft({ ...draft, trigger: event.target.value === 'persistent_drift' ? { kind: 'persistent_drift', normalized_rmse: NaN, minimum_windows: 2, minimum_seconds: NaN } : { kind: 'new_data' } }); setDirty(true); }}><option value="new_data">符合來源與品質條件的新資料</option><option value="persistent_drift">凍結模型的持續驗證殘差漂移</option></select></ResearchField>
        {draft.trigger.kind === 'persistent_drift' && <>
          <ResearchNumber label="標準化 RMSE 漂移門檻 (0,1]" value={draft.trigger.normalized_rmse} min={0.000001} max={1} onChange={value => { if (draft.trigger.kind === 'persistent_drift') { setDraft({ ...draft, trigger: { ...draft.trigger, normalized_rmse: value ?? NaN } }); setDirty(true); } }} />
          <ResearchNumber label="連續非重疊視窗數" value={draft.trigger.minimum_windows} min={2} max={20} step={1} onChange={value => { if (draft.trigger.kind === 'persistent_drift') { setDraft({ ...draft, trigger: { ...draft.trigger, minimum_windows: value ?? NaN } }); setDirty(true); } }} />
          <ResearchNumber label="漂移最少持續時間（秒）" value={draft.trigger.minimum_seconds} min={1} step={1} onChange={value => { if (draft.trigger.kind === 'persistent_drift') { setDraft({ ...draft, trigger: { ...draft.trigger, minimum_seconds: value ?? NaN } }); setDirty(true); } }} />
          <p className="text-xs text-slate-400 sm:col-span-2">只以凍結模型的 validation 殘差判斷，不使用最終 test。資料需有可核實絕對時間與不重疊來源區間；只有相對時間且缺少來源時刻時跳過，不當作持續漂移。</p>
        </>}
        <ResearchNumber label="新資料最少筆數" value={draft.minimum_rows} min={10} max={500000} step={1} onChange={value => { setDraft({ ...draft, minimum_rows: value ?? NaN }); setDirty(true); }} />
        <ResearchNumber label="最大補值比例（0–0.2）" value={draft.max_filled_fraction} min={0} max={0.2} onChange={value => { setDraft({ ...draft, max_filled_fraction: value ?? NaN }); setDirty(true); }} />
        <ResearchNumber label="觸發冷卻時間（秒）" value={draft.cooldown_seconds} min={30} onChange={value => { setDraft({ ...draft, cooldown_seconds: value ?? NaN }); setDirty(true); }} />
        <ResearchNumber label="政策總 run 預算" value={draft.max_runs} min={1} step={1} onChange={value => { setDraft({ ...draft, max_runs: value ?? NaN }); setDirty(true); }} />
        <ResearchNumber label="政策總 trial 預算" value={draft.max_trials_total} min={1} step={1} onChange={value => { setDraft({ ...draft, max_trials_total: value ?? NaN }); setDirty(true); }} />
      </div>
      <label className="flex gap-2 text-sm"><input type="checkbox" checked={draft.enabled} onChange={event => { setDraft({ ...draft, enabled: event.target.checked }); setDirty(true); }} />保存後啟用指定研究觸發（不啟用自動發布）</label>
      <button type="submit" className={researchPrimary} disabled={!studyId || !models.some(item => item.id === modelId && (item.study_id === studyId || item.id === assignedModelId)) || (draft.action === 'reidentify_and_tune' && !automaticReidentification)}>{busy === 'save' ? '保存中…' : '保存政策新版本'}</button>
    </fieldset></form>
    <p className="mt-3 text-xs text-slate-400">{draft.action === 'reidentify_and_tune' ? '重辨識只接受伺服器核實的合成開環來源與因果／激勵資格；一般上傳或觀測歷史不能靠勾選取得資格。這不是對現場注入激勵或自動發布。' : '新資料針對固定模型／plant／目標進行有界調參，不重新辨識。'} 資料不足或品質失效應跳過並保留原因。</p>
  </ResearchPanel><ResearchPanel title="伺服器觸發、跳過原因與預算">
    <button className={researchButton} disabled={!supported || !!busy} onClick={() => setRevision(value => value + 1)}>重新讀取政策與排程狀態</button>
    {scheduler !== null && <details className="mt-3"><summary className="cursor-pointer text-indigo-300">排程器狀態</summary><ResearchDetails value={scheduler} /></details>}
    {!policies.length && <p className="mt-3 text-sm text-slate-400">尚未取得政策；無資料不代表排程器正常。</p>}
    <div className="mt-3 space-y-3">{policies.map(policy => <article key={policy.id} className="min-w-0 space-y-3 rounded border border-slate-700 p-3">
      <h3 className="font-medium">{policy.name} · {policy.enabled ? '啟用' : '暫停'} · revision {policy.revision}</h3>
      <p className="break-all text-xs">{policy.id} · 研究 {policy.study_id} · 模型 {policy.model_id || '未記錄'} · {policy.action === 'reidentify_and_tune' ? '合格合成來源重辨識與調參' : '固定模型調參'}</p>
      <p className="text-sm">run {policy.state?.runs_started ?? '未知'} / {policy.max_runs} · 已預留 trial {policy.state?.trials_reserved ?? '未知'} / {policy.max_trials_total}</p>
      <p className="break-all text-xs text-slate-400">{policy.publication_template_version_id ? `候選參考 ${policy.publication_template_version_id} · ${policy.candidate_mode ?? '待核對封存模式'}` : '候選須經 M8 手動送審'}</p>
      <ResearchDetails value={policy.trigger || { kind: 'new_data' }} />
      <div className="flex flex-wrap gap-2"><button className={researchButton} disabled={!!busy} onClick={() => void changeState(policy)}>{policy.enabled ? '暫停新研究' : '啟用政策'}</button><button className={researchButton} disabled={!!busy || policy.study_id !== studyId} onClick={() => { setForkId(policy.id); setModelId(policy.model_id || ''); setTemplateId(policy.publication_template_version_id ?? ''); setDraft({ name: policy.name, action: policy.action || 'frozen_model_tuning', trigger: policy.trigger ? structuredClone(policy.trigger) : { kind: 'new_data' }, enabled: false, minimum_rows: policy.minimum_rows, max_filled_fraction: policy.max_filled_fraction, cooldown_seconds: policy.cooldown_seconds, max_runs: policy.max_runs, max_trials_total: policy.max_trials_total }); setDirty(true); }}>複製設定為新版本</button></div>
      <ResearchDetails value={policy.state?.last_decision ?? '尚未回報觸發決策'} /><details><summary className="cursor-pointer text-indigo-300">觸發／去重／跳過與 run 紀錄</summary><ResearchDetails value={policy.state?.audit ?? []} /></details>
    </article>)}</div>
  </ResearchPanel></div>;
}
