import type { EquipmentBinding, EquipmentPoint, EquipmentView, EquipmentViewVersion, TwinControllerVariables } from '../types/EquipmentView';
import type { TwinFrame } from '../types/DigitalTwin';
import type { Campaign } from '../types/Research';
import type { OpcUpdate } from '../store/tagStore';
import { twinQuality } from './digitalTwin';

const roles = ['cv', 'mv', 'dv'] as const;
export function equipmentNode(binding: EquipmentBinding, variables: TwinControllerVariables): string | undefined {
  if (binding.role === 'telemetry') return binding.node_id;
  const variable = variables[`${binding.role}s`].find(item => item.name === binding.variable_name);
  if (!variable) return undefined;
  const ids = variable.node_ids;
  if (!ids) return undefined;
  const node = binding.channel === 'actual' ? (binding.role === 'mv' ? ids.applied_sp ?? ids.sp : ids.pv)
    : binding.channel === 'command' ? ids.sp : ids[binding.channel];
  return typeof node === 'string' && (!binding.node_id || binding.node_id === node) ? node : undefined;
}

export function equipmentBindingError(binding: EquipmentBinding, variables: TwinControllerVariables): string | null {
  if (!binding.variable_name.trim()) return '點位名稱不可空白。';
  if (binding.role === 'telemetry') {
    if (!['actual', 'op', 'mode', 'running', 'fault'].includes(binding.channel)) return '獨立遙測不支援此通道。';
    if (['running', 'fault'].includes(binding.channel) && !['bool', 'boolean'].includes(binding.unit)) return '運轉／故障通道須使用 bool 或 boolean 單位。';
    if (binding.channel === 'mode' && binding.unit !== 'mode') return '模式通道須使用 mode 單位。';
    return binding.node_id?.trim() && !/:\/\/|[/\\]|\.\./.test(binding.node_id) ? null : '設備遙測須明確指定 Node ID，不接受網址或檔案路徑。';
  }
  const matches = variables[`${binding.role}s`].filter(item => item.name === binding.variable_name);
  const variable = matches[0];
  if (matches.length !== 1) return '變數必須在此凍結控制器中唯一存在。';
  const allowed = binding.role === 'cv' ? ['actual', 'target'] : binding.role === 'mv' ? ['actual', 'command', 'target', 'op', 'mode'] : ['actual'];
  if (!allowed.includes(binding.channel)) return '此角色沒有指定的資料通道。';
  if (['actual', 'target', 'command'].includes(binding.channel) && binding.unit !== variable.units) return '工程單位必須與控制器完全一致。';
  if (binding.channel === 'mode' && binding.unit !== 'mode') return '模式點位的單位必須是 mode。';
  if (binding.channel === 'op' && !binding.unit.trim()) return 'OP 必須明確指定獨立工程單位。';
  if (binding.node_id && !equipmentNode(binding, variables)) return 'Node ID 不符合此變數的精確映射。';
  return null;
}

export function validateEquipmentView(input: unknown, variables: TwinControllerVariables): EquipmentView {
  if (!input || typeof input !== 'object' || Array.isArray(input)) throw new Error('設備配置必須是 JSON 物件。');
  const view = input as EquipmentView;
  if (view.schema_version !== 1 || !['closed_ball_mill', 'vertical_mill'].includes(view.layout)
    || typeof view.label !== 'string' || !view.label.trim() || typeof view.confirmed !== 'boolean'
    || !Array.isArray(view.equipment) || !view.equipment.length || view.equipment.length > 128
    || !Array.isArray(view.connections) || view.connections.length > 256
    || !Array.isArray(view.bindings) || view.bindings.length > 512) throw new Error('設備配置版本、型式或數量不合法。');
  const ids = new Set<string>();
  for (const item of view.equipment) {
    if (!item || typeof item.id !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_-]{0,95}$/.test(item.id) || ids.has(item.id)
      || typeof item.name !== 'string' || !item.name.trim()
      || !['silo', 'feeder', 'conveyor', 'ball_mill', 'vertical_mill', 'elevator', 'separator', 'collector', 'fan'].includes(item.kind)) throw new Error('設備 ID、名稱或型式不合法。');
    ids.add(item.id);
    for (const key of ['position', 'size', 'rotation'] as const) if (!Array.isArray(item[key]) || item[key].length !== 3
      || item[key].some(value => !Number.isFinite(value) || Math.abs(value) > (key === 'size' ? 100 : key === 'rotation' ? Math.PI * 2 : 1000) || (key === 'size' && value < .01))) throw new Error('設備座標、尺寸與旋轉必須是有界的三維數值。');
  }
  const bindings = new Set<string>();
  for (const binding of view.bindings) {
    if (!binding || typeof binding.id !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_-]{0,95}$/.test(binding.id) || ids.has(binding.id)
      || !view.equipment.some(item => item.id === binding.equipment_id) || ![...roles, 'telemetry'].includes(binding.role)
      || typeof binding.variable_name !== 'string' || typeof binding.unit !== 'string'
      || !['actual', 'target', 'command', 'op', 'mode', 'running', 'fault'].includes(binding.channel)
      || !['none', 'rotation', 'flow'].includes(binding.animation)
      || (binding.node_id !== undefined && typeof binding.node_id !== 'string')) throw new Error('點位綁定格式不合法。');
    const issue = equipmentBindingError(binding, variables);
    if (issue) throw new Error(`${binding.variable_name}：${issue}`);
    if (binding.reference_value !== undefined && !(typeof binding.reference_value === 'number' && Number.isFinite(binding.reference_value) && binding.reference_value > 0)) throw new Error('動畫參考量必須為正有限數。');
    if (binding.animation !== 'none' && (!['actual', 'op'].includes(binding.channel) || binding.reference_value === undefined)) throw new Error('動畫僅接受明確實際值／OP 通道與正參考量。');
    bindings.add(binding.id); ids.add(binding.id);
  }
  for (const link of view.connections) {
    if (!link || typeof link.id !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_-]{0,95}$/.test(link.id) || ids.has(link.id) || !view.equipment.some(item => item.id === link.from) || !view.equipment.some(item => item.id === link.to) || link.from === link.to
      || !['material', 'air'].includes(link.kind) || (link.binding_id !== undefined && !bindings.has(link.binding_id))) throw new Error('設備連線或流量綁定不合法。');
    ids.add(link.id);
  }
  return view;
}

export function simulatedEquipmentPoints(view: EquipmentView, variables: Campaign['variables'], frame: TwinFrame | null | undefined,
  originTime: number, stale: boolean, source: 'simulation' | 'archive'): EquipmentPoint[] {
  return view.bindings.map(binding => {
    let value: number | null = null;
    let quality: EquipmentPoint['quality'] = 'missing';
    if (binding.role !== 'telemetry' && frame) {
      const index = variables[binding.role].findIndex(variable => variable.name === binding.variable_name && variable.unit === binding.unit);
      const offset = binding.role === 'cv' ? 0 : variables.cv.length + (binding.role === 'dv' ? variables.mv.length : 0);
      const values = binding.channel === 'actual' ? frame[binding.role] : binding.channel === 'target' && binding.role === 'cv' ? frame.targets : binding.channel === 'command' && binding.role === 'mv' ? frame.command_mv : undefined;
      if (index >= 0 && Number.isFinite(values?.[index])) { value = values![index]; quality = twinQuality(frame.quality[offset + index]); }
    }
    return { bindingId: binding.id, equipmentId: binding.equipment_id, role: binding.role, name: binding.variable_name, unit: binding.unit, channel: binding.channel,
      value, quality, time: frame && Number.isFinite(originTime) ? originTime + frame.time_seconds * 1000 : null,
      sourceLabel: value === null ? '未建模／未取得' : `${source === 'archive' ? '封存模擬' : '模擬'}${binding.channel === 'command' ? '命令' : binding.channel === 'target' ? '目標' : '實際值'}`,
      stale, animation: binding.animation, referenceValue: binding.reference_value ?? null };
  });
}

export function liveEquipmentPoints(view: EquipmentView, variables: TwinControllerVariables, tags: Record<string, OpcUpdate>, now: number, maxAgeMs: number, connected: boolean): EquipmentPoint[] {
  return view.bindings.map(binding => {
    const issue = equipmentBindingError(binding, variables);
    const node = !issue ? equipmentNode(binding, variables) : undefined, tag = node ? tags[node] : undefined;
    const time = tag ? Date.parse(tag.timestamp) : NaN;
    const stale = !connected || !Number.isFinite(now) || !Number.isFinite(maxAgeMs) || maxAgeMs <= 0 || !Number.isFinite(time) || time > now + 1000 || now - time > maxAgeMs;
    const value = typeof tag?.value === 'number' && Number.isFinite(tag.value) ? tag.value : typeof tag?.value === 'boolean' ? Number(tag.value) : null;
    const quality: EquipmentPoint['quality'] = !tag || value === null ? 'missing' : !Number.isInteger(tag.status) || tag.status < 0 || tag.status > 0xffffffff ? 'unknown'
      : (tag.status >>> 30) === 0 ? 'good' : (tag.status >>> 30) === 1 ? 'uncertain' : 'bad';
    const register = binding.role === 'mv' && binding.channel === 'actual' && !variables.mvs.find(item => item.name === binding.variable_name)?.node_ids?.applied_sp;
    return { bindingId: binding.id, equipmentId: binding.equipment_id, role: binding.role, name: binding.variable_name, unit: binding.unit, channel: binding.channel, value, quality,
      time: Number.isFinite(time) ? time : null, sourceLabel: register ? '現場 SP 暫存器回讀（非獨立應用證據）· 原始來源時間' : '現場 OPC · 原始來源時間', stale,
      animation: register ? 'none' : binding.animation, referenceValue: binding.reference_value ?? null };
  });
}

export function downloadEquipmentView(version: EquipmentViewVersion): void {
  const url = URL.createObjectURL(new Blob([JSON.stringify(version, null, 2)], { type: 'application/json' }));
  const anchor = document.createElement('a'); anchor.href = url; anchor.download = `equipment-${version.id}.json`; anchor.click();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}
