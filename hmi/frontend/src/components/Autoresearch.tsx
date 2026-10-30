import { useEffect, useState } from 'react';
import type { FormEvent } from 'react';
import { researchError, researchRequest } from '../lib/research';
import type { Dataset, ResearchBootstrap, ResearchDraft, ResearchRun, ResearchWorkspaceContext, Study } from '../types/Research';
import type { ModelingPage } from '../types/Workspace';
import { ResearchCleaning } from './ResearchCleaning';
import { DigitalTwin } from './DigitalTwin';
import { DigitalTwinConfigurationEditor } from './DigitalTwinConfigurationEditor';
import { ResearchImport } from './ResearchImport';
import { ResearchResults } from './ResearchResults';
import { ResearchStudy } from './ResearchStudy';
import { validateStudy } from '../lib/studyValidation';
import { ResearchIdentification } from './ResearchIdentification';
import { ResearchPolicy } from './ResearchPolicy';
import { ResearchField, ResearchNotice, ResearchPanel, researchButton, researchControl, researchPrimary } from './ResearchControls';

type Props = { section: ModelingPage; onSectionChange: (page: ModelingPage) => void; sceneVisible?: boolean; active?: boolean; onContextChange?: (context: ResearchWorkspaceContext) => void };
const pending = (run: ResearchRun) => ['queued', 'running', 'cancel_requested'].includes(run.status);

export function Autoresearch({ section, onSectionChange, sceneVisible = true, active = true, onContextChange }: Props) {
  const [bootstrap, setBootstrap] = useState<ResearchBootstrap | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadRevision, setLoadRevision] = useState(0);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [busy, setBusy] = useState('');
  const [datasetId, setDatasetId] = useState('');
  const [study, setStudy] = useState<Study | null>(null);
  const [savedId, setSavedId] = useState('');
  const [selectedStudy, setSelectedStudy] = useState('');
  const [runId, setRunId] = useState('');
  const [run, setRun] = useState<ResearchRun | null>(null);
  const [pollError, setPollError] = useState('');
  const [runRevision, setRunRevision] = useState(0);
  const [visited, setVisited] = useState<Set<ModelingPage>>(() => new Set([section]));
  const [engineeringTrial, setEngineeringTrial] = useState('');
  const [drafts, setDrafts] = useState<Record<string, Study>>({});
  const [serverDrafts, setServerDrafts] = useState<ResearchDraft[]>([]);
  const [draftIdentity, setDraftIdentity] = useState<{ id: string; revision: number } | null>(null);
  const [draftSnapshot, setDraftSnapshot] = useState('');
  const [draftError, setDraftError] = useState('');

  useEffect(() => { setVisited(current => current.has(section) ? current : new Set([...current, section])); }, [section]);
  useEffect(() => {
    if (!active) return;
    const controller = new AbortController();
    setLoading(true); setError('');
    researchRequest<ResearchBootstrap>('', undefined, controller.signal).then(data => {
      if (controller.signal.aborted) return;
      if (!data.enabled || data.schema_version !== 1 || !data.defaults || !data.capabilities?.methods) throw new Error('研究功能未啟用或伺服器格式不相容。');
      setBootstrap(data);
      setStudy(current => current || { id: '', name: '水泥磨控制研究', dataset_id: null, config: structuredClone(data.defaults.campaign), research: { ...structuredClone(data.defaults.research), selection_mode: 'manual' } });
      setRunId(current => current || data.runs.find(pending)?.id || '');
    }).catch(failure => { if (!controller.signal.aborted) setError(researchError(failure)); }).finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [loadRevision, active]);
  useEffect(() => {
    if (!active) return;
    const controller = new AbortController();
    researchRequest<{ drafts: ResearchDraft[] }>('/drafts', undefined, controller.signal).then(data => {
      if (!controller.signal.aborted) { setServerDrafts(data.drafts); setDraftError(''); }
    }).catch(failure => { if (!controller.signal.aborted) setDraftError(researchError(failure)); });
    return () => controller.abort();
  }, [loadRevision, active]);

  useEffect(() => {
    if (!active || !runId) return;
    const controller = new AbortController();
    let timer: number | undefined;
    async function poll() {
      let repeat = true;
      try {
        const current = await researchRequest<ResearchRun>(`/runs/${encodeURIComponent(runId)}`, undefined, controller.signal);
        if (controller.signal.aborted) return;
        if (current.id !== runId) throw new Error('執行回覆身分不相符。');
        setRun(current); setPollError('');
        setBootstrap(previous => previous ? { ...previous, runs: [current, ...previous.runs.filter(item => item.id !== current.id)] } : previous);
        repeat = pending(current);
      } catch (failure) { if (!controller.signal.aborted) setPollError(`狀態未確認，最後值可能過期。${researchError(failure)}`); }
      finally { if (!controller.signal.aborted && repeat) timer = window.setTimeout(() => void poll(), 3000); }
    }
    void poll();
    return () => { controller.abort(); window.clearTimeout(timer); };
  }, [runId, runRevision, active]);

  const dataset = bootstrap?.datasets.find(item => item.id === datasetId);
  const activeRun = bootstrap?.runs.find(pending);
  const currentRun = run?.id === runId ? run : null;
  const dirty = !!study && !savedId && JSON.stringify(study) !== draftSnapshot;
  const inspectingRun = section === 'simulation' || section === 'candidates';
  const inspectingDataset = section === 'sources' || section === 'cleaning';
  const contextStudy = inspectingRun ? bootstrap?.studies.find(item => item.id === currentRun?.study_id) : study;
  const contextDatasetId = inspectingDataset ? datasetId : contextStudy?.dataset_id || '';
  const contextStudyId = inspectingRun ? currentRun?.study_id || '' : savedId;
  const sourceLabel = inspectingDataset ? dataset ? `${dataset.kind === 'raw' ? '原始' : '清理'}資料 ${dataset.id}（非 Live）` : '尚未選取資料來源' : contextStudy?.dataset_id ? `歷史資料 ${contextStudy.dataset_id}（非 Live）` : contextStudy ? '獨立合成製程（非 Live）' : '所選 run 來源尚未確認';
  const versionLabel = inspectingRun ? currentRun ? `Run ${currentRun.id} · 研究 ${currentRun.study_id}` : '執行版本尚未確認' : savedId || (draftIdentity ? `草稿 ${draftIdentity.id} · revision ${draftIdentity.revision}${dirty ? ' · 未儲存變更' : ''}` : study ? '未儲存研究草稿' : '尚無研究版本');
  useEffect(() => { onContextChange?.({ datasetId: contextDatasetId, studyId: contextStudyId, runId, sourceLabel, versionLabel, dirty }); }, [contextDatasetId, contextStudyId, runId, sourceLabel, versionLabel, dirty, onContextChange]);
  useEffect(() => {
    if (!dirty) return;
    const warn = (event: BeforeUnloadEvent) => { event.preventDefault(); event.returnValue = ''; };
    window.addEventListener('beforeunload', warn);
    return () => window.removeEventListener('beforeunload', warn);
  }, [dirty]);

  function changeStudy(next: Study) { setStudy(next); setSavedId(''); setNotice(''); }
  function stashDraft() { if (study && !savedId) setDrafts(current => ({ ...current, [selectedStudy || 'new']: structuredClone(study) })); }
  function openStudy(id: string) {
    if (!bootstrap) return;
    stashDraft();
    setDraftIdentity(null); setDraftSnapshot('');
    const stored = drafts[id || 'new'];
    const found = bootstrap.studies.find(item => item.id === id);
    const next = stored || found || { id: '', name: '水泥磨控制研究', dataset_id: null, config: structuredClone(bootstrap.defaults.campaign), research: { ...structuredClone(bootstrap.defaults.research), selection_mode: 'manual' } };
    setStudy(structuredClone(next)); setSelectedStudy(id); setSavedId(stored ? '' : found?.id || '');
    if (next.dataset_id) setDatasetId(next.dataset_id);
  }
  async function selectStudyDataset(id: string) {
    if (!bootstrap || busy) return;
    stashDraft();
    if (!id) { openStudy(''); return; }
    setBusy('draft'); setError(''); setNotice('');
    try {
      const draft = await researchRequest<Study>('/draft-study', { dataset_id: id });
      changeStudy({ ...draft, research: { ...draft.research, selection_mode: 'manual' } }); setSelectedStudy(''); setDraftIdentity(null); setDraftSnapshot(''); onSectionChange('engineering');
      setNotice('已建立工程草稿；觀測範圍不是已核准的設備安全限。');
    } catch (failure) { setError(`清理資料保留，但研究草稿未建立：${researchError(failure)}`); }
    finally { setBusy(''); }
  }
  async function saveDraft() {
    if (!study || busy) return;
    setBusy('draft-save'); setError('');
    try {
      const { name, dataset_id, model_id, config, research } = study;
      const saved = await researchRequest<ResearchDraft>('/drafts', { ...(draftIdentity ? { id: draftIdentity.id, expected_revision: draftIdentity.revision } : {}), name, dataset_id, model_id, config, research });
      const confirmed: Study = { id: '', name: saved.name, dataset_id: saved.dataset_id ?? null, model_id: saved.model_id, config: saved.config, research: saved.research };
      setStudy(confirmed); setSavedId(''); setSelectedStudy('');
      setDraftIdentity({ id: saved.id, revision: saved.revision }); setDraftSnapshot(JSON.stringify(confirmed));
      setServerDrafts(current => [saved, ...current.filter(item => item.id !== saved.id)]);
      setNotice(`工程草稿已保存，revision ${saved.revision}；未具備執行／投運資格。`);
    } catch (failure) { setError(`草稿未保存；多人 revision 衝突不得覆寫，請更新清單並複製為新草稿。${researchError(failure)}`); }
    finally { setBusy(''); }
  }
  async function saveStudy(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!study || !bootstrap) return;
    const issue = validateStudy(study, bootstrap.datasets);
    if (issue) { setError(issue); return; }
    setBusy('save'); setError(''); setNotice('');
    try {
      const { id, name, dataset_id, model_id, config, research } = study;
      const saved = await researchRequest<Study>('/studies', { id, name, dataset_id, model_id, config, research });
      setStudy(saved); setSavedId(saved.id); setSelectedStudy(saved.id);
      setDraftIdentity(null); setDraftSnapshot('');
      setDrafts(current => { const next = { ...current }; delete next[selectedStudy || 'new']; return next; });
      setBootstrap(current => current ? { ...current, studies: [saved, ...current.studies.filter(item => item.id !== saved.id)] } : current);
      setNotice(`已儲存不可變研究版本 ${saved.id}；尚未啟動工作。`);
    } catch (failure) { setError(`研究未儲存：${researchError(failure)}`); } finally { setBusy(''); }
  }
  async function startRun() {
    if (!savedId) return;
    setBusy('start'); setError('');
    try {
      const created = await researchRequest<ResearchRun>('/runs', { study_id: savedId });
      setBootstrap(current => current ? { ...current, runs: [created, ...current.runs.filter(item => item.id !== created.id)] } : current);
      setRunId(created.id); setRun(created); onSectionChange('overview');
      setNotice('伺服器已建立工作；關頁不會重跑或取消。');
    } catch (failure) { setError(`啟動未確認，請先更新任務清單再決定是否重送：${researchError(failure)}`); }
    finally { setBusy(''); }
  }
  async function cancelRun() {
    if (!runId) return;
    setBusy('cancel'); setError(''); setNotice('');
    try { const cancelled = await researchRequest<ResearchRun>(`/runs/${encodeURIComponent(runId)}/cancel`, {}); setRun(cancelled); setRunRevision(value => value + 1); }
    catch (failure) { setError(`取消未確認：${researchError(failure)}`); } finally { setBusy(''); }
  }
  async function importGeneratedHistory() {
    if (!currentRun || currentRun.status !== 'completed') return;
    setBusy('history-import'); setError('');
    try {
      const imported = await researchRequest<Dataset>(`/runs/${encodeURIComponent(currentRun.id)}/import-history`, {});
      if (imported.kind !== 'raw' || imported.source?.kind !== 'generated_synthetic' || imported.source.run_id !== currentRun.id) throw new Error('合成歷史回覆缺少相符的 run 與來源證據。請更新資料清單確認。');
      setBootstrap(current => current ? { ...current, datasets: [imported, ...current.datasets.filter(item => item.id !== imported.id)] } : current);
      setDatasetId(imported.id); onSectionChange('cleaning');
      setNotice('已匯入伺服器驗證的合成開環歷史；請依凍結規則清洗並檢查辨識資格。這不是現場資料。');
    } catch (failure) { setError(`合成來源匯入未確認：${researchError(failure)}`); }
    finally { setBusy(''); }
  }
  const renderPage = (page: ModelingPage) => visited.has(page) || section === page;
  return <div className="min-w-0 space-y-3 p-3 sm:p-5">
    <div className="flex flex-wrap items-center gap-3 text-xs"><span className={dirty ? 'text-amber-200' : 'text-slate-400'}>{dirty ? '有未儲存草稿 · 換頁保留' : savedId ? '研究版本已凍結' : '伺服器草稿已保存，尚未凍結資格'}</span><button type="button" className={researchButton} disabled={loading || !!busy} onClick={() => { setLoadRevision(value => value + 1); setRunRevision(value => value + 1); }}>更新資料與任務</button>{study && <button type="button" className={researchButton} disabled={!!busy} onClick={() => void saveDraft()}>{busy === 'draft-save' ? '保存草稿中…' : '保存工程草稿（不要求工程資格）'}</button>}</div>
    {error && <ResearchNotice error={error} />}{notice && <p role="status" className="text-sm text-indigo-200">{notice}</p>}
    {loading && <p role="status" className="text-sm text-slate-400">讀取伺服器能力與產物…</p>}
    {!bootstrap && !loading && <ResearchPanel title="研究服務未就緒"><p>請確認研究能力、登入授權與伺服器設定。上方保留實際錯誤；不以示範資料替代。</p></ResearchPanel>}
    {bootstrap && study && <fieldset disabled={!!busy} className="min-w-0 space-y-3">
      <div className="grid min-w-0 gap-3 md:grid-cols-2">
        <ResearchField label="研究版本／保留的草稿"><select className={researchControl} disabled={!!busy} value={selectedStudy} onChange={event => openStudy(event.target.value)}><option value="">新研究草稿{drafts.new ? '（未儲存）' : ''}</option>{bootstrap.studies.map(item => <option key={item.id} value={item.id}>{item.name} · {item.id}{drafts[item.id] ? '（有未儲存草稿）' : ''}</option>)}</select></ResearchField>
        <ResearchField label="執行紀錄（不改研究草稿）"><select className={researchControl} value={runId} onChange={event => { setRunId(event.target.value); setPollError(''); }}><option value="">尚未選取 run</option>{bootstrap.runs.map(item => <option key={item.id} value={item.id}>{item.name || item.id} · {item.status}</option>)}</select></ResearchField>
      </div>
      {draftError && <ResearchNotice error={`伺服器草稿清單未更新：${draftError}`} />}
      <ResearchField label="重新開啟伺服器工程草稿（revision 防止多人覆寫）"><select className={researchControl} disabled={!!busy} value={draftIdentity?.id || ''} onChange={event => {
        const item = serverDrafts.find(value => value.id === event.target.value);
        if (!item) { setDraftIdentity(null); setDraftSnapshot(''); return; }
        stashDraft();
        const next: Study = { id: '', name: item.name, dataset_id: item.dataset_id, model_id: item.model_id, config: structuredClone(item.config), research: structuredClone(item.research) };
        setStudy(next); setSavedId(''); setSelectedStudy(''); setDraftIdentity({ id: item.id, revision: item.revision }); setDraftSnapshot(JSON.stringify(next));
        onSectionChange('engineering');
      }}><option value="">新的伺服器草稿（不覆寫既有草稿）</option>{serverDrafts.map(item => <option key={item.id} value={item.id}>{item.name} · r{item.revision} · {item.updated_at}</option>)}</select></ResearchField>
      {currentRun && currentRun.study_id !== savedId && <p className="text-xs text-amber-200">目前 run 屬於研究 {currentRun.study_id}，不是目前草稿的驗證結果。</p>}
      {pollError && <ResearchNotice error={pollError} />}
      {currentRun?.status === 'cancel_requested' && <p role="status" className="text-sm text-indigo-300">工作 {currentRun.id} 的取消請求已保存，尚未確認停止。</p>}
      {currentRun?.status === 'cancelled' && <p role="status" className="text-sm text-indigo-300">工作 {currentRun.id} 已由伺服器確認取消；保留已完成的試驗，不代表完整驗證通過。</p>}
      <section hidden={section !== 'overview'} aria-label="研究任務">
        <ResearchPanel title="任務與下一步"><div className="space-y-3">
          <div className="flex flex-wrap gap-2"><button className={researchButton} onClick={() => { stashDraft(); changeStudy({ ...structuredClone(study), id: '', name: `${study.name}（複本）` }); setSelectedStudy(''); setDraftIdentity(null); setDraftSnapshot(''); onSectionChange('engineering'); }}>複製為新草稿</button><button className={researchPrimary} onClick={() => onSectionChange(!bootstrap.datasets.length ? 'sources' : !study.config.engineering_confirmed ? 'engineering' : 'tuning')}>繼續下一個準備工作</button></div>
          {!bootstrap.runs.length && <p className="text-sm text-slate-400">尚無工作。匯入並清洗資料，或明確使用合成來源，完成工程與調參設定後儲存並啟動。</p>}
          <ul className="space-y-2">{bootstrap.runs.map(item => <li key={item.id} className="rounded border border-slate-700 p-3 text-sm"><div className="flex flex-wrap justify-between gap-2"><span className="break-all">{item.name || item.id} · {item.status} · {item.stage || '階段未記錄'}</span><button className={researchButton} onClick={() => { setRunId(item.id); onSectionChange(item.status === 'completed' ? 'candidates' : 'tuning'); }}>開啟工作</button></div><p>已確認工作 {item.progress?.completed ?? '未知'} / {item.progress?.total ?? '未知'} · {item.created_at}</p>{item.error && <p className="text-rose-200">{item.error}</p>}</li>)}</ul>
          {currentRun && pending(currentRun) && <button className={researchButton} disabled={!!busy || currentRun.status === 'cancel_requested'} onClick={() => void cancelRun()}>{currentRun.status === 'cancel_requested' ? '等待取消確認' : '取消所選工作'}</button>}
          {currentRun?.status === 'completed' && <div className="space-y-2"><button type="button" className={researchButton} disabled={!!busy || !bootstrap.studies.some(item => item.id === currentRun.study_id && item.config.plant_source === 'independent_synthetic')} onClick={() => void importGeneratedHistory()}>將所選 run 的已核實合成開環歷史匯入為原始資料</button><p className="text-xs text-slate-400">只接受完整獨立合成 run 的伺服器 replay／receipt 雜湊。一般上傳歷史無法取得此來源身分。</p></div>}
        </div></ResearchPanel>
      </section>
      <section hidden={section !== 'sources'}>{renderPage('sources') && <ResearchImport datasets={bootstrap.datasets} datasetId={datasetId} busy={!!busy} onSelect={setDatasetId} onStart={() => { setBusy('import'); setError(''); }} onFinish={() => setBusy('')} onImported={imported => { setBootstrap(current => current ? { ...current, datasets: [imported, ...current.datasets.filter(item => item.id !== imported.id)] } : current); setDatasetId(imported.id); onSectionChange('cleaning'); setNotice(`原始資料 ${imported.id} 已保存。`); }} onClean={() => onSectionChange('cleaning')} onStudy={() => onSectionChange('engineering')} />}</section>
      <section hidden={section !== 'cleaning'}>{renderPage('cleaning') && (dataset ? <fieldset disabled={!!busy}><ResearchCleaning key={dataset.id} dataset={dataset} campaign={study.config} onClean={cleaned => { setBootstrap(current => current ? { ...current, datasets: [cleaned, ...current.datasets.filter(item => item.id !== cleaned.id)] } : current); void selectStudyDataset(cleaned.id); }} /></fieldset> : <ResearchPanel title="尚未選取原始資料"><button className={researchPrimary} onClick={() => onSectionChange('sources')}>選取來源</button></ResearchPanel>)}</section>
      <section hidden={!['engineering', 'tuning'].includes(section)}>
        <form onSubmit={event => void saveStudy(event)}><fieldset disabled={!!busy} className="space-y-3"><ResearchStudy study={study} datasets={bootstrap.datasets} capabilities={bootstrap.capabilities} section={section === 'tuning' ? 'tuning' : 'engineering'} onChange={changeStudy} onSelectDataset={id => void selectStudyDataset(id)} /><div className="flex flex-wrap items-center gap-3"><button type="submit" className={researchPrimary}>{busy === 'save' ? '儲存中…' : '儲存不可變研究版本'}</button><button type="button" className={researchPrimary} disabled={!savedId || !!activeRun} onClick={() => void startRun()}>啟動有界研究</button><span className="text-xs text-amber-200">{!savedId ? '先儲存完整工程與搜尋設定' : activeRun ? '已有研究執行中；請追蹤或取消後再啟動' : `使用凍結版本 ${savedId}`}</span></div></fieldset></form>
      </section>
      <section hidden={section !== 'engineering'}>{renderPage('engineering') && <ResearchPanel title="設備配置、精確點位與設定版本">
        <ResearchField label="用於設備綁定的凍結試驗"><select className={researchControl} value={currentRun?.trials.some(item => item.id === engineeringTrial) ? engineeringTrial : ''} onChange={event => setEngineeringTrial(event.target.value)}><option value="">選擇所選 run 的真實試驗</option>{currentRun?.trials.filter(item => item.valid).map(item => <option key={item.id} value={item.id}>{item.method} · {item.id}</option>)}</select></ResearchField>
        {currentRun?.trials.some(item => item.id === engineeringTrial && item.valid) ? <DigitalTwinConfigurationEditor key={`${currentRun.id}:${engineeringTrial}`} runId={currentRun.id} trialId={engineeringTrial} sceneVisible={active && section === 'engineering' && sceneVisible} /> : <p className="mt-3 text-sm text-amber-200">先完成並選取真實 trial，才能依精確控制模型建立設備綁定版本。未綁定點位保持未知；上方工程草稿不會寫入設備。</p>}
      </ResearchPanel>}</section>
      <section hidden={section !== 'identification'}>{renderPage('identification') && <ResearchIdentification study={study} savedId={savedId} run={currentRun} capabilities={bootstrap.capabilities} onChange={changeStudy} onEngineering={() => onSectionChange('engineering')} />}</section>
      <section hidden={!['tuning', 'candidates'].includes(section)}>{currentRun && (renderPage('tuning') || renderPage('candidates')) ? <ResearchResults key={currentRun.id} run={currentRun} busy={!!busy} onCancel={() => void cancelRun()} section={section === 'candidates' ? 'candidates' : 'tuning'} /> : ['tuning', 'candidates'].includes(section) && <ResearchPanel title="尚無已確認的執行結果"><p className="text-sm text-slate-400">選擇上方 run 或先儲存並啟動研究。未完成或失聯不代表通過。</p></ResearchPanel>}</section>
      <section hidden={section !== 'simulation'}>{renderPage('simulation') && (runId ? <DigitalTwin key={runId} runId={runId} active={active && section === 'simulation'} sceneVisible={active && section === 'simulation' && sceneVisible} onResults={() => onSectionChange('candidates')} onDesign={() => onSectionChange('engineering')} /> : <ResearchPanel title="尚未選取研究 run"><button className={researchButton} onClick={() => onSectionChange('overview')}>選取研究任務</button></ResearchPanel>)}</section>
      <section hidden={section !== 'policy'}>{renderPage('policy') && <ResearchPolicy studyId={savedId} assignedModelId={study.model_id} supported={bootstrap.capabilities.continuous_research === true} automaticReidentification={bootstrap.capabilities.automatic_reidentification === true && bootstrap.capabilities.automatic_reidentification_scope === 'verified_server_synthetic_open_loop_only'} />}</section>
    </fieldset>}
  </div>;
}
