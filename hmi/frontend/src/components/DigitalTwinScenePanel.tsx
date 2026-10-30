import { lazy, Suspense, useState } from 'react';
import type { DigitalTwinSceneProps } from './DigitalTwinScene';

const Scene = lazy(() => import('./DigitalTwinScene'));

/** Keep this mounted while collapsed: visibility stops rendering, not the camera or job. */
export function DigitalTwinScenePanel(props: DigitalTwinSceneProps) {
  const [opened, setOpened] = useState(props.visible !== false);
  if (!opened && props.visible !== false) setOpened(true);
  if (!opened) return <section aria-label="已收合設備場景" className="space-y-2 rounded border border-slate-700 p-3">
    <p className="text-sm">{props.view.label} · 3D 已收合，尚未配置繪圖資源</p>
    <label className="block text-xs">設備選取<select className="ml-2 max-w-full rounded bg-slate-900 p-2" value={props.selectedEquipmentId ?? ''} onChange={event => props.onSelectEquipment(event.target.value)}><option value="" disabled>選擇設備</option>{props.view.equipment.map(item => <option key={item.id} value={item.id}>{item.name}</option>)}</select></label>
    <div className="flex flex-wrap gap-2">{props.view.bindings.filter(binding => binding.equipment_id === props.selectedEquipmentId).map(binding => <button type="button" className="rounded border border-slate-600 p-2 text-xs" key={binding.id} onClick={() => props.onSelectBinding(binding.id)}>{binding.role.toUpperCase()} · {binding.variable_name} · {binding.channel} [{binding.unit}]</button>)}</div>
  </section>;
  return <Suspense fallback={<p role="status" className="text-sm text-slate-400">載入 3D 設備模組…</p>}><Scene {...props} /></Suspense>;
}
