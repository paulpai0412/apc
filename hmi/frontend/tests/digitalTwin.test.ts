/// <reference types="node" />
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { acceptsTwinSnapshot, archivedTwinTrend, recentTwinTrend, twinQuality } from '../src/lib/digitalTwin.ts';
import { trendDomain } from '../src/lib/processTrend.ts';
import { trendRoleBlocks, trendRoleGeometry } from '../src/lib/processTrendLayout.ts';
import type { TwinManifest, TwinSession } from '../src/types/DigitalTwin.ts';

function fixture(): { manifest: TwinManifest; session: TwinSession } {
  const variables = {
    cv: [{ name: 'Fineness', unit: 'm2/kg', low: 200, high: 500 }, { name: 'Pressure', unit: 'bar', low: 0, high: 5 }],
    mv: [{ name: 'Feed', unit: 't/h', low: 0, high: 200 }, { name: 'Speed', unit: 'rpm', low: 0, high: 1500 }],
    dv: [{ name: 'Hardness', unit: '%', low: 0, high: 100 }],
  };
  const frame = { time_seconds: 25, cv: [320, 2], mv: [90, 990], dv: [45], targets: [330, 2.2], command_mv: [101, 1002], predicted_cv: [321, 2.1], quality: [192, 192, 192, 192, 192], solver_status: 'solved' };
  const session: TwinSession = {
    id: 'session-a', run_id: 'run-a', trial_id: 'trial-a', scenario: 'selection', transport: 'direct', status: 'paused',
    samples: 3, requested_samples: 100, sample_time: 10, start: '2026-01-01T00:00:00Z', created_at: '2026-01-01T00:00:00Z',
    time_basis: 'relative', source_offset_seconds: 10000, frame,
    recent_trace: { time_seconds: [15, 25], cv: [[319, 1.9], frame.cv], mv: [[89, 980], frame.mv], dv: [[44], frame.dv], targets: [[330, 2.2], frame.targets], command_mv: [[100, 1000], frame.command_mv], predicted_cv: [[320, 2], frame.predicted_cv], quality: [[0, 192, 192, 192, 192], frame.quality], solver_status: ['solved', 'solved'] },
    controller_sha256: 'controller-a', plant_sha256: 'plant-a', scenario_sha256: 'scenario-a', binary_sha256: 'binary-a',
    latest_forecast: { schema_version: 1, sample: 2, issued_at_seconds: 25, sample_time: 10, cv_count: 2, mv_count: 2, prediction_horizon: 3, control_horizon: 2, layout: 'step-major', cv_first_offset_steps: 1, mv_first_offset_steps: 0, cv: [321, 2.1, 322, 2.2, 323, 2.3], mv_base: [100, 1000], mv_delta: [1, 2, -3, 4] },
  };
  const manifest: TwinManifest = { run_id: 'run-a', scope: 'local', run_status: 'completed', stage: 'completed', variables, sample_time: 10, start: session.start, time_basis: 'relative', plant_source: null, plant_paths: [], qualification: null, identification: null, trials: [{ id: 'trial-a', method: 'dmc', round: 0, parameters: {}, score: 0, valid: true, controller_sha256: 'controller-a' }], selected_trials: {}, sessions: [session] };
  return { manifest, session };
}

test('latest native horizon preserves issuance and step-major offsets; source-window offset never shifts it', () => {
  const { manifest, session } = fixture();
  const data = recentTwinTrend(manifest, session)!;
  const cv = data.series.find(series => series.variableId === 'cv:1' && series.forecastScope === 'latest')!;
  assert.deepEqual(cv.points.map(({ time, value, issuedAt }) => [time, value, issuedAt]), [[35000, 2.1, 25000], [45000, 2.2, 25000], [55000, 2.3, 25000]]);
  assert.equal(data.currentTime, 25000);
  assert.equal(data.range.end, 55000);
  assert.equal(data.series.filter(series => series.forecastScope === 'latest').length, 4);
  assert.ok(!data.series.some(series => series.variableId === 'dv:0' && series.forecastScope === 'latest'));
  const historic = data.series.find(series => series.variableId === 'cv:0' && series.forecastScope === 'historical')!;
  assert.equal(historic.leadSeconds, 10);
  assert.deepEqual(historic.points.map(point => [point.time, point.issuedAt]), [[25000, 15000], [35000, 25000]]);
});

test('MV plans are cumulative commands from native base, never applied actuals', () => {
  const { manifest, session } = fixture();
  const data = recentTwinTrend(manifest, session)!;
  const plans = data.series.filter(series => series.kind === 'plan');
  assert.deepEqual(plans.map(series => series.points.map(point => point.value)), [[101, 98], [1002, 1006]]);
  assert.deepEqual(plans[0].points.map(point => point.time), [25000, 35000]);
  assert.deepEqual(data.series.find(series => series.variableId === 'mv:0' && series.kind === 'actual')!.points.map(point => point.value), [89, 90]);
  assert.notEqual(plans[0].id, data.series.find(series => series.variableId === 'mv:0' && series.kind === 'command')!.id);
});

test('frozen session limits supersede study limits without rewriting recorded target or bad quality', () => {
  const { manifest, session } = fixture();
  session.variables = structuredClone(manifest.variables);
  session.variables.cv[0] = { ...session.variables.cv[0], low: 280, high: 350, spec_low: 300, spec_high: 340, target: 999 };
  const data = recentTwinTrend(manifest, session)!;
  assert.ok(data.series.find(series => series.variableId === 'cv:0' && series.kind === 'operating_low')!.points.every(point => point.value === 280));
  assert.ok(data.series.find(series => series.variableId === 'cv:0' && series.kind === 'spec_high')!.points.every(point => point.value === 340));
  assert.ok(data.series.find(series => series.variableId === 'cv:0' && series.kind === 'target')!.points.every(point => point.value === 330));
  assert.equal(data.series.find(series => series.variableId === 'cv:0' && series.kind === 'actual')!.points[0].quality, 'bad');
  assert.equal(manifest.variables.cv[0].low, 200);
});

test('malformed, stale, mis-timed or wrong-model envelopes fail closed while actuals remain visible', () => {
  const mutations: ((session: TwinSession) => void)[] = [
    session => { session.latest_forecast!.cv.pop(); },
    session => { session.latest_forecast!.mv_delta.push(3); },
    session => { session.latest_forecast!.mv_base[0] = NaN; },
    session => { session.latest_forecast!.cv[0] = Infinity; },
    session => { session.latest_forecast!.sample = 1; },
    session => { session.latest_forecast!.issued_at_seconds = 24; },
    session => { session.latest_forecast!.sample_time = 5; },
    session => { session.latest_forecast!.cv_count = 1; },
    session => { session.latest_forecast!.control_horizon = 0; },
    session => { session.latest_forecast!.prediction_horizon = 2.5; },
    session => { session.latest_forecast!.mv_first_offset_steps = 1 as 0; },
    session => { session.latest_forecast!.cv_first_offset_steps = 0 as 1; },
    session => { session.latest_forecast!.layout = 'variable-major' as 'step-major'; },
    session => { session.controller_sha256 = 'wrong-controller'; },
    session => { session.run_id = 'wrong-run'; },
    session => { session.trial_id = 'wrong-trial'; },
    session => { session.frame!.time_seconds = 30; },
    session => { session.recent_trace!.time_seconds[1] = 30; },
    session => { session.latest_forecast!.mv_base[0] = Number.MAX_VALUE; session.latest_forecast!.mv_delta[0] = Number.MAX_VALUE; },
  ];
  for (const mutate of mutations) {
    const { manifest, session } = fixture();
    mutate(session);
    const data = recentTwinTrend(manifest, session)!;
    assert.equal(data.series.filter(series => series.forecastScope === 'latest').length, 0);
    assert.ok(data.series.some(series => series.kind === 'actual' && series.available));
    assert.equal(data.warnings.length, 2);
  }
});

test('server-resolved settings may change controller digest, but never physical variable identity', () => {
  const { manifest, session } = fixture();
  session.settings_version_id = 'frozen-settings';
  session.controller_sha256 = 'tuned-controller';
  session.variables = structuredClone(manifest.variables);
  assert.equal(recentTwinTrend(manifest, session)!.series.filter(series => series.forecastScope === 'latest').length, 4);
  session.variables.cv[0].name = 'Unrelated variable';
  assert.ok(!recentTwinTrend(manifest, session)!.series.some(series => series.forecastScope === 'latest'));
});

test('UTC origin is applied exactly once, and stable series IDs survive next completed frames', () => {
  const { manifest, session } = fixture();
  session.time_basis = 'utc';
  const first = recentTwinTrend(manifest, session)!;
  const origin = Date.parse(session.start);
  assert.equal(first.currentTime, origin + 25000);
  assert.equal(first.series.find(series => series.forecastScope === 'latest')!.points[0].time, origin + 35000);
  session.samples += 1;
  session.frame!.time_seconds = 35;
  session.recent_trace!.time_seconds[1] = 35;
  session.latest_forecast!.sample += 1;
  session.latest_forecast!.issued_at_seconds = 35;
  const second = recentTwinTrend(manifest, session)!;
  assert.deepEqual(first.series.map(series => series.id), second.series.map(series => series.id));
  assert.equal(second.currentTime, origin + 35000);
  session.latest_forecast = null;
  assert.ok(!recentTwinTrend(manifest, session)!.series.some(series => series.forecastScope === 'latest'));
});

test('forecast scales remain independent across variables and role lanes share timestamp pixels', () => {
  const { manifest, session } = fixture();
  const data = recentTwinTrend(manifest, session)!;
  const domain = trendDomain(data.series, 'cv:1', 'detail', data.range);
  assert.ok(domain[0] > 0 && domain[1] < 5);
  const selected = data.variables.map(variable => variable.id);
  assert.deepEqual(trendRoleBlocks(data.variables, selected, false).map(block => block.lanes.map(lane => lane.length)), [[2], [2], [1]]);
  assert.deepEqual(trendRoleBlocks(data.variables, selected, true).map(block => block.lanes.map(lane => lane.length)), [[1, 1], [1, 1], [1]]);
  const one = trendRoleGeometry(1, false), two = trendRoleGeometry(2, false);
  assert.equal(one.margin.right, two.margin.right + two.axisWidth);
  assert.equal(one.margin.left + one.axisWidth, two.margin.left + two.axisWidth);
});

test('snapshot acceptance rejects old sequences, cross-session responses and changed frozen inputs', () => {
  const { session } = fixture();
  session.sequence = 7;
  session.native_sequence = 3;
  assert.equal(acceptsTwinSnapshot(null, session, 'session-a', 'run-a'), true);
  assert.equal(acceptsTwinSnapshot(session, { ...session, sequence: 8 }, 'session-a', 'run-a'), true);
  for (const change of [
    { sequence: 6 }, { sequence: undefined }, { samples: 2 }, { id: 'other-session' },
    { run_id: 'other-run' }, { controller_sha256: 'different-model' },
    { plant_sha256: 'different-plant' }, { scenario_sha256: 'different-scenario' },
    { settings_version_id: 'new-settings' },
  ]) assert.equal(acceptsTwinSnapshot(session, { ...session, ...change }, 'session-a', 'run-a'), false);
});

test('indexed archive preserves the complete native forecast and cumulative plan from the selected scan', () => {
  const { manifest, session } = fixture();
  const archive = {
    sample: 2, total_samples: 129600, frame: session.frame!, latest_forecast: session.latest_forecast!,
    sample_time: session.sample_time, start: session.start, time_basis: session.time_basis,
    source_offset_seconds: 10000, controller_sha256: session.controller_sha256, variables: manifest.variables,
  };
  const data = archivedTwinTrend(manifest, archive, 'archive-a', session.trial_id)!;
  assert.equal(data.currentTime, 25000);
  assert.equal(data.series.find(series => series.kind === 'plan')!.points.at(-1)!.value, 98);
  assert.deepEqual(data.series.find(series => series.variableId === 'cv:0' && series.forecastScope === 'latest')!.points.map(point => [point.time, point.value]), [[35000, 321], [45000, 322], [55000, 323]]);
  assert.ok(data.series.every(series => series.groupId === 'archive-a'));
  assert.equal(archive.total_samples, 129600);
  assert.equal(archive.frame.time_seconds, 25);
});

test('simulation DA quality words never accept invalid numbers or UA status codes as good', () => {
  assert.equal(twinQuality(192), 'good');
  assert.equal(twinQuality(64), 'uncertain');
  assert.equal(twinQuality(0), 'bad');
  for (const invalid of [undefined, -1, 192.5, NaN, Infinity, 0x80000000]) assert.equal(twinQuality(invalid), 'unknown');
});

test('scene snapshots reject a recent-trace tail from another frame', () => {
  const { session } = fixture();
  const changed = structuredClone(session);
  changed.frame!.mv = [12, 990];
  assert.equal(acceptsTwinSnapshot(session, changed, session.id, session.run_id), false);
  const clockChanged = structuredClone(session);
  clockChanged.frame!.time_seconds = 99;
  assert.equal(acceptsTwinSnapshot(null, clockChanged, session.id, session.run_id), false);
});
