import { useCallback, useEffect, useMemo, useState } from 'react';
import { useRuntimeOperation } from '../hooks/useRuntimeOperation';
import { liveEquipmentPoints, validateEquipmentView } from '../lib/equipmentView';
import type { HistoryPoint } from '../lib/historianTrends';
import { appendRuntimeSamples, runtimeControllerVariables, runtimeTags, runtimeTrendData } from '../lib/runtimeView';
import { runtimeError, targetLabel, workspaceRequest } from '../lib/runtimeWorkspace';
import type { EquipmentPoint, EquipmentView } from '../types/EquipmentView';
import type { ProcessTrendData, TrendVariable } from '../types/ProcessTrend';
import type { DeploymentProfile, RuntimeOperation, RuntimeOverview, RuntimeWorkspaceContext, WorkspaceCapabilities } from '../types/RuntimeWorkspace';
import type { RuntimePage } from '../types/Workspace';
import { DigitalTwinScenePanel } from './DigitalTwinScenePanel';
import { ProcessTrendChart } from './ProcessTrendChart';
import { RuntimeControl } from './RuntimeControl';
import { RuntimeEvents, RuntimePerformance } from './RuntimeEvidence';
import { RuntimeHistory } from './RuntimeHistory';
import { RuntimeMaintenance } from './RuntimeMaintenance';
import { RuntimeVersions } from './RuntimeVersions';
import { SettingsView } from './SettingsView';

const button = 'min-h-10 rounded border border-slate-700 bg-slate-900 px-3 py-2 text-sm hover:border-indigo-400 disabled:opacity-40';
const input = 'min-h-10 w-full rounded border border-slate-700 bg-slate-950 px-3 py-2 text-sm';
const emptyHistory = new Map<string, HistoryPoint[]>();
type Props = { section: RuntimePage; active: boolean; sceneVisible: boolean; capabilities: WorkspaceCapabilities;
  onContextChange: (context: RuntimeWorkspaceContext) => void; onNavigate: (page: RuntimePage) => void };

export function RuntimeWorkspace({ section, active, sceneVisible, capabilities, onContextChange, onNavigate }: Props) {
  const [profiles, setProfiles] = useState<DeploymentProfile[]>([]); const [profileHash, setProfileHash] = useState('');
  const [overview, setOverview] = useState<RuntimeOverview | null>(null); const [operations, setOperations] = useState<RuntimeOperation[]>([]);
  const [history, setHistory] = useState<Record<string, Map<string, HistoryPoint[]>>>({});
  const [error, setError] = useState(''); const [profileError, setProfileError] = useState(''); const [loading, setLoading] = useState(false);
  const [revision, setRevision] = useState(0); const [available, setAvailable] = useState(false);
  const [profilesRevision, setProfilesRevision] = useState(0);
  const [trendMode, setTrendMode] = useState<'live' | 'history'>('live');
  const [clock, setClock] = useState(Date.now()); const [clockAnchor, setClockAnchor] = useState<{ server: number; monotonic: number } | null>(null);
  const [selection, setSelection] = useState<Record<string, { equipment: string | null; binding: string | null }>>({});
  const refresh = useCallback(() => setRevision(value => value + 1), []);
  const selectedHash = profiles.some(profile => profile.profile_hash === profileHash) ? profileHash : profiles[0]?.profile_hash ?? '';
  const profile = profiles.find(item => item.profile_hash === selectedHash);
  const snapshot = overview?.profiles.find(item => item.profile_hash === selectedHash);
  const permissions = capabilities.scopes.find(scope => scope.profile_hash === selectedHash)?.permissions ?? [];
  const tracker = useRuntimeOperation(capabilities.user.id, selectedHash, active, refresh);
  useEffect(() => {
    if (!capabilities.runtime_enabled) return;
    const controller = new AbortController(); setLoading(true); setProfileError('');
    void workspaceRequest<{ profiles: DeploymentProfile[] }>('/api/runtime/profiles', undefined, controller.signal).then(value => {
      if (controller.signal.aborted) return;
      if (!Array.isArray(value.profiles)) throw new Error('未收到有效的核准控制群組清單。');
      setProfiles(value.profiles);
    }).catch(cause => { if (!controller.signal.aborted) setProfileError(runtimeError(cause)); })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [capabilities.runtime_enabled, capabilities.user.id, profilesRevision]);
  useEffect(() => {
    if (!active || !capabilities.runtime_enabled) return;
    const controller = new AbortController(); let timer: number | undefined;
    async function observe() {
      try {
        const [state, ledger] = await Promise.all([workspaceRequest<RuntimeOverview>('/api/runtime/overview', undefined, controller.signal), workspaceRequest<{ operations: RuntimeOperation[] }>('/api/runtime/operations', undefined, controller.signal)]);
        if (controller.signal.aborted) return;
        const serverTime = Date.parse(state.observed_at);
        if (!Number.isFinite(serverTime) || !Array.isArray(state.profiles) || !Array.isArray(ledger.operations)) throw new Error('Runtime 狀態缺少可核對的伺服器時刻或操作紀錄。');
        setOverview(state); setOperations(ledger.operations); setClock(serverTime); setClockAnchor({ server: serverTime, monotonic: performance.now() }); setAvailable(true); setError('');
        setHistory(current => {
          const next = { ...current };
          for (const observed of state.profiles) next[observed.profile_hash] = appendRuntimeSamples(current[observed.profile_hash] ?? emptyHistory, observed.signals, 600);
          return next;
        });
      } catch (cause) { if (!controller.signal.aborted) { setAvailable(false); setError(runtimeError(cause)); } }
      if (!controller.signal.aborted) timer = window.setTimeout(() => void observe(), 2000);
    }
    void observe(); return () => { controller.abort(); window.clearTimeout(timer); };
  }, [active, capabilities.runtime_enabled, revision]);
  useEffect(() => {
    if (!active || !clockAnchor) return;
    const timer = window.setInterval(() => setClock(clockAnchor.server + performance.now() - clockAnchor.monotonic), 1000);
    return () => window.clearInterval(timer);
  }, [active, clockAnchor]);
  const connected = active && available && !!clockAnchor && performance.now() - clockAnchor.monotonic < 10_000;
  useEffect(() => {
    onContextChange({ targetLabel: profile ? targetLabel(profile.target) : '尚無已核准控制群組',
      sourceLabel: profile ? `OPC UA · ${profile.endpoint}` : '未取得核准來源', versionLabel: snapshot ? `活動模型 ${snapshot.version.slice(0, 12)} · Epoch ${snapshot.epoch}` : '活動版本未知',
      health: connected ? snapshot?.health ?? 'unknown' : 'disconnected', observedAt: overview?.observed_at ?? null });
  }, [profile, snapshot, connected, overview?.observed_at, onContextChange]);
  const viewState = useMemo(() => {
    if (!profile?.equipment_view) return { view: null, error: '這個 profile 尚未提供經核對的設備配置。不會自動把任意 CV／MV／DV 套到示意設備。' };
    try { const view = validateEquipmentView(profile.equipment_view, runtimeControllerVariables(profile)); if (!view.confirmed) throw new Error('設備配置尚未經工程人員確認。'); return { view, error: '' }; }
    catch (cause) { return { view: null, error: runtimeError(cause) }; }
  }, [profile]);
  const points = useMemo<EquipmentPoint[]>(() => {
    if (!profile || !viewState.view) return [];
    return liveEquipmentPoints(viewState.view, runtimeControllerVariables(profile), runtimeTags(snapshot?.signals ?? []), clock, profile.maximum_age_ms, connected);
  }, [profile, viewState.view, snapshot, clock, connected]);
  const trend = useMemo<{ data: ProcessTrendData | null; error: string }>(() => {
    if (!profile) return { data: null, error: '' };
    try { return { data: runtimeTrendData(profile, snapshot, history[selectedHash] ?? emptyHistory, clock, connected), error: '' }; }
    catch (cause) { return { data: null, error: runtimeError(cause) }; }
  }, [profile, snapshot, history, selectedHash, clock, connected]);
  const chosen = selection[selectedHash] ?? { equipment: null, binding: null };
  const selectedEquipment = viewState.view?.equipment.some(item => item.id === chosen.equipment) ? chosen.equipment : viewState.view?.equipment[0]?.id ?? null;
  const selectedBinding = viewState.view?.bindings.find(binding => binding.id === chosen.binding);
  const focusedVariable = selectedBinding && trend.data?.variables.find(variable => variable.role.toLowerCase() === selectedBinding.role && variable.name === selectedBinding.variable_name && variable.unit === selectedBinding.unit);
  function chooseEquipment(id: string) { setSelection(current => ({ ...current, [selectedHash]: { equipment: id, binding: null } })); }
  function chooseBinding(id: string) { const binding = viewState.view?.bindings.find(item => item.id === id); if (binding) setSelection(current => ({ ...current, [selectedHash]: { equipment: binding.equipment_id, binding: id } })); }
  function focusVariable(variable: TrendVariable) {
    const bindings = viewState.view?.bindings.filter(binding => binding.role === variable.role.toLowerCase() && binding.variable_name === variable.name && binding.unit === variable.unit) ?? [];
    const binding = bindings.find(item => item.channel === 'actual') ?? bindings[0];
    if (binding) chooseBinding(binding.id);
  }
  if (!capabilities.runtime_enabled) return null;
  return <div className="space-y-4 px-3 pb-6 sm:px-5">
    <div className="flex flex-wrap items-end gap-3"><label className="min-w-0 flex-1 space-y-1 text-sm">已核准控制群組<select className={input} value={selectedHash} disabled={loading || !profiles.length} onChange={event => setProfileHash(event.target.value)}>{!profiles.length && <option value="">尚無可見的核准 profile</option>}{profiles.map(item => <option key={item.profile_hash} value={item.profile_hash}>{targetLabel(item.target)}</option>)}</select></label><button type="button" className={button} onClick={refresh}>重新查詢狀態</button></div>
    {(error || profileError) && <p role="alert" className="rounded border border-rose-800 p-3 text-sm text-rose-300">{error || profileError}</p>}
    {profileError && <button type="button" className={button} disabled={loading} onClick={() => setProfilesRevision(value => value + 1)}>重新讀取核准控制群組</button>}
    {loading && <p role="status" className="text-sm text-slate-400">讀取核准設備配置…</p>}
    {!loading && !profile && <p className="rounded border border-amber-800 p-4 text-sm text-amber-200">此帳號沒有可見的已核准控制群組。管理員須先配置精確來源、設備、點位、工程界線與 profile 範圍授權；系統不會自行猜測現場映射。</p>}
    {profile && <>
      <div className="flex flex-wrap gap-2 text-xs"><span className={`rounded border px-2 py-1 ${connected && snapshot?.health === 'good' ? 'border-emerald-800 text-emerald-300' : 'border-amber-800 text-amber-200'}`}>{connected ? `原生狀態：${snapshot?.health ?? '未知'}` : '觀測連線未確認'}</span><span className="rounded border border-slate-700 px-2 py-1">控制權 {snapshot?.authority ?? '未知'}</span><span className="rounded border border-slate-700 px-2 py-1">群組 {profile.target.group}</span><span className="rounded border border-slate-700 px-2 py-1">{profile.variables.filter(variable => variable.role === 'CV').length} CV · {profile.variables.filter(variable => variable.role === 'MV').length} MV · {profile.variables.filter(variable => variable.role === 'DV').length} DV</span></div>
      {viewState.view ? <DigitalTwinScenePanel key={profile.profile_hash} view={viewState.view} points={points} selectedEquipmentId={selectedEquipment} selectedBindingId={selectedBinding?.id ?? null} onSelectEquipment={chooseEquipment} onSelectBinding={chooseBinding} motion={!connected ? 'stale' : points.some(point => point.quality === 'good' && !point.stale) ? 'running' : 'unavailable'} source="live" visible={sceneVisible && active} /> : sceneVisible && <p className="rounded border border-amber-800 p-3 text-sm text-amber-200">{viewState.error}</p>}
      <div hidden={section !== 'overview'} className="space-y-4">
        <RuntimePointTable points={points} view={viewState.view} selectedEquipment={selectedEquipment} onSelect={chooseBinding} />
        <section className="flex flex-wrap items-start justify-between gap-3 rounded border border-slate-700 p-3"><div><h2 className="text-sm font-medium">需要處理</h2>{snapshot?.blockers.length ? <ul className="mt-2 list-inside list-disc text-xs text-amber-200">{snapshot.blockers.map(blocker => <li key={blocker}>{blocker}</li>)}</ul> : <p className="mt-2 text-xs text-slate-400">{snapshot?.health === 'good' ? '目前原生健康檢查通過；這不取代 DCS／安全系統的獨立保護。' : '尚未取得可證明控制就緒的原生狀態。'}</p>}</div><div className="flex gap-2"><button type="button" className={button} onClick={() => onNavigate('control')}>查看接管條件</button><button type="button" className={button} onClick={() => onNavigate('events')}>查看事件</button></div></section>
      </div>
      <div hidden={section !== 'trends'} inert={section !== 'trends'} className="flex flex-wrap gap-2" role="group" aria-label="趨勢資料來源">
        <button type="button" className={button} aria-pressed={trendMode === 'live'} onClick={() => setTrendMode('live')}>即時與最新完整預測</button>
        <button type="button" className={button} aria-pressed={trendMode === 'history'} onClick={() => setTrendMode('history')}>指定時間歷史</button>
      </div>
      <div hidden={section !== 'overview' && !(section === 'trends' && trendMode === 'live')}>{trend.error ? <p role="alert" className="text-sm text-rose-300">{trend.error}</p> : trend.data && <ProcessTrendChart data={trend.data} storageKey={`runtime:${capabilities.user.id}:${profile.profile_hash}:trends`} initialView="role_blocks" currentTime={clock} focusedVariableId={focusedVariable?.id ?? null} onVariableFocus={focusVariable} />}</div>
      <div hidden={section !== 'trends' || trendMode !== 'history'} inert={section !== 'trends' || trendMode !== 'history'}>
        <RuntimeHistory key={profile.profile_hash} profile={profile} now={clock} storageKey={`runtime:${capabilities.user.id}:${profile.profile_hash}:history`} focusedVariableId={focusedVariable?.id ?? null} onVariableFocus={focusVariable} />
      </div>
      <div hidden={section !== 'control'} inert={section !== 'control'}><RuntimeControl key={profile.profile_hash} profile={profile} snapshot={snapshot} permissions={permissions} connected={connected} lifecycle={capabilities.capabilities.engine_lifecycle} active={active && section === 'control'} tracker={tracker} operations={operations} /></div>
      <div hidden={section !== 'events'} inert={section !== 'events'}><RuntimeEvents profileHash={profile.profile_hash} active={active && section === 'events'} operations={operations} onRefresh={refresh} /></div>
      <div hidden={section !== 'performance'} inert={section !== 'performance'}><RuntimePerformance key={profile.profile_hash} profileHash={profile.profile_hash} /></div>
      <div hidden={section !== 'versions'} inert={section !== 'versions'}><RuntimeVersions key={profile.profile_hash} profile={profile} snapshot={snapshot} permissions={permissions} userId={capabilities.user.id} tracker={tracker} active={active && section === 'versions'} connected={connected} /></div>
      <div hidden={section !== 'administration'} inert={section !== 'administration'} className="space-y-4"><section className="space-y-3 rounded-xl border border-slate-700 p-4"><h2 className="font-medium">核准連線與權限 · 唯讀</h2><dl className="grid gap-3 text-xs sm:grid-cols-2"><div><dt className="text-slate-400">精確 OPC 來源</dt><dd className="break-all">{profile.endpoint}</dd></div><div><dt className="text-slate-400">目前帳號範圍權限</dt><dd>{permissions.join('、') || '無'}</dd></div><div><dt className="text-slate-400">Profile SHA-256</dt><dd className="break-all font-mono">{profile.profile_hash}</dd></div><div><dt className="text-slate-400">核准與投運證據</dt><dd className="break-all">{profile.approved_by} · {profile.commissioning_evidence}</dd></div><div><dt className="text-slate-400">資料時間契約</dt><dd>最大年齡 {profile.maximum_age_ms} ms · 最大時差 {profile.maximum_skew_ms} ms</dd></div><div><dt className="text-slate-400">DCS Watchdog 證據</dt><dd className="break-all">{profile.handoff.watchdog_evidence}</dd></div></dl><p className="text-xs text-amber-200">現場端點、憑證、點位與工程硬限制不是即時調參。修改後必須重新核准 profile；此頁不提供繞過版本／權限的任意連線或 OPC 寫入。</p><details><summary className="cursor-pointer text-sm">完整工程映射與操作範圍</summary><pre className="mt-2 max-h-80 overflow-auto whitespace-pre-wrap break-all text-xs">{JSON.stringify({ variables: profile.variables, operating_envelope: profile.operating_envelope, handoff: profile.handoff, model_binding: profile.model_binding }, null, 2)}</pre></details></section></div>
    </>}
    <div hidden={section !== 'administration'} inert={section !== 'administration'}>
      <RuntimeMaintenance active={active && section === 'administration'} permitted={capabilities.user.role === 'admin' && capabilities.scopes.length > 0 && capabilities.scopes.every(scope => scope.permissions.includes('runtime.maintenance'))} />
    </div>
    <div hidden={section !== 'administration'} inert={section !== 'administration'}><SettingsView isAdmin={capabilities.user.role === 'admin'} active={active && section === 'administration'} /></div>
  </div>;
}

function RuntimePointTable({ points, view, selectedEquipment, onSelect }: { points: EquipmentPoint[]; view: EquipmentView | null; selectedEquipment: string | null; onSelect: (id: string) => void }) {
  const [all, setAll] = useState(false);
  const visible = all ? points : points.filter(point => point.equipmentId === selectedEquipment);
  return <section className="space-y-2"><div className="flex flex-wrap items-center justify-between gap-2"><h2 className="font-medium">{all ? '全部設備點位' : view?.equipment.find(item => item.id === selectedEquipment)?.name ?? '設備點位'}</h2><label className="flex items-center gap-2 text-xs"><input type="checkbox" checked={all} onChange={event => setAll(event.target.checked)} />顯示全部點位</label></div>{!visible.length && <p className="text-sm text-slate-400">此設備沒有已綁定的可見點位。未量測的設備不會顯示假值或運轉動畫。</p>}<div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-3">{visible.map(point => <button type="button" key={point.bindingId} onClick={() => onSelect(point.bindingId)} className="min-w-0 rounded border border-slate-700 bg-slate-900 p-3 text-left hover:border-indigo-400"><div className="flex justify-between gap-2 text-xs"><span>{point.role.toUpperCase()} · {point.name} · {point.channel}</span><span className={point.stale || point.quality !== 'good' ? 'text-amber-200' : 'text-emerald-300'}>{point.stale ? '過期／未連線' : point.quality}</span></div><p className="my-2 text-xl tabular-nums">{point.value === null ? '—' : point.value.toLocaleString(undefined, { maximumSignificantDigits: 6 })}<span className="ml-2 text-xs text-slate-400">{point.unit}</span></p><p className="text-xs text-slate-500">{point.time === null ? '無來源時刻' : new Date(point.time).toISOString()}</p><p className="mt-1 text-xs text-slate-500">{point.sourceLabel}</p></button>)}</div></section>;
}
