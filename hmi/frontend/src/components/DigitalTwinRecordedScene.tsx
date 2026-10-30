import { useEffect, useMemo, useState } from 'react';
import type { TwinArchiveFrame, TwinManifest, TwinSession, TwinVariableFocus } from '../types/DigitalTwin';
import type { EquipmentViewVersion } from '../types/EquipmentView';
import type { TrendVariable } from '../types/ProcessTrend';
import { archivedTwinTrend } from '../lib/digitalTwin';
import { simulatedEquipmentPoints } from '../lib/equipmentView';
import { unboundEquipmentTemplate } from '../lib/twinEquipmentTemplate';
import { researchError, researchRequest } from '../lib/research';
import { DigitalTwinArchive } from './DigitalTwinArchive';
import type { TwinArchiveSource } from './DigitalTwinArchive';
import { DigitalTwinScenePanel } from './DigitalTwinScenePanel';
import { ResearchNotice, researchButton, researchControl } from './ResearchControls';

/** Read-only indexed archive navigation. No playback timer and no simulation commands. */
export function DigitalTwinRecordedScene({ manifest, trialId, sources, viewVersion, session, sceneVisible = true, storageKey }: {
  manifest: TwinManifest; trialId: string; sources: TwinArchiveSource[]; viewVersion?: EquipmentViewVersion | null;
  session?: TwinSession; sceneVisible?: boolean; storageKey: string;
}) {
  const [sample, setSample] = useState(0);
  const [loaded, setLoaded] = useState<{ sourceKey: string; requestKey: string; frame: TwinArchiveFrame } | null>(null);
  const [result, setResult] = useState<{ key: string; error: string } | null>(null);
  const [revision, setRevision] = useState(0);
  const [focus, setFocus] = useState<TwinVariableFocus | null>(null);
  const [equipmentId, setEquipmentId] = useState<string | null>(null);
  const [bindingId, setBindingId] = useState<string | null>(null);
  const [selectionVersion, setSelectionVersion] = useState(viewVersion?.id);
  if (selectionVersion !== viewVersion?.id) {
    setSelectionVersion(viewVersion?.id);
    setEquipmentId(null); setBindingId(null); setFocus(null);
  }
  const path = session ? `/twin/sessions/${encodeURIComponent(session.id)}/frame` : `/runs/${encodeURIComponent(manifest.run_id)}/trials/${encodeURIComponent(trialId)}/frame`;
  const expectedHash = session?.controller_sha256 ?? manifest.trials.find(item => item.id === trialId)?.controller_sha256;
  const sourceKey = JSON.stringify([path, expectedHash]);
  const [previousSource, setPreviousSource] = useState(sourceKey);
  if (previousSource !== sourceKey) {
    setPreviousSource(sourceKey);
    setSample(0);
  }
  const requestKey = JSON.stringify([sourceKey, sample, revision]);
  const loading = result?.key !== requestKey;
  const error = result?.key === requestKey ? result.error : '';
  const archive = loaded?.sourceKey === sourceKey ? loaded.frame : null;
  useEffect(() => {
    const controller = new AbortController();
    void researchRequest<TwinArchiveFrame>(`${path}?sample=${sample}`, undefined, controller.signal).then(next => {
      if (controller.signal.aborted) return;
      if (next.sample !== sample || !Number.isSafeInteger(next.total_samples) || next.total_samples <= sample || (expectedHash && next.controller_sha256 !== expectedHash)) throw new Error('封存影格序號或控制器身分不相容。');
      setLoaded({ sourceKey, requestKey, frame: next });
      setResult({ key: requestKey, error: '' });
    }).catch(failure => { if (!controller.signal.aborted) setResult({ key: requestKey, error: researchError(failure) }); });
    return () => controller.abort();
  }, [path, sample, expectedHash, revision, sourceKey, requestKey]);
  const view = useMemo(() => viewVersion?.view ?? unboundEquipmentTemplate(), [viewVersion]);
  const versionMatches = !viewVersion || (viewVersion.run_id === manifest.run_id && viewVersion.trial_id === trialId && viewVersion.controller_sha256 === manifest.trials.find(item => item.id === trialId)?.controller_sha256);
  const confirmed = loaded?.requestKey === requestKey && archive?.sample === sample && !loading && !error && versionMatches ? archive : null;
  const sourceId = sources[0]?.id ?? '';
  const forecast = useMemo(() => confirmed ? archivedTwinTrend(manifest, confirmed, sourceId, trialId, session?.settings_version_id) : null, [manifest, confirmed, sourceId, trialId, session?.settings_version_id]);
  const origin = confirmed?.time_basis === 'relative' ? 0 : Date.parse(confirmed?.start ?? '');
  const time = confirmed ? origin + confirmed.frame.time_seconds * 1000 : null;
  const points = useMemo(() => simulatedEquipmentPoints(view, confirmed?.variables ?? manifest.variables, confirmed?.frame, origin, !confirmed, 'archive'), [view, confirmed, manifest.variables, origin]);
  const selectVariable = (variable: TrendVariable) => {
    if (variable.role === 'OTHER') return;
    setFocus({ role: variable.role, name: variable.name, unit: variable.unit });
    const binding = view.bindings.find(item => item.role.toUpperCase() === variable.role && item.variable_name === variable.name && item.unit === variable.unit);
    setBindingId(binding?.id ?? null); setEquipmentId(binding?.equipment_id ?? null);
  };
  const selectBinding = (id: string) => {
    const binding = view.bindings.find(item => item.id === id); setBindingId(id); setEquipmentId(binding?.equipment_id ?? null);
    if (binding && binding.role !== 'telemetry') setFocus({ role: binding.role.toUpperCase() as TwinVariableFocus['role'], name: binding.variable_name, unit: binding.unit });
  };
  const selectEquipment = (id: string) => {
    const binding = view.bindings.find(item => item.equipment_id === id && item.role !== 'telemetry' && item.channel === 'actual');
    if (binding) selectBinding(binding.id);
    else { setEquipmentId(id); setBindingId(null); setFocus(null); }
  };
  function seekTime(next: number | null) {
    if (next === null || !confirmed || !Number.isFinite(next)) return;
    const offset = Math.round((next - origin - confirmed.frame.time_seconds * 1000) / (confirmed.sample_time * 1000));
    setSample(Math.max(0, Math.min(confirmed.total_samples - 1, confirmed.sample + offset)));
  }
  return <div className="min-w-0 space-y-3">
    <p className="text-xs text-amber-200">封存唯讀檢視：游標查詢原生影格與可用的該次完整預測，不改模擬器狀態、不重跑、不模擬設備運轉。場景只顯示 A；B 僅作曲線比較。</p>
    <div className="flex flex-wrap items-center gap-2">
      <label className="text-sm">封存掃描（從 0 起）<input aria-label="封存掃描序號" className={`${researchControl} ml-2 w-36`} type="number" min={0} max={archive ? archive.total_samples - 1 : undefined} step={1} value={sample} onChange={event => { const value = Number(event.target.value); if (Number.isSafeInteger(value) && value >= 0 && (!archive || value < archive.total_samples)) setSample(value); }} /></label>
      <button type="button" className={researchButton} disabled={sample <= 0} onClick={() => setSample(value => value - 1)}>前一掃描</button>
      <button type="button" className={researchButton} disabled={!archive || sample >= archive.total_samples - 1} onClick={() => setSample(value => value + 1)}>後一掃描</button>
      {archive && <span className="text-xs">完整封存 {archive.total_samples.toLocaleString()} 掃描</span>}
      <button type="button" className={researchButton} disabled={loading} onClick={() => setRevision(value => value + 1)}>重新讀取原影格</button>
    </div>
    {archive && <input className="w-full" aria-label="封存掃描定位" type="range" min={0} max={archive.total_samples - 1} step={1} value={sample} onChange={event => setSample(Number(event.target.value))} />}
    {loading && <p role="status" className="text-xs">讀取所選原生影格；舊值不冒充目前游標…</p>}
    {error && <ResearchNotice error={error} />}{!versionMatches && <ResearchNotice error="設備配置與封存來源不相容，點位保持未取得。" />}
    <DigitalTwinScenePanel key={viewVersion?.id ?? 'unbound'} view={view} points={points} selectedEquipmentId={equipmentId} selectedBindingId={bindingId} onSelectEquipment={selectEquipment} onSelectBinding={selectBinding} motion={confirmed ? 'paused' : 'unavailable'} source="archive" visible={sceneVisible} />
    {confirmed && <p className="break-all text-xs text-slate-400">已確認 sample {confirmed.sample} · 模擬 t={confirmed.frame.time_seconds}s · {confirmed.time_basis === 'relative' ? '相對時鐘' : time !== null && Number.isFinite(time) ? new Date(time).toISOString() : 'UTC 時間無效'} · 控制器 {confirmed.controller_sha256} · 設備版本 {viewVersion?.id ?? '未綁定'}</p>}
    {confirmed && !confirmed.latest_forecast && <p className="text-xs text-amber-200">此掃描未封存完整預測；不以單步預測或目前模型回填。</p>}
    <DigitalTwinArchive sources={sources} storageKey={storageKey} focus={focus} onVariableFocus={selectVariable} cursorTime={time} onCursorTimeChange={seekTime} currentTime={time ?? undefined} forecastData={forecast} />
  </div>;
}
