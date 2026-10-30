import type { EquipmentPoint, EquipmentView } from '../../types/EquipmentView';

export type SceneMotion = { rotation: Map<string, number>; flow: Map<string, number> };

/** Visual speeds only; never samples, physical RPM, or inferred equipment running state. */
export function resolveSceneMotion(view: EquipmentView, points: EquipmentPoint[], enabled: boolean): SceneMotion {
  const rotation = new Map<string, number>();
  const flow = new Map<string, number>();
  if (!enabled || !view.confirmed) return { rotation, flow };
  const pointMap = new Map(points.map(point => [point.bindingId, point]));
  const drivers = new Map<string, number>();
  const bindingSpeeds = new Map<string, number>();
  for (const binding of view.bindings) {
    if (binding.animation === 'rotation') drivers.set(binding.equipment_id, (drivers.get(binding.equipment_id) ?? 0) + 1);
    const point = pointMap.get(binding.id);
    if (!point || point.equipmentId !== binding.equipment_id || point.role !== binding.role || point.name !== binding.variable_name || point.unit !== binding.unit || point.channel !== binding.channel) continue;
    if (binding.channel !== 'actual' && binding.channel !== 'op') continue;
    if (point.quality !== 'good' || point.stale || point.value === null || !Number.isFinite(point.value) || point.time === null || !Number.isFinite(point.time)) continue;
    if (point.animation !== binding.animation || point.referenceValue === null || !Number.isFinite(point.referenceValue) || point.referenceValue <= 0 || binding.reference_value !== point.referenceValue) continue;
    bindingSpeeds.set(binding.id, Math.max(-2, Math.min(2, point.value / point.referenceValue)));
  }
  const equipment = new Map(view.equipment.map(item => [item.id, item]));
  const bindings = new Map(view.bindings.map(binding => [binding.id, binding]));
  for (const binding of view.bindings) {
    const item = equipment.get(binding.equipment_id);
    const speed = bindingSpeeds.get(binding.id);
    if (binding.animation === 'rotation' && drivers.get(binding.equipment_id) === 1 && speed && item && item.kind !== 'silo' && item.kind !== 'collector') rotation.set(binding.equipment_id, speed);
  }
  for (const link of view.connections) {
    if (!link.binding_id) continue;
    const binding = bindings.get(link.binding_id);
    const speed = bindingSpeeds.get(link.binding_id);
    if (binding?.animation === 'flow' && speed) flow.set(link.id, speed);
  }
  return { rotation, flow };
}
