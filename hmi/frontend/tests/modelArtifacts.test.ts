/// <reference types="node" />
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { buildModelArtifacts, importControllerModel } from '../src/lib/modelArtifacts.ts';

const fixture = () => ({
  metadata: { name: 'synthetic-cement-2026-q1', version: '1.0', model_type: 'step_response', description: 'A "quoted"\nmodel' },
  tuning: { sample_time: 60, prediction_horizon: 3, control_horizon: 2, solver_tolerance: 0.00001, max_iterations: 300, terminal_weight_factor: 1, future_solver_setting: 7 },
  variables: {
    cvs: [{ name: 'ProductFineness', description: 'Product', units: 'm2/kg', weight: 0.0025, alpha: 0.3, ece_factor: 1, slack_weight: 100, is_integrating: true, optimization_mode: { type: 'Target', value: 360 }, limits: { low: 330, high: 390, low_low: 320, high_high: 400, target: 360 }, node_ids: { pv: 'ProductFineness:pv', target: 'plant/set_point', prediction: 'quality:prediction', limits: { low: 'quality:low', high: 'quality:high', ll: 'quality:ll', hh: 'quality:hh' } } }],
    mvs: [{ name: 'FreshFeed', description: 'Feed', units: 't/h', weight_r: 3, max_move: 1.5, target: 95, target_weight: 0, limits: { low: 80, high: 120, low_low: 70, high_high: 130 }, node_ids: { pv: 'FreshFeed:pv', sp: 'PLC/feed_command', applied_sp: 'PLC/feed_applied', op: 'Feed:OP_percent', mode: 'FreshFeed:mode', mode_target: 'FreshFeed:mode_target', future_plan: 'FreshFeed:future_plan', target: 'economics/feed', limits: { low: 'feed:low', high: 'feed:high' } } }],
    dvs: [],
  },
  physics: { step_coefficients: [[[0, 0.1, 0.2]]], dv_coefficients: [[]] as number[][][] },
});

test('controller import/export preserves exact identity, mappings, physics and solver settings', () => {
  const raw = fixture();
  const artifacts = buildModelArtifacts(importControllerModel(raw));
  const exported = JSON.parse(artifacts.json);
  assert.deepEqual(exported, raw);
  assert.equal(artifacts.binding.controller_name, 'synthetic-cement-2026-q1');
  assert.equal(artifacts.binding.cvs[0].node_ids.pv, 'ProductFineness:pv');
  assert.equal(artifacts.binding.mvs[0].node_ids.applied_sp, 'PLC/feed_applied');
  assert.equal(artifacts.binding.mvs[0].node_ids.target, 'economics/feed');
  const nodes = new Map(artifacts.nodes.map(node => [node.nodeId, node]));
  assert.deepEqual(nodes.get('quality:prediction')?.initialValue, [0, 0, 0]);
  assert.deepEqual(nodes.get('FreshFeed:future_plan')?.initialValue, [0, 0]);
  for (const id of ['FreshFeed:mode', 'FreshFeed:mode_target', 'synthetic-cement-2026-q1:OperatingMode', 'synthetic-cement-2026-q1:AcquisitionSequence']) assert.equal(nodes.get(id)?.initialValue, 0);
  assert.equal(nodes.get('economics/feed')?.initialValue, 95);
  const document = JSON.parse(artifacts.yaml); // JSON flow syntax is valid YAML.
  assert.deepEqual(document.model_binding, artifacts.binding);
  assert.deepEqual(document.nodes, artifacts.nodes);
});

test('label changes never rewrite physical IDs and missing mappings never get defaulted', () => {
  const model = importControllerModel(fixture());
  model.cvs[0].name = 'Renamed quality';
  assert.equal(buildModelArtifacts(model).binding.cvs[0].node_ids.pv, 'ProductFineness:pv');
  delete model.mvs[0].nodeIds.sp;
  assert.throws(() => buildModelArtifacts(model), /FreshFeed.*sp/);
});

test('duplicate physical roles and diagnostic collisions are rejected', () => {
  const model = importControllerModel(fixture());
  model.mvs[0].nodeIds.sp = 'ProductFineness:pv';
  assert.throws(() => buildModelArtifacts(model), /collision/i);
  model.mvs[0].nodeIds.sp = 'FreshFeed:Weight';
  assert.throws(() => buildModelArtifacts(model), /collision/i);
});

test('different CV/MV/DV dimensions and extra configured identifiers survive import and YAML provisioning', () => {
  const raw = fixture();
  const first = raw.variables.cvs[0];
  raw.variables.cvs.push({ ...structuredClone(first), name: 'Pressure', units: 'mbar', node_ids: { pv: 'pressure/pv', target: 'pressure/target', prediction: 'pressure/predicted', limits: { low: 'pressure/low', high: 'pressure/high', ll: 'pressure/ll', hh: 'pressure/hh' } } });
  raw.physics.step_coefficients.push([[0, 0.4, 0.6]]);
  raw.physics.dv_coefficients = [[[0, 1, 2]], [[0, 3, 4]]];
  const input = {
    ...raw,
    variables: {
      ...raw.variables,
      dvs: [{ name: 'FeedMoisture', units: '%', description: 'Moisture', limits: { low: 0.2, high: 2.2 }, node_ids: { pv: 'moisture:pv', sp: 'unused_but_configured:sp', mode_target: 'unused_but_configured:mode_target', limits: { low: 'moisture:low', high: 'moisture:high' } } }],
    },
  };
  const artifacts = buildModelArtifacts(importControllerModel(input));
  assert.deepEqual(JSON.parse(artifacts.json), input);
  assert.deepEqual([artifacts.binding.cvs.length, artifacts.binding.mvs.length, artifacts.binding.dvs.length], [2, 1, 1]);
  assert.equal(artifacts.binding.dvs[0].node_ids.pv, 'moisture:pv');
  assert.equal('sp' in artifacts.binding.dvs[0].node_ids, false);
  assert.ok(artifacts.nodes.some(node => node.nodeId === 'unused_but_configured:sp'));
  assert.equal(artifacts.nodes.find(node => node.nodeId === 'unused_but_configured:mode_target')?.initialValue, 0);
  assert.deepEqual(JSON.parse(buildModelArtifacts(importControllerModel(JSON.parse(artifacts.json))).json), input);
});

test('missing and malformed imported IDs are never replaced by conventional defaults', () => {
  const raw = fixture();
  const input = { ...raw, variables: { ...raw.variables, cvs: [{ ...raw.variables.cvs[0], node_ids: {} }] } };
  const model = importControllerModel(input);
  assert.deepEqual(model.cvs[0].nodeIds, {});
  assert.throws(() => buildModelArtifacts(model), /ProductFineness.*limits.low/);
  const malformed = importControllerModel(fixture());
  malformed.cvs[0].nodeIds.pv = ' ';
  assert.throws(() => buildModelArtifacts(malformed), /ProductFineness.*pv/);
});

test('imported applied feedback absence stays explicit', () => {
  const model = importControllerModel(fixture());
  delete model.mvs[0].nodeIds.applied_sp;
  assert.equal(buildModelArtifacts(model).binding.mvs[0].node_ids.applied_sp, undefined);
  model.tuning.predictionHorizon = 7;
  assert.equal(buildModelArtifacts(model).binding.prediction_horizon, 7);
});

test('timing is not guessed for imports and top-level sample time stays authoritative', () => {
  const raw = fixture();
  const withoutSample: Partial<typeof raw.tuning> = { ...raw.tuning };
  delete withoutSample.sample_time;
  const input = { ...raw, sample_time: 17, tuning: withoutSample };
  const artifacts = buildModelArtifacts(importControllerModel(input));
  assert.equal(artifacts.binding.sample_time, 17);
  assert.deepEqual(JSON.parse(artifacts.json), input);
  assert.throws(() => importControllerModel({ ...raw, tuning: withoutSample }), /sample_time/);
});

test('quoted custom physical IDs serialize safely without changing spelling', () => {
  const model = importControllerModel(fixture());
  model.mvs[0].nodeIds.sp = 'line-1/feed "command"\\\\register';
  const artifacts = buildModelArtifacts(model);
  const yamlDocument = JSON.parse(artifacts.yaml);
  assert.equal(yamlDocument.model_binding.mvs[0].node_ids.sp, 'line-1/feed "command"\\\\register');
  assert.ok(yamlDocument.nodes.some((node: { nodeId: string }) => node.nodeId === 'line-1/feed "command"\\\\register'));
  assert.equal(JSON.parse(artifacts.json).variables.mvs[0].node_ids.sp, 'line-1/feed "command"\\\\register');
});
