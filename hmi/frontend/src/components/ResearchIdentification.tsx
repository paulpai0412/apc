import { useEffect, useState } from 'react';
import { researchError, researchRequest } from '../lib/research';
import type { ResearchBootstrap, ResearchRun, ResearchValue, Study } from '../types/Research';
import { ResearchDetails, ResearchField, ResearchNotice, ResearchPanel, researchButton, researchControl, researchPrimary } from './ResearchControls';

type ModelArtifact = { id: string; study_id: string; model_sha256: string; created_at: string; model: ResearchValue; report: ResearchValue; qualification: ResearchValue };
type ModelSummary = Omit<ModelArtifact, 'model' | 'report'>;
export function ResearchIdentification({ study, savedId, run, capabilities, onEngineering, onChange }: { study: Study; savedId: string; run: ResearchRun | null; capabilities: ResearchBootstrap['capabilities']; onEngineering: () => void; onChange: (study: Study) => void }) {
  const [models, setModels] = useState<ModelSummary[]>([]);
  const [selectedId, setSelectedId] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [revision, setRevision] = useState(0);
  const [loadedArtifact, setLoadedArtifact] = useState<ModelArtifact | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [detailError, setDetailError] = useState('');
  const methods = Array.isArray(capabilities.identification) ? capabilities.identification.filter((item): item is Record<string, ResearchValue> => !!item && typeof item === 'object' && !Array.isArray(item)) : [];
  const supported = methods.some(item => item.supported === true && item.scope === 'control_model');
  const family = typeof study.config.model_family === 'string' ? study.config.model_family : 'fir';
  const familySupported = methods.some(item => item.id === family && item.supported === true && item.scope === 'control_model');
  const evaluationFamily = typeof study.config.evaluation_model_family === 'string' ? study.config.evaluation_model_family : 'sopdt';
  const evaluationFamilies = Array.isArray(capabilities.evaluation_model_families) ? capabilities.evaluation_model_families.filter((item): item is string => typeof item === 'string') : [];
  const evaluationSupported = evaluationFamilies.includes(evaluationFamily);
  useEffect(() => {
    if (!supported) return;
    const controller = new AbortController();
    researchRequest<{ models: ModelSummary[] }>('/models', undefined, controller.signal).then(data => {
      if (!controller.signal.aborted) { setModels(data.models); setError(''); }
    }).catch(failure => { if (!controller.signal.aborted) setError(researchError(failure)); });
    return () => controller.abort();
  }, [revision, supported]);
  const selectedModel = models.find(item => item.id === selectedId);
  const expectedDigest = selectedModel?.model_sha256;
  const expectedStudyId = selectedModel?.study_id;
  useEffect(() => {
    if (!selectedId || loadedArtifact?.id === selectedId) return;
    const controller = new AbortController();
    setDetailLoading(true); setDetailError('');
    researchRequest<ModelArtifact>(`/models/${encodeURIComponent(selectedId)}`, undefined, controller.signal).then(result => {
      if (controller.signal.aborted) return;
      if (result.id !== selectedId || (expectedDigest && result.model_sha256 !== expectedDigest) || (expectedStudyId && result.study_id !== expectedStudyId)) throw new Error('模型詳細回覆與所選不可變身分不相符。');
      setLoadedArtifact(result);
    }).catch(failure => { if (!controller.signal.aborted) setDetailError(researchError(failure)); }).finally(() => { if (!controller.signal.aborted) setDetailLoading(false); });
    return () => controller.abort();
  }, [selectedId, expectedDigest, expectedStudyId, loadedArtifact, revision]);
  async function identify() {
    if (!savedId || !familySupported || !evaluationSupported) return;
    setBusy(true); setError('');
    try {
      const result = await researchRequest<ModelArtifact>('/identify', { study_id: savedId });
      if (result.study_id !== savedId || !result.id || !result.model_sha256) throw new Error('辨識回覆缺少相符的研究與不可變模型身分。請更新模型清單確認。');
      const summary: ModelSummary = { id: result.id, study_id: result.study_id, model_sha256: result.model_sha256, created_at: result.created_at, qualification: result.qualification };
      setModels(current => [summary, ...current.filter(item => item.id !== result.id)]);
      setLoadedArtifact(result); setSelectedId(result.id); setDetailError('');
    } catch (failure) { setError(researchError(failure)); } finally { setBusy(false); }
  }
  const artifact = loadedArtifact?.id === selectedId ? loadedArtifact : null;
  return <div className="space-y-3">
    <ResearchPanel title="模型族與真實辨識能力">
      {error && <ResearchNotice error={error} />}
      {!methods.length && <p className="text-sm text-amber-200">伺服器尚未宣告模型辨識能力，不能選擇或啟動未確認的方法。</p>}
      <ul className="space-y-2">{methods.map(item => <li key={String(item.id)} className="rounded border border-slate-700 p-3 text-sm"><strong>{String(item.id).toUpperCase()}</strong> · {item.supported === true ? item.scope === 'control_model' ? '可辨識控制模型' : '僅 evaluation plant' : '未支援'}{typeof item.reason === 'string' && <p className="mt-1 text-slate-400">{item.reason}</p>}</li>)}</ul>
      <div className="my-3 grid gap-3 sm:grid-cols-2">
        <ResearchField label="控制模型族（使用 training 辨識）"><select className={researchControl} disabled={busy} value={family} onChange={event => onChange({ ...study, model_id: undefined, config: { ...study.config, model_family: event.target.value } })}>{methods.filter(item => item.scope === 'control_model').map(item => <option key={String(item.id)} value={String(item.id)} disabled={item.supported !== true}>{String(item.id).toUpperCase()}{item.supported !== true ? ' · 未支援' : ''}</option>)}</select></ResearchField>
        <ResearchField label="evaluation plant 模型族（獨立版本身分）"><select className={researchControl} value={evaluationFamily} disabled={busy || !evaluationFamilies.length} onChange={event => onChange({ ...study, model_id: undefined, config: { ...study.config, evaluation_model_family: event.target.value } })}>{!evaluationFamilies.includes(evaluationFamily) && <option value={evaluationFamily}>{evaluationFamily.toUpperCase()} · 尚未宣告能力</option>}{evaluationFamilies.map(item => <option key={item} value={item}>{item.toUpperCase()}</option>)}</select></ResearchField>
      </div>
      {!familySupported && <p className="text-xs text-amber-200">所選模型族未獲伺服器支援，不能默默改用其他方法。</p>}
      {!evaluationSupported && <p className="text-xs text-amber-200">evaluation plant 模型族未獲伺服器宣告支援，請選擇可用方法。</p>}
      <dl className="my-4 grid gap-2 text-xs sm:grid-cols-2"><div><dt>資料／來源</dt><dd className="break-all">{study.dataset_id || '獨立合成資料（非工廠）'}</dd></div><div><dt>Ts</dt><dd>{study.config.sample_time} 秒</dd></div><div><dt>模型版本依據</dt><dd className="break-all">{savedId || '草稿未凍結'}</dd></div><div><dt>輸入座標</dt><dd>{String(study.config.mv_coordinate || '未確認')}</dd></div></dl>
      <div className="flex flex-wrap gap-2"><button className={researchPrimary} disabled={busy || !savedId || !familySupported || !evaluationSupported} onClick={() => void identify()}>{busy ? '辨識與資格檢查中…' : `辨識並凍結 ${family.toUpperCase()} 控制模型`}</button><button className={researchButton} onClick={onEngineering}>儲存模型設定／修改上游工程</button><button className={researchButton} disabled={!supported || busy} onClick={() => setRevision(value => value + 1)}>更新模型清單</button></div>
      {!savedId && <p className="mt-2 text-xs text-amber-200">請先保存工程與研究版本。此操作不使用最終 test 選擇模型，也不啟動現場試探。</p>}
    </ResearchPanel>
    <ResearchPanel title="不可變模型與資格證據">
      <ResearchField label="開啟凍結模型"><select className={researchControl} value={selectedId} onChange={event => { setSelectedId(event.target.value); setDetailError(''); setDetailLoading(false); }}><option value="">選取模型產物</option>{models.map(item => <option key={item.id} value={item.id}>{item.id} · {item.created_at} · {item.study_id === savedId ? '目前研究' : '其他研究'}</option>)}</select></ResearchField>
      {detailLoading && !artifact && <p role="status" className="mt-3 text-sm text-slate-400">讀取所選模型的完整係數與資格證據…</p>}
      {detailError && !artifact && <ResearchNotice error={detailError} />}
      {artifact ? <div className="mt-4 space-y-3"><p className="break-all text-xs">SHA-256：{artifact.model_sha256} · 研究：{artifact.study_id}</p><ResearchDetails value={artifact.qualification} /><details open><summary className="cursor-pointer text-indigo-300">激勵、共線、預測誤差與資格報告</summary><ResearchDetails value={artifact.report} /></details><details><summary className="cursor-pointer text-indigo-300">完整 MV→CV／DV→CV 動態與模型參數</summary><ResearchDetails value={artifact.model} /></details></div> : <p className="mt-3 text-sm text-slate-400">尚未開啟模型。缺少激勵或模型資格不是零增益；依報告補足資料，不自動向現場注入激勵。</p>}
      {artifact && <button type="button" className={`${researchPrimary} mt-3`} disabled={artifact.study_id !== savedId || study.model_id === artifact.id} onClick={() => onChange({ ...study, model_id: artifact.id })}>將此模型與 plant 固定到研究草稿（仍需儲存新版本）</button>}
      {run?.results?.identification && <details className="mt-4"><summary className="cursor-pointer text-indigo-300">所選 run 的歷史辨識證據（不作重新選模）</summary><ResearchDetails value={run.results.identification} /></details>}
    </ResearchPanel>
  </div>;
}
