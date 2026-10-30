import { useEffect, useMemo, useRef, useState } from 'react';
import type { EquipmentBinding, EquipmentLayout, EquipmentView, EquipmentViewVersion, TwinConfiguration, TwinSettingsVersion } from '../types/EquipmentView';
import { validateEquipmentView, equipmentNode, downloadEquipmentView } from '../lib/equipmentView';
import { unboundEquipmentTemplate } from '../lib/twinEquipmentTemplate';
import { researchError, researchRequest } from '../lib/research';
import { DigitalTwinScenePanel } from './DigitalTwinScenePanel';
import { ResearchField, ResearchNotice, ResearchPanel, researchButton, researchControl, researchPrimary } from './ResearchControls';

export type TwinConfigurationSelection = { equipmentViewId: string; settingsVersionId: string; configuration: TwinConfiguration };
export type DigitalTwinConfigurationEditorProps = {
  runId: string; trialId: string; sceneVisible?: boolean;
  onSelection?: (selection: TwinConfigurationSelection) => void;
};

export function DigitalTwinConfigurationEditor({ runId, trialId, sceneVisible = true, onSelection }: DigitalTwinConfigurationEditorProps) {
  const [configuration, setConfiguration] = useState<TwinConfiguration | null>(null);
  const [viewId, setViewId] = useState('');
  const [settingsId, setSettingsId] = useState('');
  const [draft, setDraft] = useState<EquipmentView>(() => unboundEquipmentTemplate());
  const [geometry, setGeometry] = useState('');
  const [settings, setSettings] = useState('');
  const [name, setName] = useState('');
  const [typeConfirmed, setTypeConfirmed] = useState(false);
  const [topologyConfirmed, setTopologyConfirmed] = useState(false);
  const [selectedEquipment, setSelectedEquipment] = useState<string | null>(null);
  const [selectedBinding, setSelectedBinding] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [revision, setRevision] = useState(0);
  const notify = useRef(onSelection); notify.current = onSelection;
  const target = `${runId}:${trialId}`;
  const currentTarget = useRef(target); currentTarget.current = target;
  const loadedTarget = useRef('');
  useEffect(() => {
    const controller = new AbortController();
    setBusy(true); setError('');
    if (loadedTarget.current !== target) {
      setConfiguration(null); setViewId(''); setSettingsId(''); setDraft(unboundEquipmentTemplate());
      setGeometry(''); setSettings(''); setTypeConfirmed(false); setTopologyConfirmed(false); setNotice('');
      setSelectedEquipment(null); setSelectedBinding(null);
    }
    if (!runId || !trialId) { setBusy(false); return () => controller.abort(); }
    void researchRequest<TwinConfiguration>(`/runs/${encodeURIComponent(runId)}/trials/${encodeURIComponent(trialId)}/twin-config`, undefined, controller.signal).then(next => {
      if (controller.signal.aborted) return;
      if (loadedTarget.current !== target) setSettings(JSON.stringify(next.controller.variables, null, 2));
      loadedTarget.current = target; setConfiguration(next);
    }).catch(failure => { if (!controller.signal.aborted) setError(researchError(failure)); }).finally(() => { if (!controller.signal.aborted) setBusy(false); });
    return () => controller.abort();
  }, [runId, trialId, target, revision]);
  useEffect(() => { if (configuration && loadedTarget.current === target) notify.current?.({ equipmentViewId: viewId, settingsVersionId: settingsId, configuration }); }, [configuration, viewId, settingsId, target]);
  const controllerVariables = configuration?.controller.variables;
  const pointChoices = useMemo(() => controllerVariables ? (['cvs', 'mvs', 'dvs'] as const).flatMap(role => controllerVariables[role].map(variable => ({ role: role.slice(0, -1) as 'cv' | 'mv' | 'dv', variable }))) : [], [controllerVariables]);
  const changeView = (next: EquipmentView) => { setDraft({ ...next, confirmed: false }); setViewId(''); setTopologyConfirmed(false); setNotice('草稿已修改，須另存新版本才能供新工作階段使用。'); };
  const changeBinding = (id: string, patch: Partial<EquipmentBinding>) => changeView({ ...draft, bindings: draft.bindings.map(binding => binding.id === id ? { ...binding, ...patch } : binding) });
  async function save(kind: 'equipment' | 'settings') {
    if (!configuration || busy) return;
    setBusy(true); setError(''); setNotice('');
    try {
      if (kind === 'equipment') {
        const view = validateEquipmentView({ ...draft, confirmed: typeConfirmed && topologyConfirmed }, configuration.controller.variables);
        const saved = await researchRequest<EquipmentViewVersion>('/twin/equipment-views', { run_id: runId, trial_id: trialId, view });
        if (currentTarget.current !== target) return;
        setConfiguration(current => current ? { ...current, equipment_views: [...current.equipment_views, saved] } : current);
        setDraft(saved.view); setViewId(saved.id); setNotice(`設備版本已保存：${saved.id}。既有工作階段不受影響。`);
      } else {
        if (!name.trim()) throw new Error('請輸入設定版本名稱。');
        const variables: unknown = JSON.parse(settings);
        const saved = await researchRequest<TwinSettingsVersion>('/twin/settings', { run_id: runId, trial_id: trialId, name: name.trim(), variables });
        if (currentTarget.current !== target) return;
        setConfiguration(current => current ? { ...current, settings_versions: [...current.settings_versions, saved] } : current);
        setSettingsId(saved.id); setNotice(`診斷設定已保存：${saved.id}。不改寫試驗資格或現場設定。`);
      }
    } catch (failure) { if (currentTarget.current === target) setError(`${researchError(failure)} 若儲存回應遺失，請更新版本清單確認，不會自動重送。`); }
    finally { if (currentTarget.current === target) setBusy(false); }
  }
  return <ResearchPanel title="設備與診斷設定版本">
    <div className="min-w-0 space-y-3">
      <p className="text-xs text-amber-200">此處僅修改研究草稿。設備型式與拓樸不自動推定；只有明確綁定的點位可取得值。另存產生不可變新版本，既有 session 永遠使用建立時凍結的版本。</p>
      {!runId || !trialId ? <p>請先選擇有效的凍結試驗；上游工程草稿不需在此假造控制器。</p> : null}
      {error && <ResearchNotice error={error} />}{notice && <p role="status" className="break-all text-xs text-indigo-200">{notice}</p>}
      <button type="button" className={researchButton} disabled={busy || !trialId} onClick={() => setRevision(value => value + 1)}>更新版本清單（保留草稿）</button>
      {configuration && <>
        <p className="break-all text-xs text-slate-400">來源控制器 SHA-256：{configuration.controller_sha256} · Ts {configuration.controller.tuning.sample_time}s · P {configuration.controller.tuning.prediction_horizon} · M {configuration.controller.tuning.control_horizon}</p>
        <div className="grid gap-3 md:grid-cols-2">
          <ResearchField label="新工作階段設備版本"><select className={researchControl} disabled={busy} value={viewId} onChange={event => {
            const id = event.target.value, version = configuration.equipment_views.find(item => item.id === id); setViewId(id);
            if (version) { setDraft(structuredClone(version.view)); setTypeConfirmed(version.view.confirmed); setTopologyConfirmed(version.view.confirmed); setGeometry(''); }
          }}><option value="">尚未選擇已保存版本（新 session 不帶設備）</option>{configuration.equipment_views.map(version => <option key={version.id} value={version.id}>{version.view.label} · {version.view.confirmed ? '已確認' : '草稿'} · {version.id.slice(0, 12)}</option>)}</select></ResearchField>
          <ResearchField label="新工作階段控制設定"><select className={researchControl} disabled={busy} value={settingsId} onChange={event => {
            const id = event.target.value, version = configuration.settings_versions.find(item => item.id === id); setSettingsId(id); setSettings(JSON.stringify((version?.controller ?? configuration.controller).variables, null, 2));
          }}><option value="">原試驗凍結控制器</option>{configuration.settings_versions.map(version => <option key={version.id} value={version.id}>{version.name} · 僅診斷 · {version.id.slice(0, 12)}</option>)}</select></ResearchField>
        </div>
        <DigitalTwinScenePanel key={viewId || 'draft'} view={draft} points={[]} selectedEquipmentId={selectedEquipment} selectedBindingId={selectedBinding} onSelectEquipment={setSelectedEquipment} onSelectBinding={id => { setSelectedBinding(id); setSelectedEquipment(draft.bindings.find(binding => binding.id === id)?.equipment_id ?? null); }} motion="unavailable" source="simulation" visible={sceneVisible} />
        <details className="rounded border border-slate-700 p-3"><summary className="cursor-pointer text-indigo-300">編輯設備、拓樸與精確點位</summary>
          <div className="mt-3 space-y-3">
            <ResearchField label="設備配置名稱"><input className={researchControl} value={draft.label} onChange={event => changeView({ ...draft, label: event.target.value })} /></ResearchField>
            <ResearchField label="明確選擇設備型式"><select className={researchControl} value={draft.layout} onChange={event => { const layout = event.target.value as EquipmentLayout; changeView(unboundEquipmentTemplate(layout)); setTypeConfirmed(false); setGeometry(''); }}><option value="closed_ball_mill">閉路球磨（僅範本，需確認）</option><option value="vertical_mill">立磨（僅範本，需確認）</option></select></ResearchField>
            <p className="text-xs text-slate-400">更換型式建立無連線、無綁定範本；不沿用猜測的拓樸。座標以公尺、旋轉以弧度表示。</p>
            <ResearchField label="設備與連線 JSON（完整 schema，明確 from／to／kind／binding_id）"><textarea className={`${researchControl} min-h-40 font-mono`} value={geometry || JSON.stringify({ equipment: draft.equipment, connections: draft.connections }, null, 2)} onChange={event => setGeometry(event.target.value)} /></ResearchField>
            <button type="button" className={researchButton} onClick={() => { try { const value = JSON.parse(geometry || JSON.stringify(draft)); const next = validateEquipmentView({ ...draft, equipment: value.equipment, connections: value.connections, confirmed: false }, configuration.controller.variables); changeView(next); setGeometry(''); setError(''); } catch (failure) { setError(researchError(failure)); } }}>檢查並套用拓樸草稿</button>
            <div className="space-y-3">{draft.bindings.map(binding => <fieldset key={binding.id} className="min-w-0 space-y-2 rounded border border-slate-700 p-2"><legend className="break-all text-xs">{binding.id}</legend>
              <div className="grid gap-2 sm:grid-cols-2">
                <ResearchField label="綁定設備"><select className={researchControl} value={binding.equipment_id} onChange={event => changeBinding(binding.id, { equipment_id: event.target.value })}>{draft.equipment.map(item => <option key={item.id} value={item.id}>{item.name}</option>)}</select></ResearchField>
                <ResearchField label="精確角色／變數"><select className={researchControl} value={`${binding.role}:${binding.variable_name}`} onChange={event => {
                  if (event.target.value.startsWith('telemetry:')) { changeBinding(binding.id, { role: 'telemetry', variable_name: '', unit: '', channel: 'actual', node_id: undefined, animation: 'none', reference_value: undefined }); return; }
                  const choice = pointChoices.find(item => `${item.role}:${item.variable.name}` === event.target.value);
                  if (choice) changeBinding(binding.id, { role: choice.role, variable_name: choice.variable.name, unit: choice.variable.units, channel: 'actual', node_id: undefined, animation: 'none', reference_value: undefined });
                }}><option value={binding.role === 'telemetry' ? `telemetry:${binding.variable_name}` : 'telemetry:'}>獨立遙測（必須明確指定 Node ID）</option>{pointChoices.map(({ role, variable }) => <option key={`${role}:${variable.name}`} value={`${role}:${variable.name}`}>{role.toUpperCase()} · {variable.name} · {variable.units}</option>)}</select></ResearchField>
                <ResearchField label="資料通道"><select className={researchControl} value={binding.channel} onChange={event => changeBinding(binding.id, { channel: event.target.value as EquipmentBinding['channel'], node_id: binding.role === 'telemetry' ? binding.node_id : undefined, animation: 'none' })}>{(binding.role === 'cv' ? ['actual', 'target'] : binding.role === 'mv' ? ['actual', 'command', 'target', 'op', 'mode'] : binding.role === 'telemetry' ? ['actual', 'op', 'mode', 'running', 'fault'] : ['actual']).map(channel => <option key={channel} value={channel}>{({ actual: '實際／應用值', target: '目標', command: '命令 SP', op: 'PID OP（獨立單位）', mode: '模式', running: '運轉指示（非轉速）', fault: '故障指示' } as Record<string, string>)[channel]}</option>)}</select></ResearchField>
                <ResearchField label="精確工程單位"><input className={researchControl} value={binding.unit} onChange={event => changeBinding(binding.id, { unit: event.target.value })} /></ResearchField>
                {binding.role === 'telemetry' && <><ResearchField label="獨立遙測名稱"><input className={researchControl} value={binding.variable_name} onChange={event => changeBinding(binding.id, { variable_name: event.target.value })} /></ResearchField><ResearchField label="精確遙測 Node ID"><input className={researchControl} value={binding.node_id ?? ''} onChange={event => changeBinding(binding.id, { node_id: event.target.value })} /></ResearchField></>}
                <ResearchField label="動畫用途"><select className={researchControl} value={binding.animation} onChange={event => changeBinding(binding.id, { animation: event.target.value as EquipmentBinding['animation'] })}><option value="none">無動畫</option><option value="rotation">旋轉示意（實際／OP）</option><option value="flow">流量示意（實際／OP）</option></select></ResearchField>
                <ResearchField label="動畫參考量（正值，非 RPM 換算）"><input className={researchControl} type="number" step="any" value={binding.reference_value ?? ''} onChange={event => changeBinding(binding.id, { reference_value: event.target.value === '' ? undefined : Number(event.target.value) })} /></ResearchField>
              </div>
              <p className="break-all text-xs text-slate-400">精確 Node ID：{equipmentNode(binding, configuration.controller.variables) ?? '未配置；不猜 Tag'} · 模擬沒有 OP／mode 遙測時保持缺值。</p>
              <button type="button" className={researchButton} onClick={() => changeView({ ...draft, bindings: draft.bindings.filter(item => item.id !== binding.id), connections: draft.connections.map(link => link.binding_id === binding.id ? { id: link.id, from: link.from, to: link.to, kind: link.kind } : link) })}>移除此綁定</button>
            </fieldset>)}</div>
            <button type="button" className={researchButton} disabled={!draft.equipment.length} onClick={() => changeView({ ...draft, bindings: [...draft.bindings, { id: `point-${crypto.randomUUID()}`, equipment_id: selectedEquipment ?? draft.equipment[0].id, role: 'telemetry', variable_name: '', unit: '', channel: 'actual', animation: 'none' }] })}>新增綁定（必須明確選變數）</button>
            <details><summary className="cursor-pointer text-sm">完整配置匯入（含獨立遙測 Node ID）</summary><input aria-label="匯入設備配置 JSON" className={researchControl} type="file" accept=".json,application/json" onChange={event => { const file = event.target.files?.[0]; if (!file) return; if (file.size > 512000) { setError('配置檔超過 500 KB。'); return; } void file.text().then(text => { if (currentTarget.current !== target) return; const value = JSON.parse(text); changeView(validateEquipmentView(value.view ?? value, configuration.controller.variables)); setTypeConfirmed(false); setGeometry(''); }).catch(failure => { if (currentTarget.current === target) setError(researchError(failure)); }); }} /></details>
            <label className="flex gap-2 text-sm"><input type="checkbox" checked={typeConfirmed} onChange={event => { setTypeConfirmed(event.target.checked); setViewId(''); }} />我已確認此設備型式，不以範本推定實際工廠。</label>
            <label className="flex gap-2 text-sm"><input type="checkbox" checked={topologyConfirmed} onChange={event => { setTopologyConfirmed(event.target.checked); setViewId(''); }} />我已核對設備實例、連線方向、精確角色／通道／單位與點位綁定。</label>
            <button type="button" className={researchPrimary} disabled={busy || !!geometry} onClick={() => void save('equipment')}>{typeConfirmed && topologyConfirmed ? '另存已確認設備版本' : '另存未確認設備草稿版本'}</button>
            {viewId && <button type="button" className={researchButton} onClick={() => { const version = configuration.equipment_views.find(item => item.id === viewId); if (version) downloadEquipmentView(version); }}>匯出所選設備版本</button>}
          </div>
        </details>
        <details className="rounded border border-slate-700 p-3"><summary className="cursor-pointer text-indigo-300">編輯診斷控制設定（不改原試驗）</summary><div className="mt-3 space-y-3">
          <p className="text-xs text-amber-200">僅 CV 限制／目標／權重／alpha／slack／積分／最佳化方式、MV 限制／權重／每掃描 max_move／目標可改。Tag、變數身分、DV、Ts、P／M 與物理模型保持凍結；伺服器重新驗證完整契約。硬限修改僅供隔離診斷，絕不覆寫 Runtime。</p>
          <ResearchField label="診斷設定版本名稱"><input className={researchControl} value={name} onChange={event => setName(event.target.value)} /></ResearchField>
          <ResearchField label="完整 cvs／mvs／dvs 設定 JSON"><textarea className={`${researchControl} min-h-64 font-mono`} value={settings} onChange={event => { setSettings(event.target.value); setSettingsId(''); }} /></ResearchField>
          <button type="button" className={researchPrimary} disabled={busy || !name.trim()} onClick={() => void save('settings')}>另存診斷設定新版本</button>
        </div></details>
      </>}
    </div>
  </ResearchPanel>;
}
