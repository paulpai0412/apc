/// <reference types="node" />
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { decodeModelBinding, runtimeVariables, runtimeNodeId, forecastPoints, identificationInput } from '../src/lib/modelBinding.ts';
import { historianTrendData } from '../src/lib/historianTrends.ts';
import type { HistoryField } from '../src/lib/historianTrends.ts';
import type { ModelBinding } from '../src/types/ModelBinding.ts';

const fixture = (): ModelBinding => ({ schema_version: 1, controller_name: 'cement-Line-A', sample_time: 7.5, prediction_horizon: 3, control_horizon: 2,
  cvs: [{ name: 'Quality', description: 'Fineness', units: 'cm²/g', node_ids: { pv: 'Quality:pv', target: 'q.SetPOINT', prediction: 'forecast-Q', limits: { low: 'q.lo', high: 'q.HI' } } }],
  mvs: [{ name: 'Feed', description: 'Fresh feed', units: 't/h', node_ids: { pv: 'Feed:pv', sp: 'Feed:sp', op: 'Drive:Output', mode: 'PID.actual', mode_target: 'PID.request', future_plan: 'next-feed', applied_sp: 'accepted-feed', limits: { low: 'feed.lo', high: 'feed.hi' } } }], dvs: [] });

test('explicit roles and exact IDs survive mixed case and arbitrary physical naming', () => {
  const binding = decodeModelBinding(fixture());
  assert.equal(binding.controller_name, 'cement-Line-A');
  const rows = runtimeVariables(binding);
  assert.deepEqual(rows.map(row => [row.role, row.name, row.units]), [['CV', 'Quality', 'cm²/g'], ['MV', 'Feed', 't/h']]);
  assert.equal(runtimeNodeId(rows[0], 'PV'), 'Quality:pv');
  assert.equal(runtimeNodeId(rows[0], 'Target'), 'q.SetPOINT');
  assert.equal(runtimeNodeId(rows[1], 'SP'), 'Feed:sp');
  assert.equal(runtimeNodeId(rows[1], 'ModeTarget'), 'PID.request');
  assert.equal(runtimeNodeId(rows[1], 'AppliedSP'), 'accepted-feed');
  assert.equal(runtimeNodeId(rows[1], 'Target'), undefined);
  assert.equal(runtimeNodeId(rows[1], 'Alpha'), undefined);
  assert.equal(runtimeNodeId(rows[1], 'DeltaMin'), undefined);
  assert.equal(runtimeNodeId(rows[0], 'Weight'), 'Quality:Weight');
  assert.equal(runtimeNodeId(rows[0], 'Mode'), undefined);
});

test('missing, invalid, ambiguous mappings and sampling fail closed', () => {
  for (const value of [undefined, null, {}, { ...fixture(), schema_version: 2 }, { ...fixture(), sample_time: 0 }, { ...fixture(), sample_time: NaN }, { ...fixture(), prediction_horizon: 1.5 }]) {
    assert.throws(() => decodeModelBinding(value));
  }
  const missing = fixture(); delete (missing.cvs[0].node_ids as Partial<typeof missing.cvs[0]['node_ids']>).target;
  assert.throws(() => decodeModelBinding(missing));
  const collision = fixture(); collision.mvs[0].node_ids.sp = collision.cvs[0].node_ids.pv;
  assert.throws(() => decodeModelBinding(collision));
  const reserved = fixture(); reserved.cvs[0].node_ids.target = 'cement-Line-A:AcquisitionSequence';
  assert.throws(() => decodeModelBinding(reserved));
  const emptyId = fixture(); emptyId.mvs[0].node_ids.mode_target = ' ';
  assert.throws(() => decodeModelBinding(emptyId));
  const optionalNull = fixture();
  assert.throws(() => decodeModelBinding({ ...optionalNull, mvs: [{ ...optionalNull.mvs[0], node_ids: { ...optionalNull.mvs[0].node_ids, applied_sp: null } }] }));
  assert.throws(() => decodeModelBinding({ ...fixture(), dvs: undefined }));
});

test('empty DVs are valid and model dimension changes do not retain previous roles', () => {
  const initial = decodeModelBinding(fixture());
  const changed = fixture(); changed.cvs.push({ ...changed.cvs[0], name: 'Second', node_ids: { pv: 'two.pv', target: 'two.target', prediction: 'two.pred', limits: { low: 'two.low', high: 'two.high' } } });
  assert.equal(runtimeVariables(initial).length, 2);
  assert.equal(runtimeVariables(decodeModelBinding(changed)).length, 3);
  assert.equal(initial.dvs.length, 0);
  changed.dvs.push({ name: 'Hardness', description: 'Feed hardness', units: 'kWh/t', node_ids: { pv: 'Feed:Hardness.Value', limits: { low: 'hard.validLow', high: 'hard.validHigh' } } });
  const dv = runtimeVariables(decodeModelBinding(changed))[3];
  assert.equal(dv.role, 'DV');
  assert.equal(runtimeNodeId(dv, 'PV'), 'Feed:Hardness.Value');
  assert.equal(runtimeNodeId(dv, 'Prediction'), undefined);
  assert.equal(runtimeNodeId(dv, 'Weight'), undefined);
});

test('forecasts use real sample time and reject unavailable sampling instead of defaulting to twenty', () => {
  assert.deepEqual(forecastPoints([10, 12, 14], 7.5), [{ seconds: 7.5, value: 10 }, { seconds: 15, value: 12 }, { seconds: 22.5, value: 14 }]);
  assert.deepEqual(forecastPoints([100, 101], 7.5, 0), [{ seconds: 0, value: 100 }, { seconds: 7.5, value: 101 }]);
  for (const period of [undefined, 0, -1, NaN]) assert.deepEqual(forecastPoints([10, 12], period), []);
  assert.deepEqual(forecastPoints(undefined, 7.5), []);
  assert.deepEqual(forecastPoints([10, NaN], 7.5), [{ seconds: 7.5, value: 10 }, { seconds: 15, value: null }]);
});

test('historian selectors pair exact mapped nodes and keep OP separate from PV engineering units', () => {
  const rows = runtimeVariables(decodeModelBinding(fixture()));
  const fields: HistoryField[] = rows.flatMap(row => ['PV', 'Target', 'SP', 'OP', 'AppliedSP', 'LowLimit', 'HighLimit'].flatMap(field => {
    const key = runtimeNodeId(row, field);
    return key ? [{ key, base: row.name, field, role: row.role, alias: `${row.name} ${field}`, unit: field === 'OP' ? '' : row.units, enabled: true, pinned: false }] : [];
  }));
  fields.push({ key: 'derived-quality-error', base: 'Quality', field: 'Error', role: 'CV', alias: 'error', unit: 'cm²/g', enabled: true, pinned: false });
  const values: Record<string, number> = { 'Quality:pv': 3000, 'q.SetPOINT': 2900, 'Feed:pv': 100, 'Feed:sp': 101, 'Drive:Output': 45, 'accepted-feed': 99, 'q.lo': 2800, 'q.HI': 3200, 'feed.lo': 80, 'feed.hi': 120 };
  const chart = historianTrendData({ points: Object.entries(values).map(([node_id, value]) => ({ timestamp: '2026-01-01T00:00:00Z', node_id, value, status: 0 })), rawSamples: 10, gapThresholdMs: 10000, warnings: [] }, fields, { start: Date.parse('2026-01-01T00:00:00Z'), end: Date.parse('2026-01-01T00:00:01Z') });
  assert.equal(chart.series.find(series => series.id === 'Quality:pv')?.points[0].value, 3000);
  assert.equal(chart.series.find(series => series.id === 'derived-quality-error')?.points[0].value, 100);
  assert.equal(chart.series.find(series => series.id === 'accepted-feed')?.points[0].value, 99);
  const pv = chart.series.find(series => series.id === 'Feed:pv')!;
  const op = chart.series.find(series => series.id === 'Drive:Output')!;
  assert.notEqual(op.variableId, pv.variableId);
  assert.equal(chart.variables.find(variable => variable.id === op.variableId)?.unit, '');
  assert.equal(chart.variables.find(variable => variable.id === pv.variableId)?.unit, 't/h');
});

test('identification uses applied feedback or explicitly weaker SP feedback, never PID OP', () => {
  const binding = fixture();
  const variables = runtimeVariables(decodeModelBinding(binding));
  assert.equal(identificationInput(variables[0]), undefined);
  assert.deepEqual(identificationInput(variables[1]), { nodeId: 'accepted-feed', feedback: 'applied-sp' });
  delete binding.mvs[0].node_ids.applied_sp;
  assert.deepEqual(identificationInput(runtimeVariables(decodeModelBinding(binding))[1]), { nodeId: 'Feed:sp', feedback: 'sp-register' });
});
