import { useCallback, useEffect, useRef, useState } from 'react';
import type { TwinManifest, TwinSession } from '../types/DigitalTwin';
import { researchError, researchRequest } from '../lib/research';
import { DigitalTwinModel } from './DigitalTwinModel';
import { DigitalTwinSession } from './DigitalTwinSession';
import { DigitalTwinConfigurationEditor } from './DigitalTwinConfigurationEditor';
import type { TwinConfigurationSelection } from './DigitalTwinConfigurationEditor';
import { DigitalTwinRecordedScene } from './DigitalTwinRecordedScene';
import { ResearchField, ResearchNotice, ResearchPanel, researchButton, researchControl, researchPrimary } from './ResearchControls';

export function DigitalTwin({ runId, onResults, onDesign, sceneVisible = true, active = true }: { runId: string; onResults: () => void; onDesign: () => void; sceneVisible?: boolean; active?: boolean }) {
  const [manifest, setManifest] = useState<TwinManifest | null>(null);
  const [trialId, setTrialId] = useState('');
  const [trialB, setTrialB] = useState('');
  const [mode, setMode] = useState<'recorded' | 'session'>('recorded');
  const [scenario, setScenario] = useState<'selection' | 'full'>('full');
  const [transport, setTransport] = useState<'direct' | 'opc-da'>('direct');
  const [sessionId, setSessionId] = useState('');
  const [revision, setRevision] = useState(0);
  const [loading, setLoading] = useState(true);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState('');
  const [configuration, setConfiguration] = useState<TwinConfigurationSelection | null>(null);
  const createRequest = useRef<AbortController | null>(null);
  useEffect(() => {
    createRequest.current?.abort(); setCreating(false); setManifest(null); setConfiguration(null);
    setTrialId(''); setTrialB(''); setSessionId(''); setError('');
  }, [runId]);
  useEffect(() => () => createRequest.current?.abort(), []);
  useEffect(() => {
    const controller = new AbortController();
    setLoading(true); setError('');
    void researchRequest<TwinManifest>(`/runs/${encodeURIComponent(runId)}/twin`, undefined, controller.signal).then(data => {
      if (controller.signal.aborted) return;
      setManifest(data);
      setTrialId(current => data.trials.some(item => item.id === current && item.valid) ? current : Object.values(data.selected_trials).find(id => data.trials.some(item => item.id === id && item.valid)) || data.trials.find(item => item.valid)?.id || '');
    }).catch(failure => { if (!controller.signal.aborted) setError(researchError(failure)); }).finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [runId, revision]);
  const updateSession = useCallback((session: TwinSession) => {
    setManifest(current => current ? { ...current, sessions: [session, ...current.sessions.filter(item => item.id !== session.id)] } : current);
  }, []);
  function changeTrial(id: string) {
    createRequest.current?.abort(); setCreating(false); setTrialId(id); setTrialB(''); setSessionId(''); setConfiguration(null); setError('');
  }
  async function createSession() {
    if (creating || !trialId) return;
    const controller = new AbortController(); createRequest.current = controller;
    setCreating(true); setError(''); setSessionId('');
    try {
      const session = await researchRequest<TwinSession>('/twin/sessions', { run_id: runId, trial_id: trialId, scenario, transport,
        ...(configuration?.equipmentViewId ? { equipment_view_id: configuration.equipmentViewId } : {}),
        ...(configuration?.settingsVersionId ? { settings_version_id: configuration.settingsVersionId } : {}),
      }, controller.signal);
      if (controller.signal.aborted) return;
      updateSession(session); setSessionId(session.id);
    } catch (failure) {
      if (!controller.signal.aborted) setError(`建立工作階段未確認：${researchError(failure)}。不會自動重試；請更新孿生清單確認是否已建立。`);
    } finally { if (!controller.signal.aborted) setCreating(false); }
  }
  const trial = manifest?.trials.find(item => item.id === trialId);
  const validTrials = manifest?.trials.filter(item => item.valid) ?? [];
  const sources = [trialId, ...(trialB && trialB !== trialId ? [trialB] : [])].filter(Boolean).map(id => ({
    id: `${runId}:${id}`, label: `${manifest?.trials.find(item => item.id === id)?.method ?? ''} · ${id}`,
    path: `/runs/${encodeURIComponent(runId)}/trials/${encodeURIComponent(id)}/series`,
  }));
  return <div className="min-w-0 space-y-3" aria-label="數位孿生工作區">
    <ResearchPanel title="數位孿生 · 研究內的閉迴路驗證">
      <div className="space-y-3"><p className="text-sm text-slate-300">清理資料 → 辨識與模型資格 → Autoresearch（每個試驗已執行閉迴路模擬）→ 凍結試驗再驗證。</p><div className="flex flex-wrap gap-2"><button type="button" className={researchButton} onClick={onDesign}>返回研究設計與模型資格</button><button type="button" className={researchButton} onClick={onResults}>返回結果與報告</button><button type="button" className={researchButton} disabled={loading || creating} onClick={() => setRevision(value => value + 1)}>更新孿生清單</button></div><p className="text-xs text-amber-200">純研究環境，無自動部署。新工作階段凍結原始控制器、被控模型與情境；不改寫既有證據。</p></div>
    </ResearchPanel>
    {loading && <p role="status" className="text-sm text-indigo-300">讀取試驗與模型清單…</p>}
    {error && <ResearchNotice error={error} />}
    {manifest && manifest.run_id === runId && <>
      <ResearchPanel title="選擇凍結試驗">
        <div className="space-y-3"><ResearchField label="孿生試驗 A"><select className={researchControl} value={trialId} disabled={creating} onChange={event => changeTrial(event.target.value)}><option value="">選擇有效試驗</option>{validTrials.map(item => <option key={item.id} value={item.id}>{item.method} · {item.id} · 第 {item.round} 輪{Object.values(manifest.selected_trials).includes(item.id) ? ' · 已選版' : ''}{item.eligible === true && !item.diagnostic_only ? '' : ' · 僅診斷'}</option>)}</select></ResearchField>
          <div className="flex flex-wrap gap-2" role="group" aria-label="孿生工作模式"><button type="button" className={researchButton} aria-pressed={mode === 'recorded'} disabled={creating} onClick={() => setMode('recorded')}>已記錄試驗 A/B</button><button type="button" className={researchButton} aria-pressed={mode === 'session'} disabled={creating} onClick={() => setMode('session')}>新模擬與工作階段</button></div>
          {!validTrials.length && <p className="text-sm text-amber-200">尚無有效封存試驗。研究狀態：{manifest.run_status} · {manifest.stage}。可返回結果查看進度與資格檢查。</p>}
        </div>
      </ResearchPanel>
      <DigitalTwinModel manifest={manifest} trial={trial} />
      {trial && <DigitalTwinConfigurationEditor key={`${runId}:${trialId}`} runId={runId} trialId={trialId} sceneVisible={false} onSelection={setConfiguration} />}
      {trial && <div hidden={mode !== 'recorded'}><ResearchPanel title="已記錄試驗 · 驗證情境 A/B">
        <div className="space-y-3"><p className="text-xs text-slate-400">實際試驗封存，僅驗證區間，不是全期重跑。A/B 來自同一研究的固定情境與時間軸；選參分數不因檢視改變。</p><ResearchField label="孿生比較試驗 B"><select className={researchControl} value={trialB} onChange={event => setTrialB(event.target.value)}><option value="">不比較</option>{validTrials.filter(item => item.id !== trialId).map(item => <option key={item.id} value={item.id}>{item.method} · {item.id} · 第 {item.round} 輪</option>)}</select></ResearchField>
          <DigitalTwinRecordedScene key={`${runId}:${trialId}`} manifest={manifest} trialId={trialId} sources={sources} viewVersion={configuration?.configuration.equipment_views.find(item => item.id === configuration.equipmentViewId)} sceneVisible={sceneVisible && active && mode === 'recorded'} storageKey={`authrity.twin.recorded.${runId}`} />
          {trialB && <details className="rounded-lg border border-slate-700 p-3"><summary className="cursor-pointer text-sm text-indigo-300">比較試驗 B 參數與來源</summary><div className="mt-3"><DigitalTwinModel manifest={manifest} trial={manifest.trials.find(item => item.id === trialB)} /></div></details>}
        </div>
      </ResearchPanel></div>}
      <div hidden={mode !== 'session'}>
        <ResearchPanel title="建立或重新開啟工作階段">
          <div className="space-y-3"><div className="grid gap-3 sm:grid-cols-2"><ResearchField label="孿生情境"><select className={researchControl} disabled={creating} value={scenario} onChange={event => { setScenario(event.target.value as typeof scenario); setSessionId(''); }}><option value="full">完整情境重跑 · 探索用途（含留出資料）</option><option value="selection">原選參驗證情境</option></select></ResearchField><ResearchField label="孿生傳輸路徑"><select className={researchControl} disabled={creating} value={transport} onChange={event => { setTransport(event.target.value as typeof transport); setSessionId(''); }}><option value="direct">Direct · 隔離原生模擬</option><option value="opc-da">OPC DA · 已設定的隔離研究橋接</option></select></ResearchField></div>
            <p className="text-xs text-slate-400">使用儲存的控制器原始位元組，不重套調參倍率。完整情境可能重用測試資料，只供探索；不提升試驗資格。</p><button type="button" className={researchPrimary} disabled={creating || !trial} onClick={() => void createSession()}>{creating ? '凍結模型並建立中…' : '建立凍結模型工作階段'}</button>
            <ResearchField label="重新開啟孿生工作階段"><select className={researchControl} value={sessionId} disabled={creating} onChange={event => {
              const found = manifest.sessions.find(item => item.id === event.target.value);
              setSessionId(event.target.value); setError('');
              if (found) { setTrialId(found.trial_id); setConfiguration(null); setScenario(found.scenario); setTransport(found.transport); }
            }}><option value="">選擇已建立或已封存工作階段</option>{manifest.sessions.map(item => <option key={item.id} value={item.id}>{item.trial_id} · {item.scenario} · {item.status} · {item.samples}/{item.requested_samples} · {item.id}</option>)}</select></ResearchField>
          </div>
        </ResearchPanel>
        {sessionId && <DigitalTwinSession key={sessionId} id={sessionId} manifest={manifest} onUpdate={updateSession} sceneVisible={sceneVisible && mode === 'session'} active={active && mode === 'session'} />}
      </div>
    </>}
  </div>;
}
