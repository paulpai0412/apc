/// <reference types="node" />
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { resolveSceneMotion } from '../src/components/twinScene/sceneMotion.ts';
import { unboundEquipmentTemplate } from '../src/lib/twinEquipmentTemplate.ts';
import type { EquipmentPoint, EquipmentView } from '../src/types/EquipmentView.ts';

function fixture(): { view: EquipmentView; point: EquipmentPoint } {
  const view = unboundEquipmentTemplate();
  view.confirmed = true;
  view.bindings = [{ id: 'speed', equipment_id: 'mill', role: 'mv', variable_name: 'Mill speed', unit: 'rpm', channel: 'actual', animation: 'rotation', reference_value: 100 }];
  return { view, point: { bindingId: 'speed', equipmentId: 'mill', role: 'mv', name: 'Mill speed', unit: 'rpm', channel: 'actual', value: 50, quality: 'good', time: 1000, sourceLabel: 'native simulation', stale: false, animation: 'rotation', referenceValue: 100 } };
}

test('only a fresh good exact-channel actual bound to a confirmed device drives visual motion', () => {
  const { view, point } = fixture();
  assert.equal(resolveSceneMotion(view, [point], true).rotation.get('mill'), 0.5);
  for (const patch of [
    { stale: true }, { quality: 'unknown' as const }, { quality: 'bad' as const }, { quality: 'uncertain' as const },
    { channel: 'command' as const }, { channel: undefined }, { unit: '%' }, { name: 'Other speed' },
    { equipmentId: 'fan' }, { time: null }, { value: NaN }, { referenceValue: 200 },
  ]) assert.equal(resolveSceneMotion(view, [{ ...point, ...patch }], true).rotation.size, 0);
  assert.equal(resolveSceneMotion(view, [point], false).rotation.size, 0);
  assert.equal(resolveSceneMotion({ ...view, confirmed: false }, [point], true).rotation.size, 0);
});

test('command, running flags, ambiguous rotation drivers and unbound templates remain static', () => {
  const { view, point } = fixture();
  for (const channel of ['command', 'running', 'target', 'mode', 'fault'] as const) {
    const changed = { ...view, bindings: [{ ...view.bindings[0], channel }] };
    assert.equal(resolveSceneMotion(changed, [{ ...point, channel }], true).rotation.size, 0);
  }
  view.bindings.push({ ...view.bindings[0], id: 'second-driver' });
  assert.equal(resolveSceneMotion(view, [point], true).rotation.size, 0);
  const template = unboundEquipmentTemplate('vertical_mill');
  assert.equal(template.confirmed, false);
  assert.equal(template.bindings.length, 0);
  assert.equal(template.connections.length, 0);
  assert.equal(resolveSceneMotion(template, [point], true).rotation.size, 0);
});

test('flow uses only the connection explicit binding and is bounded independently of measurements', () => {
  const { view, point } = fixture();
  view.bindings[0].animation = 'flow';
  view.connections = [{ id: 'material-path', from: 'feed', to: 'mill', kind: 'material', binding_id: 'speed' }, { id: 'unbound-path', from: 'mill', to: 'separator', kind: 'material' }];
  const input = { ...point, animation: 'flow' as const, value: 10000 };
  const motion = resolveSceneMotion(view, [input], true);
  assert.equal(motion.flow.get('material-path'), 2);
  assert.equal(motion.flow.has('unbound-path'), false);
  assert.equal(input.value, 10000);
  assert.equal(resolveSceneMotion(view, [{ ...input, stale: true }], true).flow.size, 0);
});
