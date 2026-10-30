import { useEffect, useRef, useState } from 'react';
import { researchError, researchRequest } from '../lib/research';
import { apiFetch } from '../lib/api';
import type { ControlVersion } from '../types/ControlVersion';
import type { ResearchRun, ResearchValue } from '../types/Research';
import { ResearchDetails, ResearchField, ResearchNotice, ResearchPanel, researchButton, researchControl, researchPrimary } from './ResearchControls';

type ReviewTarget = { profile_hash: string; target: { plant: string; line: string; area: string; group: string }; equipment_instance_ids: string[]; operating_envelope: ResearchValue };
type ReviewVersion = ControlVersion & Partial<Omit<ReviewTarget, 'profile_hash'>> & { deployment_profile_hash?: string };

function hasVersionIdentity(version: ControlVersion): boolean {
  return !!version && version.schema_version === 1 && version.authority === 'review_only'
    && typeof version.id === 'string' && /^[a-f0-9]{64}$/.test(version.id)
    && typeof version.model_sha256 === 'string' && /^[a-f0-9]{64}$/.test(version.model_sha256)
    && typeof version.method === 'string' && typeof version.trial_id === 'string'
    && typeof version.run_id === 'string';
}

function VersionIdentity({ version }: { version: ReviewVersion }) {
  return <dl className="grid min-w-0 gap-2 text-xs sm:grid-cols-2">
    <div className="min-w-0 sm:col-span-2"><dt className="text-slate-400">不可變版本 ID · 內容雜湊</dt><dd className="break-all font-mono text-indigo-200">{version.id}</dd></div>
    <div className="min-w-0 sm:col-span-2"><dt className="text-slate-400">原始模型 SHA-256</dt><dd className="break-all font-mono">{version.model_sha256}</dd></div>
    <div><dt className="text-slate-400">方法 / 試驗</dt><dd className="break-all">{version.method} / {version.trial_id}</dd></div>
    <div><dt className="text-slate-400">權限</dt><dd className="text-amber-200">review_only · 僅供審查</dd></div>
    <div><dt className="text-slate-400">Run</dt><dd className="break-all">{version.run_id}</dd></div>
    <div><dt className="text-slate-400">選版人 / 時間</dt><dd className="break-words">{version.selected_by || '未記錄'} / {version.created_at || '未記錄'}</dd></div>
    <div className="min-w-0 sm:col-span-2"><dt className="text-slate-400">Runtime profile／精確設備</dt><dd className="break-all">{version.deployment_profile_hash || '未綁定目標（不可直接發布）'} · {version.equipment_instance_ids?.join('、') || '未記錄設備集合'}</dd></div>
  </dl>;
}

export function ResearchVersionSelection({ run }: { run: ResearchRun }) {
  const [method, setMethod] = useState('');
  const [acknowledged, setAcknowledged] = useState(false);
  const [versions, setVersions] = useState<ReviewVersion[]>([]);
  const [receipt, setReceipt] = useState<ReviewVersion | null>(null);
  const [listError, setListError] = useState('');
  const [selectionError, setSelectionError] = useState('');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [revision, setRevision] = useState(0);
  const [profiles, setProfiles] = useState<ReviewTarget[]>([]);
  const [profileHash, setProfileHash] = useState('');
  const [profileError, setProfileError] = useState('');
  const [profileLoading, setProfileLoading] = useState(true);
  const selectionRequest = useRef<AbortController | null>(null);
  useEffect(() => () => selectionRequest.current?.abort(), []);
  useEffect(() => {
    const controller = new AbortController();
    setProfileLoading(true); setProfileError('');
    apiFetch('/api/runtime/profiles', { signal: controller.signal }).then(async response => {
      if (!response.ok) throw new Error(`讀取核准目標失敗（HTTP ${response.status}）；需要明確 runtime.view 範圍授權與本機 profile 目錄。`);
      const data = await response.json() as { profiles: ReviewTarget[] };
      if (!Array.isArray(data.profiles) || data.profiles.some(item => !/^[a-f0-9]{64}$/.test(item.profile_hash) || !item.target || !Array.isArray(item.equipment_instance_ids))) throw new Error('核准 profile 目錄格式不相容。');
      if (!controller.signal.aborted) setProfiles(data.profiles);
    }).catch(failure => { if (!controller.signal.aborted) { setProfiles([]); setProfileError(researchError(failure)); } }).finally(() => { if (!controller.signal.aborted) setProfileLoading(false); });
    return () => controller.abort();
  }, [revision]);
  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setListError('');
    researchRequest<{ versions: ControlVersion[] }>('/versions', undefined, controller.signal).then(data => {
      if (controller.signal.aborted) return;
      if (!data || !Array.isArray(data.versions) || !data.versions.every(hasVersionIdentity)) throw new Error('版本清單格式不正確，保留先前清單。');
      setVersions(data.versions);
    }).catch(error => { if (!controller.signal.aborted) setListError(researchError(error)); })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [revision]);
  const selected = run.results?.methods.find(item => item.method === method);
  const profile = profiles.find(item => item.profile_hash === profileHash);
  const eligible = run.status === 'completed' && selected?.recommendation === 'candidate_for_review';
  async function saveVersion() {
    if (!eligible || !acknowledged || !profile || profileLoading || profileError || selectionRequest.current) return;
    const controller = new AbortController();
    selectionRequest.current = controller;
    setSaving(true);
    setSelectionError('');
    try {
      const version = await researchRequest<ReviewVersion>('/versions', { run_id: run.id, method, deployment_profile_hash: profile.profile_hash, target: profile.target, equipment_instance_ids: profile.equipment_instance_ids, operating_envelope: profile.operating_envelope }, controller.signal);
      if (controller.signal.aborted) return;
      if (!hasVersionIdentity(version) || version.run_id !== run.id || version.method !== method || version.trial_id !== selected?.selected_trial) throw new Error('選版回應與所選研究／試驗不符；請重新整理版本清單確認。');
      if (version.deployment_profile_hash !== profile.profile_hash || !version.target || (['plant', 'line', 'area', 'group'] as const).some(key => version.target?.[key] !== profile.target[key]) || !version.equipment_instance_ids || [...version.equipment_instance_ids].sort().join('\n') !== [...profile.equipment_instance_ids].sort().join('\n')) throw new Error('候選回覆的精確目標不相符；請更新版本清單確認，不可將回覆視為送審成功。');
      setReceipt(version);
      setVersions(current => [version, ...current.filter(item => item.id !== version.id)]);
      setRevision(current => current + 1);
    } catch (error) {
      if (!controller.signal.aborted) setSelectionError(researchError(error));
    } finally {
      selectionRequest.current = null;
      if (!controller.signal.aborted) setSaving(false);
    }
  }
  return <ResearchPanel title="下一步 · 保存工程審查版本">
    <p className="mb-4 text-sm text-slate-300">封存所選模型與證據，不部署、不寫入工廠控制。</p>
    <form className="space-y-3" onSubmit={event => { event.preventDefault(); void saveVersion(); }}>
      <ResearchField label="選擇要送審的方法"><select className={researchControl} value={method} disabled={saving} onChange={event => { setMethod(event.target.value); setAcknowledged(false); }}>
        <option value="">請選擇方法</option>
        {run.results?.methods.map(item => <option key={item.method} value={item.method} disabled={run.status !== 'completed' || item.recommendation !== 'candidate_for_review'}>{item.method} · {item.recommendation === 'candidate_for_review' ? '可送審' : '不可選版'} · {item.selected_trial}</option>)}
      </select></ResearchField>
      <ResearchField label="精確 Runtime 核准目標（唯讀目錄）"><select className={researchControl} value={profileHash} disabled={saving || profileLoading} onChange={event => { setProfileHash(event.target.value); setAcknowledged(false); }}><option value="">選擇已配置 profile；不猜設備或 Tag</option>{profiles.map(item => <option key={item.profile_hash} value={item.profile_hash}>{Object.values(item.target).join(' / ')} · {item.profile_hash}</option>)}</select></ResearchField>
      {profileLoading && <p role="status" className="text-xs text-slate-400">讀取核准目標目錄…</p>}
      {profileError && <ResearchNotice error={profileError} />}
      {!profileLoading && !profileError && !profiles.length && <p className="text-xs text-amber-200">沒有可用的核准目標。請現場工程配置 profile 與明確檢視權限；研究結果仍可檢視，但不可猜測目標送審。</p>}
      {profile && <details><summary className="cursor-pointer text-xs text-indigo-300">送審綁定的設備與操作包絡（不可編輯）</summary><ResearchDetails value={profile} /></details>}
      {run.status !== 'completed' ? <p className="text-xs text-amber-200">研究完成且建議為 candidate_for_review 才可選版。</p> : !run.results?.methods.some(item => item.recommendation === 'candidate_for_review') ? <p className="text-xs text-amber-200">目前沒有通過採用評估的方法，不能只依低分數選版。</p> : null}
      <label className="flex items-start gap-2 text-sm text-slate-300"><input className="mt-1" type="checkbox" checked={acknowledged} disabled={!eligible || saving} onChange={event => setAcknowledged(event.target.checked)} /><span>我了解此版本僅供工程審查，沒有工廠控制權限。</span></label>
      <button type="submit" className={researchPrimary} disabled={!eligible || !acknowledged || !profile || profileLoading || !!profileError || saving}>{saving ? '正在保存版本…' : '保存精確目標的候選送審版本'}</button>
      {selectionError && <div className="space-y-2"><ResearchNotice error={selectionError} /><p className="text-xs text-slate-400">已保留方法與確認選項；若連線中斷，可先重新整理下方清單確認是否已保存。</p></div>}
    </form>
    {receipt && <div className="mt-4 space-y-3 rounded-lg border border-emerald-800 bg-emerald-950/20 p-3"><div role="status" aria-live="polite"><h3 className="text-sm font-medium text-emerald-300">已保存審查版本 · 尚未授權控制</h3></div><VersionIdentity version={receipt} /><p className="text-xs text-slate-300">安裝、核准與現場接管只在 Runtime 版本頁進行，建模不提供安裝操作。</p></div>}
    <div className="mt-5 border-t border-slate-700 pt-4">
      <div className="mb-3 flex flex-wrap items-center justify-between gap-3"><h3 className="text-sm font-medium">已保存版本 · 所有研究</h3><button type="button" className={researchButton} disabled={loading || saving} onClick={() => setRevision(current => current + 1)}>重新整理版本清單</button></div>
      {loading && <p role="status" className="mb-3 text-sm text-slate-400">正在讀取版本清單…</p>}
      {listError && <div className="mb-3 space-y-2"><ResearchNotice error={listError} />{versions.length > 0 && <p className="text-xs text-amber-200">以下保留上次取得的版本；清單尚未更新。</p>}</div>}
      {!loading && !listError && versions.length === 0 && <p className="text-sm text-slate-400">還沒有保存的審查版本。</p>}
      <div className="space-y-3">{versions.map(version => <details key={version.id} className="min-w-0 rounded-lg border border-slate-700 p-3">
        <summary className="cursor-pointer break-words text-sm text-indigo-200">{version.method} · {version.trial_id} · {version.run_id === run.id ? '此研究' : version.process_area || '其他研究'} · {version.created_at || '時間未記錄'}</summary>
        <div className="mt-3 space-y-3"><VersionIdentity version={version} /><p className="break-words text-xs text-amber-200">{version.evidence_scope || '證據範圍未記錄'}</p><details><summary className="cursor-pointer text-xs text-indigo-300">凍結的參數、指標與驗收證據</summary><div className="mt-3"><ResearchDetails value={{ parameters: version.parameters, variables: version.variables, metrics: version.metrics, assessment: version.assessment, model_test: version.model_test, source_hashes: version.source_hashes }} /></div></details></div>
        <button type="button" className={`${researchButton} mt-3`} disabled={saving} onClick={() => setReceipt(version)}>檢視不可變版本身分</button>
      </details>)}</div>
    </div>
  </ResearchPanel>;
}
