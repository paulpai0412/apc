import { Component, useCallback, useEffect, useId, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import type { ReactNode } from 'react';
import { Canvas } from '@react-three/fiber';
import type { EquipmentMotion, EquipmentPoint, EquipmentView } from '../types/EquipmentView';
import { SceneContents } from './twinScene/SceneContents';
import type { CameraAction, CameraCommands } from './twinScene/SceneContents';
import { resolveSceneMotion } from './twinScene/sceneMotion';
import './twinScene/DigitalTwinScene.css';

export type DigitalTwinSceneProps = {
  view: EquipmentView;
  points: EquipmentPoint[];
  selectedEquipmentId: string | null;
  selectedBindingId: string | null;
  onSelectEquipment: (id: string) => void;
  onSelectBinding: (id: string) => void;
  motion: EquipmentMotion;
  source: 'simulation' | 'archive' | 'live';
  visible?: boolean;
};

const numberFormat = new Intl.NumberFormat(undefined, { maximumSignificantDigits: 6 });
const kinds: Record<string, true> = { silo: true, feeder: true, conveyor: true, ball_mill: true, vertical_mill: true, elevator: true, separator: true, collector: true, fan: true };
const sourceNames = { simulation: '閉環模擬', archive: '已記錄封存', live: '現場唯讀觀察' };
const motionNames = { running: '執行中', paused: '已暫停', stale: '資料過期', unavailable: '尚無資料' };
const qualityNames = { good: '良好', bad: '不良', uncertain: '不確定', unknown: '未知', missing: '缺值' };
const initialCamera = { position: [30, 20, 30] as [number, number, number], fov: 45, near: 0.01, far: 10000 };
const rendererOptions = { antialias: true, alpha: false, powerPreference: 'high-performance' as const };
const pixelRatio: [number, number] = [1, 1.5];
const subscribeVisibility = (notify: () => void) => {
  document.addEventListener('visibilitychange', notify);
  return () => document.removeEventListener('visibilitychange', notify);
};
const getVisible = () => document.visibilityState === 'visible';
const subscribeReducedMotion = (notify: () => void) => {
  const query = window.matchMedia('(prefers-reduced-motion: reduce)');
  query.addEventListener('change', notify);
  return () => query.removeEventListener('change', notify);
};
const getReducedMotion = () => window.matchMedia('(prefers-reduced-motion: reduce)').matches;
const serverVisible = () => false;
const serverReducedMotion = () => true;

class WebGLBoundary extends Component<{ children: ReactNode; onFailure: (message: string) => void }, { failed: boolean }> {
  state = { failed: false };
  static getDerivedStateFromError() { return { failed: true }; }
  componentDidCatch(error: Error) {
    this.props.onFailure(`3D 繪圖失敗：${error.message}。仍可使用點位表；此狀態不算 3D 驗收成功。`);
  }
  render() { return this.state.failed ? null : this.props.children; }
}

/** Presentation only. The owning board supplies a coherent, already accepted source/frame. */
export default function DigitalTwinScene({ view, points, selectedEquipmentId, selectedBindingId, onSelectEquipment, onSelectBinding, motion, source, visible: expanded = true }: DigitalTwinSceneProps) {
  const documentVisible = useSyncExternalStore(subscribeVisibility, getVisible, serverVisible);
  const visible = expanded && documentVisible;
  const reducedMotion = useSyncExternalStore(subscribeReducedMotion, getReducedMotion, serverReducedMotion);
  const [animationEnabled, setAnimationEnabled] = useState(true);
  const [particles, setParticles] = useState(true);
  const [showLabels, setShowLabels] = useState(true);
  const [failure, setFailure] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const labels = useRef(new Map<string, HTMLButtonElement>());
  const commands = useRef<CameraCommands | null>(null);
  const helpId = useId();
  const equipmentSelectId = useId();
  const onFailure = useCallback((message: string) => setFailure(message), []);
  const configError = useMemo(() => {
    if (view.schema_version !== 1) return '不支援的設備配置版本。';
    if (view.equipment.length === 0) return '此配置尚無設備。';
    if (view.equipment.length > 128 || view.connections.length > 256 || view.bindings.length > 512) return '超過場景容量（128 設備、256 連線、512 綁定）。';
    const ids = new Set<string>();
    for (const item of view.equipment) {
      if (!item.id || ids.has(item.id) || kinds[item.kind] !== true) return '設備 ID 必須唯一，型式必須受支援。';
      ids.add(item.id);
      if (item.position.length !== 3 || item.rotation.length !== 3 || item.size.length !== 3 || ![...item.position, ...item.rotation, ...item.size].every(Number.isFinite)) return '設備轉換須為三個有限座標。';
      if (item.size.some(value => value <= 0 || value > 1000) || item.position.some(value => Math.abs(value) > 10000)) return '設備尺寸或位置超出繪圖安全範圍。';
    }
    const linkIds = new Set<string>();
    for (const link of view.connections) {
      if (linkIds.has(link.id) || !ids.has(link.from) || !ids.has(link.to) || link.from === link.to || (link.kind !== 'air' && link.kind !== 'material')) return '設備連線無效或重複。';
      linkIds.add(link.id);
    }
    return null;
  }, [view]);
  const pointMap = useMemo(() => new Map(points.map(point => [point.bindingId, point])), [points]);
  const bindingsByEquipment = useMemo(() => {
    const result = new Map<string, EquipmentView['bindings']>();
    for (const binding of view.bindings) {
      const bindings = result.get(binding.equipment_id) ?? [];
      bindings.push(binding);
      result.set(binding.equipment_id, bindings);
    }
    return result;
  }, [view]);
  const selectedId = selectedEquipmentId ?? view.bindings.find(binding => binding.id === selectedBindingId)?.equipment_id ?? null;
  const selectedItem = view.equipment.find(item => item.id === selectedId);
  const selectedBindings = selectedId ? bindingsByEquipment.get(selectedId) ?? [] : [];
  const animated = visible && !reducedMotion && animationEnabled && motion === 'running';
  const sceneMotion = useMemo(() => resolveSceneMotion(view, points, animated), [view, points, animated]);
  const hasAnimation = sceneMotion.rotation.size > 0 || (particles && sceneMotion.flow.size > 0);
  useEffect(() => { commands.current?.execute('redraw', null); }, [showLabels, visible]);
  const cameraAction = (action: CameraAction) => commands.current?.execute(action, selectedId);
  const error = configError ?? failure;
  return <section className="twin-scene" aria-label="3D 設備看板">
    <header className="twin-scene-header">
      <div><strong>{view.label}</strong><span>{sourceNames[source]} · {motionNames[motion]}{!expanded ? ' · 3D 已收合（相機保留）' : ''}</span></div>
      <span className="twin-scene-status">{view.confirmed ? '已確認設備配置' : '未確認拓樸 · 未綁定範本不是工廠模型'}</span>
    </header>
    <div hidden={!expanded}>
    <div className="twin-scene-toolbar" role="group" aria-label="3D 相機控制">
      <button type="button" onClick={() => cameraAction('home')} disabled={!!error}>重設／全廠</button>
      <button type="button" onClick={() => cameraAction('focus')} disabled={!!error || !selectedItem}>聚焦所選設備</button>
      <button type="button" onClick={() => cameraAction('left')} disabled={!!error} aria-label="向左環繞">↶</button>
      <button type="button" onClick={() => cameraAction('right')} disabled={!!error} aria-label="向右環繞">↷</button>
      <button type="button" onClick={() => cameraAction('up')} disabled={!!error}>抬高視角</button>
      <button type="button" onClick={() => cameraAction('down')} disabled={!!error}>降低視角</button>
      <button type="button" onClick={() => cameraAction('zoom-in')} disabled={!!error} aria-label="放大">+</button>
      <button type="button" onClick={() => cameraAction('zoom-out')} disabled={!!error} aria-label="縮小">−</button>
      <label><input type="checkbox" checked={showLabels} onChange={event => setShowLabels(event.target.checked)} /> 點位標籤</label>
      <label><input type="checkbox" checked={animationEnabled && !reducedMotion} disabled={reducedMotion} onChange={event => setAnimationEnabled(event.target.checked)} /> 設備動畫</label>
      <label><input type="checkbox" checked={particles} onChange={event => setParticles(event.target.checked)} /> 流向粒子</label>
    </div>
    <p id={helpId} className="twin-scene-help">拖曳環繞、右鍵拖曳平移、滾輪／雙指縮放。鍵盤：方向鍵環繞、Shift＋方向鍵平移、＋／− 縮放、Home 重設、F 聚焦。選取不寫入設備。</p>
    <div className="twin-scene-viewport" tabIndex={0} role="group" aria-label="互動 3D 設備視窗" aria-describedby={helpId} onKeyDown={event => {
      if (event.target !== event.currentTarget) return;
      const keys: Record<string, CameraAction> = { ArrowLeft: event.shiftKey ? 'pan-left' : 'left', ArrowRight: event.shiftKey ? 'pan-right' : 'right', ArrowUp: event.shiftKey ? 'pan-up' : 'up', ArrowDown: event.shiftKey ? 'pan-down' : 'down', '+': 'zoom-in', '=': 'zoom-in', '-': 'zoom-out', Home: 'home', f: 'focus', F: 'focus' };
      const action = keys[event.key];
      if (action) { event.preventDefault(); cameraAction(action); }
    }}>
      {error ? <div className="twin-scene-error" role="alert"><strong>3D 無法使用</strong><p>{error}</p>{!configError && <button type="button" onClick={() => { setFailure(null); setAttempt(value => value + 1); }}>重試 WebGL</button>}<p>下方設備與點位表仍可使用；此替代畫面不算通過 3D 驗收。</p></div> : <WebGLBoundary key={attempt} onFailure={onFailure}>
        <Canvas dpr={pixelRatio} frameloop={!visible ? 'never' : hasAnimation ? 'always' : 'demand'} camera={initialCamera} gl={rendererOptions} fallback={<div className="twin-scene-error" role="alert"><strong>3D 無法使用：不支援 WebGL。</strong><p>未以替代繪圖冒充 3D 結果，請使用下方點位表。</p></div>}>
          <SceneContents view={view} selectedEquipmentId={selectedId} onSelectEquipment={onSelectEquipment} motion={sceneMotion} particles={particles} labels={labels} commandsRef={commands} onFailure={onFailure} />
        </Canvas>
      </WebGLBoundary>}
      {!error && showLabels && <div className="twin-scene-labels" aria-hidden="true">
        {view.equipment.map(item => {
          const bindings = bindingsByEquipment.get(item.id) ?? [];
          const binding = bindings.find(value => value.id === selectedBindingId) ?? bindings[0];
          const point = binding ? pointMap.get(binding.id) : undefined;
          return <button key={item.id} type="button" tabIndex={-1} className={`twin-scene-label${selectedId === item.id ? ' is-selected' : ''}`} ref={element => { if (element) labels.current.set(item.id, element); else labels.current.delete(item.id); }} onClick={() => onSelectEquipment(item.id)}>
            <strong>{item.name}</strong>
            {binding ? <><span>{binding.role.toUpperCase()} · {binding.variable_name}</span><span>{point?.value !== null && point?.value !== undefined && Number.isFinite(point.value) ? numberFormat.format(point.value) : '—'} {binding.unit}</span><small>{qualityNames[point?.quality ?? 'missing']}{point?.stale ? ' · 過期' : ''} · {binding.channel}</small></> : <small>未綁定／未建模 · 靜止</small>}
            {bindings.length > 1 && <small>另有 {bindings.length - 1} 點位</small>}
          </button>;
        })}
      </div>}
      <div className="twin-scene-legend">物料：砂色路徑 · 空氣：藍色路徑 · 箭頭：已配置方向<br />{reducedMotion ? '減少動態效果：設備動畫已停用' : !animationEnabled ? '設備動畫已停用' : motion !== 'running' ? `動畫停止：${motionNames[motion]}` : !view.confirmed ? '未確認配置：動畫停用' : '只有品質良好、新鮮且精確綁定的實際值／OP 可驅動動畫'}</div>
    </div>
    </div>
    <div className="twin-scene-selection">
      <label htmlFor={equipmentSelectId}>設備（支援鍵盤）</label>
      <select id={equipmentSelectId} value={selectedId ?? ''} onChange={event => { if (event.target.value) onSelectEquipment(event.target.value); }}>
        <option value="" disabled>選擇設備</option>
        {view.equipment.map(item => <option key={item.id} value={item.id}>{item.name} · {item.kind.replaceAll('_', ' ')}</option>)}
      </select>
      <span>{view.equipment.length} 設備 · {view.connections.length} 連線</span>
    </div>
    <div className="twin-scene-points" aria-label="所選設備點位">
      <h4>{selectedItem ? `${selectedItem.name} — 綁定點位` : '選擇設備以檢查點位'}</h4>
      {selectedItem && selectedBindings.length === 0 && <p>未綁定／未建模；不推測量測值、運轉狀態或動畫。</p>}
      {selectedBindings.map(binding => {
        const point = pointMap.get(binding.id);
        const value = point?.value;
        return <button type="button" key={binding.id} className={`twin-scene-point${selectedBindingId === binding.id ? ' is-selected' : ''}`} aria-pressed={selectedBindingId === binding.id} onClick={() => onSelectBinding(binding.id)}>
          <strong>{binding.role.toUpperCase()} · {binding.variable_name} <small>({binding.channel})</small></strong>
          <span className="twin-scene-value">{value !== null && value !== undefined && Number.isFinite(value) ? numberFormat.format(value) : '—'} {binding.unit}</span>
          <span>{qualityNames[point?.quality ?? 'missing']}{point?.stale ? ' · 過期／最後已知值' : ''}</span>
          <small>{point?.sourceLabel ?? '尚無來源樣本'} · 來源時刻：{point?.time === null || point?.time === undefined ? '未知' : point.time}</small>
          {binding.animation !== 'none' && <small>{binding.animation === 'rotation' ? '旋轉' : '流動'}示意 · 參考量 {binding.reference_value ?? '未設定'} {binding.unit} · 受品質、來源與執行狀態限制</small>}
        </button>;
      })}
    </div>
    <p className="twin-scene-help">幾何僅為設備示意，不是製程物理。動畫使用數值／參考量比例，旋轉限制 ±2 rad/s、流動限制 ±0.24 路徑/s，不能當作實際轉速或輸送時間。未綁定、無效或過期輸入保持未知／靜止；多個旋轉來源不明確時保持靜止。不內插或製造量測值。</p>
  </section>;
}
