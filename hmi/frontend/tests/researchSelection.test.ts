/// <reference types="node" />
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { researchSelectionIssue } from '../src/lib/researchSelection.ts';
import type { ResearchRun } from '../src/types/Research.ts';

function developmentRun(): ResearchRun {
  return { id: 'run', study_id: 'study', name: 'study', status: 'running', stage: 'awaiting_selection', created_at: '2026-01-01T00:00:00Z', progress: { completed: 3, total: 5 }, checks: [], trials: [
    { id: 'baseline', method: 'dmc', round: 0, parameters: {}, valid: true, score: 5 },
    { id: 'candidate', method: 'dmc', round: 1, parameters: {}, valid: true, eligible: true, score: 3 },
    { id: 'rejected', method: 'dmc', round: 1, parameters: {}, valid: true, eligible: false, score: 1 },
  ], development: { objectives: ['quality', 'movement'], vectors: { baseline: [2, 1], candidate: [1, 2], rejected: [0, 0] }, nondominated_trial_ids: ['baseline', 'candidate'], scope: 'development only' } };
}

test('baseline may be retained despite a lower candidate score', () => {
  assert.equal(researchSelectionIssue(developmentRun(), { dmc: 'baseline' }, 'Retain lower movement'), null);
});
test('a low score cannot offset a failed hard gate', () => {
  assert.ok(researchSelectionIssue(developmentRun(), { dmc: 'rejected' }, 'Low score'));
});
test('selection requires actual development evidence and a recorded reason', () => {
  const run = developmentRun();
  assert.ok(researchSelectionIssue(run, { dmc: 'candidate' }, ' '));
  delete run.development;
  assert.ok(researchSelectionIssue(run, { dmc: 'candidate' }, 'Conservative choice'));
});
test('cross-method, missing and extra choices are denied', () => {
  const run = developmentRun();
  assert.ok(researchSelectionIssue(run, {}, 'Reason'));
  assert.ok(researchSelectionIssue(run, { dmc: 'candidate', other: 'baseline' }, 'Reason'));
  assert.ok(researchSelectionIssue(run, { other: 'candidate' }, 'Reason'));
});
test('completed, cancelled, failed, or already opened final results cannot choose a new winner', () => {
  for (const status of ['completed', 'cancelled', 'failed'] as const) {
    const run = developmentRun(); run.status = status;
    assert.ok(researchSelectionIssue(run, { dmc: 'candidate' }, 'Reason'));
  }
  const run = developmentRun();
  run.results = { methods: [], variables: { cv: [], mv: [], dv: [] }, splits: [], identification: {}, scope: 'opened holdout' };
  assert.ok(researchSelectionIssue(run, { dmc: 'candidate' }, 'Reason'));
});
