/// <reference types="node" />
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { appendRuntimeSamples, runtimeHistoryData, runtimeTrendData } from '../src/lib/runtimeView.ts';
import type { HistoryPoint } from '../src/lib/historianTrends.ts';
import type { DeploymentProfile, RuntimeProfileSnapshot, RuntimeSignal } from '../src/types/RuntimeWorkspace.ts';

function fixture() {
  const issued = Date.parse('2026-01-01T00:00:00Z'), now = issued + 30_000;
  const profile: DeploymentProfile = {
    profile_hash: 'profile-a', target: { plant: 'test', line: '1', area: 'cement', group: 'mill' },
    equipment_instance_ids: ['mill'], engine_id: 'mill', model_sha256: 'commissioned-model',
    endpoint: 'opc.tcp://127.0.0.1:4840/', model_file: 'mill.json', equipment_view: null,
    model_binding: { schema_version: 1, controller_name: 'mill', sample_time: 60, prediction_horizon: 3, control_horizon: 2,
      cvs: [{ name: 'Quality', description: '', units: 'cm2/g', node_ids: { pv: 'q.pv', target: 'q.target', prediction: 'q.pred', limits: { low: 'q.low', high: 'q.high' } } }],
      mvs: [{ name: 'Feed', description: '', units: 't/h', node_ids: { pv: 'f.pv', sp: 'f.sp', op: 'f.op', mode: 'f.mode', mode_target: 'f.request', applied_sp: 'f.applied', future_plan: 'f.plan', limits: { low: 'f.low', high: 'f.high' } } }], dvs: [] },
    forecast_tag: 'mill:LiveForecast', observation_tag: 'mill:ControlObservation',
    variables: [
      { id: 'Quality', equipment_instance_id: 'mill', role: 'CV', unit: 'cm2/g', pv_tag: 'q.pv', target_tag: 'q.target', low_tag: 'q.low', high_tag: 'q.high', hard_low: 2000, hard_high: 5000,
        command_tag: null, applied_tag: null, op_tag: null, mode_tag: null, mode_target_tag: null, max_move: null, command_coordinate: null, feedback_coordinate: null, remote_mode: null },
      { id: 'Feed', equipment_instance_id: 'mill', role: 'MV', unit: 't/h', pv_tag: 'f.pv', target_tag: null, low_tag: 'f.low', high_tag: 'f.high', hard_low: 0, hard_high: 200,
        command_tag: 'f.sp', applied_tag: 'f.applied', op_tag: 'f.op', mode_tag: 'f.mode', mode_target_tag: 'f.request', max_move: 5, command_coordinate: 'pid_sp', feedback_coordinate: 'pid_sp', remote_mode: 3 },
    ], approved_by: 'test-engineer', commissioning_evidence: 'synthetic test', operating_envelope: 'test', commissioning_kind: 'synthetic_local', application_tolerance: 0.001,
    maximum_age_ms: 2000, maximum_skew_ms: 500, acquisition_sequence_node: 'mill:AcquisitionSequence', history_capacity_samples: 100,
    handoff: { release_tag: 'release', release_value: 1, acknowledgement_tag: 'ack', acknowledgement_value: 1, timeout_ms: 5000, watchdog_evidence: 'test' },
  };
  const forecast = { schema_version: 1, source: 'engine_solver', time_basis: 'utc', issued_at: new Date(issued).toISOString(),
    engine_id: 'mill', instance_id: 'instance-current', controller_sha256: 'candidate-model', sequence: 7,
    sample_time: 60, prediction_horizon: 4, control_horizon: 2, cv_count: 1, mv_count: 1,
    layout: 'step-major', cv_first_offset_steps: 1, mv_first_offset_steps: 0, cv: [3200, 3210, 3220, 3230], mv_base: [100], mv_delta: [2, -3] };
  const snapshot: RuntimeProfileSnapshot = { profile_hash: profile.profile_hash, target: profile.target, equipment_instance_ids: ['mill'], epoch: 4,
    version: 'candidate-model', health: 'good', blockers: [], owner_group: 'mill', instance_id: 'instance-current', authority: 'monitor', arming: false, dcs_handoff: true,
    registry_epoch: 1, active_model_binding: { ...profile.model_binding, prediction_horizon: 4 },
    native_status: { healthy: true, acquired_at: new Date(now - 500).toISOString(), observed_at: forecast.issued_at, maximum_solve_age_ms: 75000, forecast_sequence: 7 },
    signals: [{ node_id: profile.forecast_tag, value: JSON.stringify(forecast), timestamp: forecast.issued_at, status: 0, received_at: new Date(now).toISOString() }] };
  const history = new Map<string, HistoryPoint[]>([['q.pv', [{ node_id: 'q.pv', timestamp: new Date(now).toISOString(), value: 3195, status: 0 }]]]);
  return { profile, snapshot, history, issued, now, forecast };
}

test('native solve cadence is separate from acquisition age and uses the active model horizon', () => {
  const { profile, snapshot, history, issued, now } = fixture();
  const data = runtimeTrendData(profile, snapshot, history, now, true);
  const forecast = data.series.find(series => series.kind === 'prediction')!;
  assert.equal(forecast.available, true);
  assert.deepEqual(forecast.points.map(point => [point.time - issued, point.value]), [[60000, 3200], [120000, 3210], [180000, 3220], [240000, 3230]]);
  const plan = data.series.find(series => series.kind === 'plan')!;
  assert.deepEqual(plan.points.map(point => [point.time - issued, point.value]), [[0, 102], [60000, 99]]);
  assert.equal(data.currentTime, now);
});

test('display permits only the commissioned source clock skew without renewing the solve timestamp', () => {
  const item = fixture();
  item.now = item.issued - item.profile.maximum_skew_ms;
  item.snapshot.native_status!.acquired_at = item.forecast.issued_at;
  const data = runtimeTrendData(item.profile, item.snapshot, item.history, item.now, true);
  assert.equal(data.series.find(series => series.kind === 'prediction')!.available, true);
  assert.equal(data.series.find(series => series.kind === 'prediction')!.points[0].issuedAt, item.issued);
  item.now -= 1;
  assert.equal(runtimeTrendData(item.profile, item.snapshot, item.history, item.now, true)
    .series.find(series => series.kind === 'prediction')!.available, false);
});

test('a forecast cannot survive a stale acquisition, cutover, mismatched native receipt or missing gate', () => {
  const cases: ((item: ReturnType<typeof fixture>) => void)[] = [
    item => { item.snapshot.native_status!.healthy = false; },
    item => { item.snapshot.native_status!.acquired_at = new Date(item.now - 2001).toISOString(); },
    item => { delete item.snapshot.native_status!.maximum_solve_age_ms; },
    item => { item.snapshot.native_status!.observed_at = new Date(item.issued + 1).toISOString(); },
    item => { item.snapshot.native_status!.forecast_sequence = 8; },
    item => { item.snapshot.instance_id = 'replacement-instance'; },
    item => { item.snapshot.version = 'replacement-model'; },
    item => { item.snapshot.active_model_binding!.prediction_horizon = 3; },
    item => { item.now = item.issued + 75001; item.snapshot.native_status!.acquired_at = new Date(item.now).toISOString(); },
  ];
  for (const change of cases) {
    const item = fixture(); change(item);
    const data = runtimeTrendData(item.profile, item.snapshot, item.history, item.now, true);
    assert.ok(data.series.filter(series => series.forecastScope === 'latest').every(series => !series.available && series.points.length === 0));
    assert.equal(data.series.find(series => series.id === 'q.pv')!.points[0].value, 3195);
  }
});

test('recorded settings retain their real sampled tag and time without remapping PV or inventing history forecasts', () => {
  const { profile, issued } = fixture();
  const data = runtimeHistoryData(profile, { rawSamples: 3, gapThresholdMs: 2000, warnings: [], points: [
    { node_id: 'sampled.q.target', timestamp: new Date(issued + 1000).toISOString(), value: 3300, status: 0 },
    { node_id: 'q.pv', timestamp: new Date(issued + 1200).toISOString(), value: 3290, status: 0 },
    { node_id: 'not-a-pv', timestamp: new Date(issued + 1200).toISOString(), value: 99, status: 0 },
  ] }, { start: issued, end: issued + 2000 }, { 'q.target': 'sampled.q.target', 'q.pv': 'not-a-pv' });
  assert.deepEqual(data.series.find(series => series.kind === 'target')!.points.map(point => [point.time, point.value]), [[issued + 1000, 3300]]);
  assert.equal(data.series.find(series => series.kind === 'target')!.id, 'sampled.q.target');
  assert.equal(data.series.find(series => series.id === 'q.pv')!.points[0].value, 3290);
  assert.equal(data.series.some(series => series.kind === 'prediction' || series.kind === 'plan'), false);
});

test('same-instant conflicting observations stay invalid until a genuinely new source timestamp', () => {
  const signal = (timestamp: string, value: number): RuntimeSignal => ({ node_id: 'q.pv', timestamp, value, status: 0, received_at: '2026-01-01T00:05:00Z' });
  const first = signal('2026-01-01T00:00:00Z', 3200);
  let history = appendRuntimeSamples(new Map(), [first], 2);
  history = appendRuntimeSamples(history, [signal('2026-01-01T00:00:00.000Z', 3200)], 2);
  assert.equal(history.get('q.pv')!.length, 1);
  history = appendRuntimeSamples(history, [signal(first.timestamp, 3300)], 2);
  assert.deepEqual(history.get('q.pv')!.map(point => [point.value, point.status]), [[null, 0x80000000]]);
  history = appendRuntimeSamples(history, [first], 2);
  assert.equal(history.get('q.pv')![0].value, null);
  history = appendRuntimeSamples(history, [signal('2026-01-01T00:00:01Z', 3210)], 2);
  assert.deepEqual(history.get('q.pv')!.map(point => point.value), [null, 3210]);
  history = appendRuntimeSamples(history, [signal('2025-12-31T23:59:59Z', 1)], 2);
  assert.deepEqual(history.get('q.pv')!.map(point => point.value), [null, 3210]);
  history = appendRuntimeSamples(history, [signal('2026-01-01T00:00:02Z', 3220)], 1);
  assert.deepEqual(history.get('q.pv')!.map(point => point.value), [3220]);
});
