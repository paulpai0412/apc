/// <reference types="node" />
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { historianTrendData, opcQuality } from '../src/lib/historianTrends.ts';
import { mergeTrendData, parseTrendDomain, trendDomain, trendResidual } from '../src/lib/processTrend.ts';
import { trendRoleBlocks, trendRoleGeometry } from '../src/lib/processTrendLayout.ts';
import type { HistoryEnvelope, HistoryField } from '../src/lib/historianTrends.ts';
import type { ProcessTrendData, TrendSeries, TrendVariable } from '../src/types/ProcessTrend.ts';

const range = { start: 0, end: 60000 };
const trace = (id: string, variableId: string, kind: TrendSeries['kind'], value: number): TrendSeries => ({
  id, variableId, kind, label: id, source: 'recorded', method: 'dmc', interpolation: 'linear', available: true,
  points: [{ time: 1000, value, quality: 'good' }],
});
const field = (base: string, name: string, unit: string, role: HistoryField['role'] = 'MV'): HistoryField => ({
  key: `${base}:${name}`, base, field: name, unit, role, alias: `${base}:${name}`, enabled: true, pinned: false,
});
const history = (rows: [number, string, string, number | null, number | null][]): HistoryEnvelope => ({
  rawSamples: 1000, gapThresholdMs: 3000, warnings: [],
  points: rows.map(([time, tag, name, value, status]) => ({ timestamp: new Date(time).toISOString(), node_id: `${tag}:${name}`, value, status })),
});

test('each variable has an independent domain; methods and bound types keep their own engineering coordinate', () => {
  const series = [trace('dmc:a:cv:0:actual', 'cv:0', 'actual', 10), trace('dmc:b:cv:0:prediction', 'cv:0', 'prediction', 15),
    trace('limit', 'cv:0', 'spec_high', 100), trace('other', 'cv:1', 'actual', 1000)];
  const detail = trendDomain(series, 'cv:0', 'detail', range);
  assert.ok(detail[0] < 10 && detail[1] > 15 && detail[1] < 100);
  assert.ok(trendDomain(series, 'cv:0', 'all', range)[1] > 100);
  const before = trendDomain(series, 'cv:0', 'detail', range);
  series[3].points[0].value = 1e9;
  assert.deepEqual(trendDomain(series, 'cv:0', 'detail', range), before);
});

test('residuals pair the same method and variant at exact forecast valid time', () => {
  const predicted = trace('dmc:transport:cv:0:prediction', 'cv:0', 'prediction', 12);
  predicted.points[0].issuedAt = 0;
  const actual = trace('dmc:transport:cv:0:actual', 'cv:0', 'actual', 14);
  const baseline = trace('dmc:baseline:cv:0:actual', 'cv:0', 'actual', 900);
  assert.equal(trendResidual([baseline, actual, predicted], predicted, predicted.points[0]), 2);
  actual.points[0].time = 999;
  assert.equal(trendResidual([baseline, actual, predicted], predicted, predicted.points[0]), null);
  actual.points[0].time = 1000;
  actual.points[0].quality = 'bad';
  assert.equal(trendResidual([actual, predicted], predicted, predicted.points[0]), null);
});

test('display decimation is not a telemetry gap; recorded missing samples and quality remain intact', () => {
  const reduced = historianTrendData(history([[0, 'C', 'PV', 5, 0], [60000, 'C', 'PV', 7, 0]]), [field('C', 'PV', 'C', 'CV')], range);
  assert.deepEqual(reduced.series[0].points.map(point => point.value), [5, 7]);
  const missing = historianTrendData(history([[0, 'C', 'PV', 5, 0], [3000, 'C', 'PV', null, null], [60000, 'C', 'PV', 7, 0x80000000]]), [field('C', 'PV', 'C', 'CV')], range);
  assert.deepEqual(missing.series[0].points.map(point => point.quality), ['good', 'missing', 'bad']);
  assert.ok(missing.events.some(event => event.kind === 'gap' && event.time === 3000));
  assert.deepEqual([0, 0x40000000, 0x80000000, null].map(opcQuality), ['good', 'uncertain', 'bad', 'unknown']);
});

test('PID output stays on its own axis even when its unit equals PV; actual and command remain paired', () => {
  const fields = [field('M', 'PV', '%'), field('M', 'SP', '%'), field('M', 'OP', '%'), field('M', 'LowLimit', '%')];
  const data = historianTrendData(history(fields.map((item, index) => [0, item.base, item.field, index, 0])), fields, { start: 0, end: 1 });
  assert.equal(data.series[0].variableId, data.series[1].variableId);
  assert.equal(data.series[0].variableId, data.series[3].variableId);
  assert.notEqual(data.series[0].variableId, data.series[2].variableId);
});

test('derived diagnostics never use future samples or subtract incompatible engineering units', () => {
  const fields = [field('C', 'PV', 'C', 'CV'), field('C', 'Target', 'C', 'CV'), field('C', 'Error', 'C', 'CV')];
  const input = history([[1000, 'C', 'PV', 15, 0], [2000, 'C', 'Target', 10, 0], [2000, 'C', 'PV', 17, 0]]);
  const data = historianTrendData(input, fields, { start: 1000, end: 2000 });
  assert.deepEqual(data.series[2].points.map(point => point.value), [null, 7]);
  fields[1].unit = 'bar';
  const mismatch = historianTrendData(input, fields, { start: 1000, end: 2000 });
  assert.equal(mismatch.series[2].available, false);
  assert.ok(mismatch.series[2].points.every(point => point.value === null));
});

test('legacy forecast vectors remain unavailable rather than acquiring fabricated timestamps', () => {
  const data = historianTrendData(history([[0, 'C', 'Prediction', 12, 0]]), [field('C', 'Prediction', 'C', 'CV')], range);
  assert.equal(data.series[0].available, false);
  assert.deepEqual(data.series[0].points, []);
});

test('manual axis bounds reject blank, non-finite, inverted, and degenerate ranges', () => {
  for (const [low, high] of [['', '4'], ['0', 'Infinity'], ['5', '2'], ['2', '2']]) assert.equal(parseTrendDomain(low, high), null);
  assert.deepEqual(parseTrendDomain('-0.2', '0.1'), [-0.2, 0.1]);
});

test('diagnostic residuals and command increments cannot distort absolute engineering axes', () => {
  const actual = trace('actual', 'cv:0', 'actual', 360);
  const domain = trendDomain([actual], 'cv:0', 'detail', range);
  const auxiliary = [trace('error', 'cv:0', 'residual', -10000), trace('delta', 'cv:0', 'movement', 50000)];
  assert.deepEqual(trendDomain([actual, ...auxiliary], 'cv:0', 'detail', range), domain);
  assert.deepEqual(trendDomain([actual, ...auxiliary], 'cv:0', 'all', range), domain);
});

test('comparison never combines statistics from different windows or a missing statistical source', () => {
  const data = (end: number): ProcessTrendData => ({
    ...historianTrendData(history([[1000, 'C', 'PV', 10, 0]]), [field('C', 'PV', 'C', 'CV')], { start: 0, end }),
    windowSummary: { range: { start: 0, end }, basis: 'full-resolution good samples', rows: [] },
  });
  const first = data(10000), second = data(20000);
  assert.equal(mergeTrendData([first, second])?.windowSummary, undefined);
  second.range = first.range;
  assert.equal(mergeTrendData([first, second])?.windowSummary, undefined);
  second.windowSummary = undefined;
  assert.equal(mergeTrendData([first, second])?.windowSummary, undefined);
});

test('role blocks retain CV MV DV order and never present absent DV as zero data', () => {
  const variables: TrendVariable[] = [
    { id: 'mv', role: 'MV', name: 'Feed', unit: 'kg/h' },
    { id: 'cv', role: 'CV', name: 'Temperature', unit: 'C' },
  ];
  const blocks = trendRoleBlocks(variables, ['cv', 'mv'], false);
  assert.deepEqual(blocks.map(block => block.role), ['CV', 'MV', 'DV']);
  assert.deepEqual(blocks.map(block => block.lanes.map(lane => lane.map(variable => variable.id))), [[['cv']], [['mv']], []]);
  assert.equal(blocks[2].configured, 0);
  const hidden = trendRoleBlocks(variables, [], false);
  assert.equal(hidden[0].configured, 1);
  assert.deepEqual(hidden[0].lanes, []);
});

test('role blocks use two independent engineering axes, and sublanes for mobile or larger roles', () => {
  const variables: TrendVariable[] = Array.from({ length: 3 }, (_, index) => ({ id: `cv:${index}`, role: 'CV', name: `CV ${index}`, unit: index ? 'bar' : 'C' }));
  const selected = variables.map(variable => variable.id);
  assert.deepEqual(trendRoleBlocks(variables, selected.slice(0, 2), false)[0].lanes.map(lane => lane.length), [2]);
  assert.deepEqual(trendRoleBlocks(variables, selected, false)[0].lanes.map(lane => lane.length), [1, 1, 1]);
  assert.deepEqual(trendRoleBlocks(variables, selected.slice(0, 2), true)[0].lanes.map(lane => lane.length), [1, 1]);
  assert.equal(trendRoleBlocks(variables, selected, false)[0].lanes[0][0], variables[0]);
});

test('single and dual engineering axes reserve identical physical timestamp coordinates', () => {
  for (const mobile of [false, true]) {
    for (const width of [320, 600, 1000]) {
      const coordinate = (count: number, fraction: number) => {
        const { axisWidth, margin } = trendRoleGeometry(count, mobile);
        const left = margin.left + axisWidth;
        const right = margin.right + (count === 2 ? axisWidth : 0);
        return left + fraction * (width - left - right);
      };
      for (const fraction of [0, 0.25, 0.5, 0.9, 1]) assert.equal(coordinate(1, fraction), coordinate(2, fraction));
    }
  }
});

test('same-variable comparison domains include every trial but exclude other engineering variables', () => {
  const first = { ...trace('trial-a', 'cv:0', 'actual', 10), groupId: 'a' };
  const second = { ...trace('trial-b', 'cv:0', 'actual', 80), groupId: 'b' };
  const other = { ...trace('mv', 'mv:0', 'command', 1e6), groupId: 'b' };
  const shared = trendDomain([first, second, other], 'cv:0', 'detail', range);
  assert.ok(shared[0] < 10 && shared[1] > 80 && shared[1] < 1e6);
  const unlocked = trendDomain([first], 'cv:0', 'detail', range);
  assert.ok(unlocked[1] < 80);
});
