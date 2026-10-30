/// <reference types="node" />
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { runtimeReadinessAllows } from '../src/lib/runtimeWorkspace.ts';

test('cold Monitor recovery does not require a ready solve or permit Arm and Engage', () => {
  assert.equal(runtimeReadinessAllows('monitor', false, false), true);
  assert.equal(runtimeReadinessAllows('arm', false, false), false);
  assert.equal(runtimeReadinessAllows('engage', false, false), false);
  assert.equal(runtimeReadinessAllows('arm', true, false), true);
  assert.equal(runtimeReadinessAllows('engage', true, false), true);
});

test('Monitor remains blocked by unresolved operations while explicit release remains available', () => {
  assert.equal(runtimeReadinessAllows('monitor', false, true), false);
  assert.equal(runtimeReadinessAllows('monitor', true, true), false);
  assert.equal(runtimeReadinessAllows('exit', false, true), true);
  assert.equal(runtimeReadinessAllows('recover_monitor', false, true), true);
  assert.equal(runtimeReadinessAllows('cv_target', true, true), false);
});

test('cold starts remain separate from ready control and cannot bypass unresolved intent', () => {
  assert.equal(runtimeReadinessAllows('start', false, false), true);
  assert.equal(runtimeReadinessAllows('start_candidate', false, false), true);
  assert.equal(runtimeReadinessAllows('start', false, true), false);
  assert.equal(runtimeReadinessAllows('start_candidate', true, true), false);
  assert.equal(runtimeReadinessAllows('operating_high', false, false), false);
});
