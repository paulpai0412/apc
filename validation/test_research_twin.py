"""Public twin artifact boundary: immutable inputs, truthful windows, restart and path safety."""
import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from concurrent.futures import ThreadPoolExecutor

from campaign import controller_template

from research import execute


class TwinArtifactsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / 'runs' / 'run-1'
        self.source.mkdir(parents=True)
        self.trial_id = 'standard_dmc-trial-000'
        variables = {
            'cv': [{'name': 'Temperature', 'unit': 'C', 'low': 0, 'high': 100,
                    'target': 50, 'weight': 1, 'alpha': .3, 'slack_weight': 100}],
            'mv': [{'name': 'Fuel', 'unit': '%', 'low': 0, 'high': 100,
                    'weight_r': 1, 'max_move': 5}],
            'dv': [],
        }
        self.run_record = {'id': 'run-1', 'status': 'completed', 'stage': 'completed', 'scope': 'synthetic',
                    'study': {'config': {'start': '2026-01-01T00:00:00Z', 'sample_time': 10,
                                        'plant_source': 'synthetic', 'variables': variables},
                              'research': {'bands': [{'half_width': 2}]}},
                    'trials': [{'id': self.trial_id, 'method': 'standard_dmc', 'valid': True,
                                'eligible': False, 'controller': self.trial_id + '-controller.json',
                                'parameters': {'cv_weight_scale': 9, 'mv_weight_scale': 7, 'alpha': .3}}]}
        self.run_record['study']['config'].update(
            id='twin-fixture', description='Isolated twin artifact fixture',
            controller={'prediction_horizon': 3, 'control_horizon': 1,
                        'solver_tolerance': 1e-6, 'max_iterations': 100,
                        'terminal_weight_factor': 1})
        self.controller = controller_template(self.run_record['study']['config'])
        # Formatting is intentional: copying parsed/re-serialized JSON must fail this boundary.
        self.controller_bytes = json.dumps(self.controller, indent=3).encode() + b'\n\n'
        (self.source / (self.trial_id + '-controller.json')).write_bytes(self.controller_bytes)
        self.save('run.json', self.run_record)
        self.save('plant.json', {'sample_time': 10, 'baseline_cv': [50], 'baseline_mv': [10],
                                'baseline_dv': [], 'paths': [[{'gain': 1, 'tau1': 10, 'tau2': 20, 'delay_seconds': 0}]]})
        self.scenario = {'sample_time': 10, 'time_seconds': [0, 10, 20], 'cv': [[50]] * 3,
                         'mv': [[10]] * 3, 'dv': [[]] * 3, 'targets': [[50]] * 3}
        self.save('scenario.json', self.scenario)
        selection = dict(self.scenario, provenance={'source_first_sample': 100, 'source_last_sample_exclusive': 103})
        self.save('selection-scenario.json', selection)
        self.trace = {'time_seconds': [0, 10, 20], 'cv': [[50], [51], [52]], 'mv': [[10]] * 3,
                      'dv': [[]] * 3, 'targets': [[50]] * 3, 'command_mv': [[10]] * 3,
                      'predicted_cv': [[51], [52], [53]], 'quality': [[192, 192]] * 3}
        self.save(self.trial_id + '.json', {'status': 'valid', 'trace': self.trace})
        # A selected winner's full curve is deliberately different from its trial.
        self.save('standard_dmc-optimized.json', {'trace': dict(self.trace, cv=[[99]] * 3)})
        self.binary = self.root / 'native'
        self.binary.write_bytes(b'boundary executable identity; never executed by these tests')
        self.env = patch.dict(os.environ, {'AUTHRITY_RESEARCH_RUNTIME': json.dumps({
            'local_only': True, 'binary': str(self.binary), 'bridge': str(self.binary), 'bridge_args': []})})
        self.env.start()
        self.addCleanup(self.env.stop)

    def save(self, name, value):
        (self.source / name).write_text(json.dumps(value))

    def create(self, **changes):
        return execute(self.root, 'twin-create', dict(run_id='run-1', trial_id=self.trial_id,
            scenario='full', transport='direct', owner='owner-1', **changes))

    def control(self, session, command, operation_id, **options):
        return execute(self.root, 'twin-control', dict(id=session['id'], owner='owner-1',
            command=command, operation_id=operation_id, **options))

    def reconcile(self, owner='owner-2'):
        return execute(self.root, 'twin-reconcile', {'owner': owner})

    def test_continuous_commands_are_durable_idempotent_and_reads_do_not_advance(self):
        session = self.create()
        started = self.control(session, 'start', 'start-1', batch_steps=128)['session']
        self.assertEqual(started['execution_status'], 'running')
        self.assertEqual(started['sequence'], 1)
        directory = self.root / 'twin-sessions' / session['id']
        before = {p.name: p.read_bytes() for p in directory.iterdir()}
        duplicate = self.control(session, 'start', 'start-1', batch_steps=128)
        self.assertFalse(duplicate['dispatch'])
        self.assertEqual(duplicate['session']['sequence'], 1)
        for owner in ('owner-1', 'other-owner'):
            snapshot = execute(self.root, 'twin-get', {'id': session['id'], 'owner': owner})
            self.assertEqual(snapshot['samples'], 0)
            self.assertEqual(snapshot['execution_status'], 'running')
        self.assertEqual(before, {p.name: p.read_bytes() for p in directory.iterdir()})
        with self.assertRaisesRegex(ValueError, 'different'):
            self.control(session, 'pause', 'start-1')
        rejected = self.control(session, 'step', 'step-while-running')['session']
        self.assertEqual(rejected['operation']['status'], 'rejected')
        paused = self.control(session, 'pause', 'pause-1')['session']
        self.assertEqual(paused['execution_status'], 'paused')
        self.assertEqual(paused['samples'], 0)

    def test_native_intent_is_recorded_before_effect_and_restart_never_replays(self):
        session = self.create()
        accepted = self.control(session, 'step', 'one-scan')
        self.assertTrue(accepted['dispatch'])
        before = accepted['session']
        intent = execute(self.root, 'twin-native-begin', {'id': session['id'], 'owner': 'owner-1',
            'expected_sequence': before['sequence'], 'command': {'command': 'advance', 'expected_samples': 0, 'count': 1},
            'operation_id': 'one-scan'})
        self.assertEqual(intent['native_sequence'], 1)
        self.assertEqual(intent['pending_native']['expected_samples'], 0)
        with self.assertRaisesRegex(ValueError, 'pending'):
            execute(self.root, 'twin-native-begin', {'id': session['id'], 'owner': 'owner-1',
                'expected_sequence': intent['sequence'], 'command': {'command': 'advance', 'expected_samples': 0, 'count': 1},
                'operation_id': 'one-scan'})
        self.reconcile()
        restarted = execute(self.root, 'twin-get', {'id': session['id'], 'owner': 'owner-2'})
        self.assertEqual(restarted['execution_status'], 'interrupted')
        self.assertEqual(restarted['operation']['status'], 'unknown')
        duplicate = execute(self.root, 'twin-control', {'id': session['id'], 'owner': 'owner-2',
            'command': 'step', 'operation_id': 'one-scan'})
        self.assertFalse(duplicate['dispatch'])
        self.assertEqual(duplicate['session']['operation']['status'], 'unknown')
        self.assertEqual(duplicate['session']['native_sequence'], 1)

    def test_interruption_preserves_full_trace_journal_and_known_forecast(self):
        session = self.create()
        execute(self.root, 'twin-update', {'id': session['id'], 'owner': 'owner-1', 'response': self.native_response(0)})
        directory = self.root / 'twin-sessions' / session['id']
        content = b'{"schema_version":1,"first_sample":0,"trace":{"time_seconds":[0]}}\n'
        journal = directory / 'result.json.trace.ndjson'
        journal.write_bytes(content)
        self.reconcile()
        recovered = execute(self.root, 'twin-get', {'id': session['id'], 'owner': 'owner-2'})
        self.assertEqual(recovered['execution_status'], 'interrupted')
        self.assertEqual(recovered['latest_forecast'], self.forecast(0))
        self.assertEqual(recovered['trace_journal_sha256'], hashlib.sha256(content).hexdigest())
        self.assertEqual(recovered['trace_journal_bytes'], len(content))
        self.assertEqual(journal.read_bytes(), content)

    def test_concurrent_duplicate_step_is_dispatched_once(self):
        session = self.create()
        with ThreadPoolExecutor(max_workers=8) as pool:
            replies = list(pool.map(lambda _: self.control(session, 'step', 'same-step'), range(8)))
        self.assertEqual(sum(reply['dispatch'] for reply in replies), 1)
        self.assertTrue(all(reply['session']['operation']['status'] == 'accepted' for reply in replies))
        self.assertTrue(all(reply['session']['sequence'] == 1 for reply in replies))

    def test_pause_cannot_be_acknowledged_until_accepted_native_batch_is_settled(self):
        session = self.create()
        running = self.control(session, 'start', 'start')['session']
        intent = execute(self.root, 'twin-native-begin', {'id': session['id'], 'owner': 'owner-1',
            'expected_sequence': running['sequence'], 'command': {'command': 'advance', 'expected_samples': 0, 'count': 1}})
        rejected = self.control(session, 'pause', 'too-early')['session']
        self.assertEqual(rejected['operation']['status'], 'rejected')
        self.assertEqual(rejected['execution_status'], 'running')
        response = dict(self.native_response(0), request_id=intent['pending_native']['request_id'])
        confirmed = execute(self.root, 'twin-update', {'id': session['id'], 'owner': 'owner-1', 'response': response})
        self.assertEqual(confirmed['execution_status'], 'running')
        self.assertEqual(confirmed['samples'], 1)
        paused = self.control(session, 'pause', 'after-batch')['session']
        self.assertEqual(paused['execution_status'], 'paused')
        self.assertEqual(paused['samples'], 1)
        self.assertEqual(paused['latest_forecast'], self.forecast(0))

    def test_native_identity_and_exact_step_count_are_required_before_acceptance(self):
        session = self.create()
        accepted = self.control(session, 'step', 'step-1')['session']
        intent = execute(self.root, 'twin-native-begin', {'id': session['id'], 'owner': 'owner-1',
            'expected_sequence': accepted['sequence'], 'command': {'command': 'advance', 'expected_samples': 0, 'count': 1},
            'operation_id': 'step-1'})
        for response in (dict(self.native_response(0), request_id='wrong'),
                         dict(self.native_response(1), request_id=intent['pending_native']['request_id'])):
            with self.assertRaises(ValueError):
                execute(self.root, 'twin-update', {'id': session['id'], 'owner': 'owner-1', 'response': response})
        response = dict(self.native_response(0), request_id=intent['pending_native']['request_id'])
        confirmed = execute(self.root, 'twin-update', {'id': session['id'], 'owner': 'owner-1', 'response': response})
        self.assertEqual(confirmed['operation']['status'], 'completed')
        self.assertEqual(confirmed['samples'], 1)
        duplicate = self.control(session, 'step', 'step-1')
        self.assertFalse(duplicate['dispatch'])
        self.assertEqual(duplicate['session']['samples'], 1)

    def test_commands_reject_missing_malformed_identity_and_unbounded_batch(self):
        session = self.create()
        for operation_id in (None, '', '../escape', 'x' * 97, 17):
            with self.subTest(operation_id=operation_id), self.assertRaises(ValueError):
                self.control(session, 'start', operation_id)
        for count in (0, 513, True, 1.5):
            with self.subTest(count=count), self.assertRaises(ValueError):
                self.control(session, 'start', 'start', batch_steps=count)
        with self.assertRaises(ValueError):
            self.create(equipment_view_id=None)
        with self.assertRaises(ValueError):
            self.create(settings_version_id='')

    def test_training_progress_is_visible_before_a_plant_has_qualified(self):
        self.run_record.update(status='running', stage='qualify-history-plant', trials=[])
        self.save('run.json', self.run_record)
        (self.source / 'plant.json').unlink()
        current = execute(self.root, 'twin-manifest', {'id': 'run-1', 'owner': 'owner-1'})
        self.assertEqual(current['run_status'], 'running')
        self.assertEqual(current['stage'], 'qualify-history-plant')
        self.assertEqual(current['plant_paths'], [])
        self.assertIsNone(current['qualification'])
        self.assertEqual(current['trials'], [])
        self.assertEqual(current['selected_trials'], {})

    def test_creation_copies_exact_controller_without_retuning_and_keeps_originals(self):
        before = {p.name: p.read_bytes() for p in self.source.iterdir()}
        session = self.create()
        frozen = self.root / 'twin-sessions' / session['id']
        self.assertEqual((frozen / 'controller.json').read_bytes(), self.controller_bytes)
        self.assertEqual(session['controller_sha256'], hashlib.sha256(self.controller_bytes).hexdigest())
        self.assertTrue(session['diagnostic_only'])
        self.assertTrue(session['exploratory'])
        self.assertEqual(session['status'], 'starting')
        self.assertIsNone(session['frame'])
        self.assertEqual(before, {p.name: p.read_bytes() for p in self.source.iterdir()})
        self.assertNotEqual(self.create()['id'], session['id'])
        self.assertTrue((frozen / 'manifest.json').is_file())

    def test_recorded_trial_is_validation_only_with_real_calendar_offset_and_forecast_lead(self):
        data = execute(self.root, 'twin-trial-series', {'id': 'run-1', 'trial_id': self.trial_id, 'owner': 'owner-1'})
        self.assertEqual(data['originTime'], 1767226600000)
        actual = next(row for row in data['series'] if row['variableId'] == 'cv:0' and row['kind'] == 'actual')
        self.assertEqual([p['value'] for p in actual['points']], [50, 51, 52])
        self.assertEqual(data['overview']['splits'], [])
        prediction = next(row for row in data['series'] if row['variableId'] == 'cv:0' and row['kind'] == 'prediction')
        self.assertEqual(prediction['points'][0]['issuedAt'], data['originTime'])
        self.assertEqual(prediction['points'][0]['time'], data['originTime'] + 10000)

    def test_trial_limits_and_violations_use_frozen_controller_not_study_ranges(self):
        self.controller['variables']['cvs'][0]['limits'].update(low=30, high=51)
        self.controller['variables']['mvs'][0]['limits'].update(low=5, high=15)
        self.save(self.trial_id + '-controller.json', self.controller)
        data = execute(self.root, 'twin-trial-series',
                       {'id': 'run-1', 'trial_id': self.trial_id, 'owner': 'owner-1'})
        expected_bounds = {('cv:0', 'operating_low'): 30, ('cv:0', 'operating_high'): 51,
                           ('mv:0', 'operating_low'): 5, ('mv:0', 'operating_high'): 15}
        for (variable, kind), expected in expected_bounds.items():
            with self.subTest(variable=variable, kind=kind):
                series = next(row for row in data['series']
                              if row['variableId'] == variable and row['kind'] == kind)
                self.assertTrue(series['available'])
                self.assertEqual({point['value'] for point in series['points']}, {expected})
        summary = next(row for row in data['windowSummary']['rows'] if row['variableId'] == 'cv:0')
        self.assertEqual(summary['operatingObservedSeconds'], 30)
        self.assertEqual(summary['operatingViolationSeconds'], 10)
        violations = [event for event in data['events'] if event['kind'] == 'violation']
        self.assertEqual([(event['variableId'], event['time']) for event in violations],
                         [('cv:0', data['originTime'] + 20000)])

    def test_metadata_cannot_select_paths_executables_or_unrecorded_trials(self):
        base = dict(run_id='run-1', trial_id=self.trial_id, scenario='full', transport='direct', owner='owner-1')
        for change in ({'trial_id': '../escape'}, {'trial_id': 'unrecorded'}, {'binary': str(self.binary)},
                       {'transport': 'opc-ua'}, {'scenario': '/tmp/scenario.json'}):
            with self.assertRaises(ValueError):
                execute(self.root, 'twin-create', {**base, **change})
        self.run_record['trials'][0]['controller'] = '../native'
        self.save('run.json', self.run_record)
        with self.assertRaisesRegex(ValueError, 'identity'):
            self.create()

    def test_incompatible_sample_time_and_input_hash_changes_reject_new_sessions(self):
        self.controller['tuning']['sample_time'] = 5
        self.save(self.trial_id + '-controller.json', self.controller)
        with self.assertRaisesRegex(ValueError, 'sample times'):
            self.create()
        self.save(self.trial_id + '.command.json', {'controller_sha256': '0' * 64})
        with self.assertRaisesRegex(ValueError, 'integrity'):
            self.create()

    def test_equal_dimensions_do_not_hide_different_names_or_engineering_units(self):
        for field, changed in (('name', 'OtherTemperature'), ('units', 'F')):
            with self.subTest(field=field):
                controller = json.loads(self.controller_bytes)
                controller['variables']['cvs'][0][field] = changed
                self.save(self.trial_id + '-controller.json', controller)
                with self.assertRaisesRegex(ValueError, 'names, order or engineering units'):
                    self.create()

    def test_restart_marks_empty_partial_output_interrupted_never_completed(self):
        session = self.create()
        directory = self.root / 'twin-sessions' / session['id']
        (directory / 'result.json').write_bytes(b'')
        self.reconcile()
        reopened = execute(self.root, 'twin-get', {'id': session['id'], 'owner': 'owner-2'})
        self.assertEqual(reopened['status'], 'interrupted')
        self.assertEqual(reopened['samples'], 0)
        with self.assertRaisesRegex(ValueError, 'completed'):
            execute(self.root, 'twin-session-series', {'id': session['id'], 'owner': 'owner-2'})

    def test_completed_session_reopens_independently_and_original_artifacts_stay_unchanged(self):
        session = self.create()
        directory = self.root / 'twin-sessions' / session['id']
        (directory / 'result.json').write_text(json.dumps({'status': 'valid', 'trace': self.trace}))
        updated = execute(self.root, 'twin-update', {'id': session['id'], 'owner': 'owner-1',
            'response': {'status': 'completed', 'samples': 3, 'requested_samples': 3, 'sample_time': 10}})
        self.assertEqual(updated['status'], 'completed')
        (directory / 'owner.lock').unlink()  # A legacy archive need not have a mutable lock file.
        before = {p.name: p.read_bytes() for p in directory.iterdir()}
        self.reconcile()
        reopened = execute(self.root, 'twin-get', {'id': session['id'], 'owner': 'owner-2'})
        self.assertEqual(reopened['status'], 'completed')
        chart = execute(self.root, 'twin-session-series', {'id': session['id'], 'owner': 'owner-2'})
        self.assertEqual(chart['rawSamples'], 3)
        self.assertEqual(before, {p.name: p.read_bytes() for p in directory.iterdir()})
        (directory / 'result.json').write_text('{}')
        with self.assertRaisesRegex(ValueError, 'integrity'):
            execute(self.root, 'twin-session-series', {'id': session['id'], 'owner': 'owner-2'})

    def test_relative_selection_retains_elapsed_clock_without_inventing_calendar_date(self):
        self.run_record['study']['config']['start'] = 'relative'
        self.save('run.json', self.run_record)
        session = execute(self.root, 'twin-create', {'run_id': 'run-1', 'trial_id': self.trial_id,
            'scenario': 'selection', 'transport': 'direct', 'owner': 'owner-1'})
        self.assertEqual(session['time_basis'], 'relative')
        self.assertEqual(session['start'], 'relative')
        self.assertEqual(session['source_offset_seconds'], 1000)
        data = execute(self.root, 'twin-trial-series', {'id': 'run-1', 'trial_id': self.trial_id, 'owner': 'owner-1'})
        self.assertEqual(data['originTime'], 0)
        self.assertEqual(data['timeBasis'], 'relative')

    def test_lost_completion_response_recovers_only_exact_finalized_scenario(self):
        session = self.create()
        directory = self.root / 'twin-sessions' / session['id']
        (directory / 'result.json').write_text(json.dumps({'status': 'valid', 'trace': self.trace}))
        self.reconcile('new-owner')
        recovered = execute(self.root, 'twin-get', {'id': session['id'], 'owner': 'new-owner'})
        self.assertEqual(recovered['status'], 'completed')
        self.assertEqual(recovered['samples'], 3)
        second = self.create()
        directory = self.root / 'twin-sessions' / second['id']
        wrong_times = dict(self.trace, time_seconds=[0, 10, 30])
        (directory / 'result.json').write_text(json.dumps({'status': 'valid', 'trace': wrong_times}))
        self.reconcile('new-owner')
        recovered = execute(self.root, 'twin-get', {'id': second['id'], 'owner': 'new-owner'})
        self.assertEqual(recovered['status'], 'interrupted')

    def test_recovery_of_final_native_output_restores_exact_last_forecast_without_reexecution(self):
        session = self.create()
        started = self.control(session, 'start', 'continuous')['session']
        execute(self.root, 'twin-native-begin', {'id': session['id'], 'owner': 'owner-1',
            'expected_sequence': started['sequence'], 'command': {'command': 'advance', 'expected_samples': 0, 'count': 128}})
        directory = self.root / 'twin-sessions' / session['id']
        (directory / 'result.json').write_text(json.dumps({'status': 'valid', 'trace': self.trace}))
        self.write_forecasts(directory, 'forecast.ndjson')
        self.reconcile()
        recovered = execute(self.root, 'twin-get', {'id': session['id'], 'owner': 'owner-2'})
        self.assertEqual(recovered['execution_status'], 'completed')
        self.assertEqual(recovered['samples'], 3)
        self.assertEqual(recovered['latest_forecast'], self.forecast(2))
        self.assertEqual(recovered['frame']['cv'], [52])
        self.assertEqual(recovered['native_sequence'], 1)
        self.assertIsNone(recovered['pending_native'])

    def test_full_quarter_is_frozen_without_truncating_to_display_window(self):
        count = 129600
        self.run_record['study']['config']['sample_time'] = 60
        self.controller['tuning']['sample_time'] = 60
        self.save('run.json', self.run_record)
        self.save(self.trial_id + '-controller.json', self.controller)
        plant = json.loads((self.source / 'plant.json').read_text())
        self.save('plant.json', dict(plant, sample_time=60))
        scenario = dict(self.scenario, sample_time=60, time_seconds=list(range(0, count * 60, 60)),
                        cv=[[50]] * count, mv=[[10]] * count, dv=[[]] * count, targets=[[50]] * count)
        self.save('scenario.json', scenario)
        session = self.create()
        self.assertEqual(session['requested_samples'], 129600)
        self.assertEqual(session['sample_time'], 60)
        frozen = self.root / 'twin-sessions' / session['id'] / 'scenario.json'
        self.assertEqual(frozen.read_bytes(), (self.source / 'scenario.json').read_bytes())
        self.assertEqual(session['samples'], 0)  # Freeze is not evidence of native execution.

    def test_manifest_tampering_and_symlinked_controller_are_rejected(self):
        session = self.create()
        state_path = self.root / 'twin-sessions' / session['id'] / 'session.json'
        state = json.loads(state_path.read_text())
        state['controller_sha256'] = '0' * 64
        state_path.write_text(json.dumps(state))
        with self.assertRaisesRegex(ValueError, 'immutable manifest'):
            execute(self.root, 'twin-get', {'id': session['id'], 'owner': 'owner-1'})
        controller_path = self.source / (self.trial_id + '-controller.json')
        controller_path.unlink()
        controller_path.symlink_to(self.binary)
        with self.assertRaisesRegex(ValueError, 'unsafe'):
            self.create()

    def test_settings_versions_freeze_effective_targets_without_rewriting_trial(self):
        variables = json.loads(self.controller_bytes)['variables']
        variables['cvs'][0]['limits']['target'] = 60
        variables['cvs'][0]['optimization_mode']['value'] = 60
        variables['cvs'][0]['weight'] = 3
        version = execute(self.root, 'twin-settings', {'run_id': 'run-1', 'trial_id': self.trial_id,
            'name': 'Diagnostic target 60', 'variables': variables, 'owner': 'owner-1'})
        session = self.create(settings_version_id=version['id'])
        directory = self.root / 'twin-sessions' / session['id']
        self.assertEqual(json.loads((directory / 'scenario.json').read_text())['targets'], [[60]] * 3)
        self.assertEqual(session['variables']['cv'][0]['target'], 60)
        self.assertEqual(session['variables']['cv'][0]['weight'], 3)
        self.assertEqual(session['controller_sha256'], version['controller_sha256'])
        self.assertNotEqual(session['scenario_sha256'], self.create()['scenario_sha256'])
        self.assertEqual((self.source / (self.trial_id + '-controller.json')).read_bytes(), self.controller_bytes)
        self.assertEqual(json.loads((self.source / 'scenario.json').read_text())['targets'], [[50]] * 3)
        config = execute(self.root, 'twin-config', {'id': 'run-1', 'trial_id': self.trial_id, 'owner': 'owner-1'})
        self.assertEqual(config['settings_versions'], [version])
        self.assertEqual(config['equipment_views'], [])
        self.assertIsNone(self.create().get('equipment_view'))
        response = self.native_response(0)
        with self.assertRaisesRegex(ValueError, 'targets differ'):
            execute(self.root, 'twin-update', {'id': session['id'], 'owner': 'owner-1', 'response': response})
        response['frame']['targets'] = [60]
        updated = execute(self.root, 'twin-update', {'id': session['id'], 'owner': 'owner-1', 'response': response})
        self.assertEqual(updated['frame']['targets'], [60])

    def test_settings_reject_identity_physics_unknown_fields_and_invalid_tuning(self):
        for change in ({'name': 'Other'}, {'units': 'F'}, {'node_ids': {'pv': '/tmp/other'}},
                       {'weight': -1}, {'alpha': 2}, {'is_integrating': 1},
                       {'optimization_mode': {'type': 'Target', 'value': 99}},
                       {'limits': {'low': 100, 'high': 0}}, {'physics': {'gain': [[5]]}}):
            variables = json.loads(self.controller_bytes)['variables']
            variables['cvs'][0].update(change)
            with self.subTest(change=change), self.assertRaises(ValueError):
                execute(self.root, 'twin-settings', {'run_id': 'run-1', 'trial_id': self.trial_id,
                    'name': 'Rejected', 'variables': variables, 'owner': 'owner-1'})

    def test_non_target_settings_preserve_scenario_bytes_and_apply_mv_target(self):
        variables = json.loads(self.controller_bytes)['variables']
        variables['mvs'][0].update(target=30, target_weight=2, weight_r=4, max_move=3)
        variables['cvs'][0].update(optimization_mode={'type': 'Zone'}, slack_weight=200, is_integrating=True)
        version = execute(self.root, 'twin-settings', {'run_id': 'run-1', 'trial_id': self.trial_id,
            'name': 'Zone diagnostic', 'variables': variables, 'owner': 'owner-1'})
        session = self.create(settings_version_id=version['id'])
        directory = self.root / 'twin-sessions' / session['id']
        self.assertEqual((directory / 'scenario.json').read_bytes(), (self.source / 'scenario.json').read_bytes())
        self.assertEqual(session['variables']['mv'][0]['target'], 30)
        self.assertEqual(session['variables']['mv'][0]['target_weight'], 2)
        self.assertEqual(session['variables']['cv'][0]['optimization_mode'], {'type': 'Zone'})

    def equipment_view(self):
        return {'schema_version': 1, 'label': 'Confirmed mill', 'layout': 'closed_ball_mill',
            'confirmed': True, 'equipment': [{'id': 'mill', 'name': 'Mill', 'kind': 'ball_mill',
                'position': [0, 0, 0], 'size': [4, 2, 2], 'rotation': [0, 0, 0]}],
            'connections': [], 'bindings': [{'id': 'temperature', 'equipment_id': 'mill',
                'role': 'cv', 'variable_name': 'Temperature', 'unit': 'C', 'channel': 'actual',
                'node_id': 'Temperature:pv', 'animation': 'none'}]}

    def test_equipment_versions_freeze_explicit_bindings_and_detect_tampering(self):
        view = self.equipment_view()
        version = execute(self.root, 'twin-equipment-view', {'run_id': 'run-1', 'trial_id': self.trial_id,
            'view': view, 'owner': 'owner-1'})
        session = self.create(equipment_view_id=version['id'])
        self.assertEqual(session['equipment_view'], version)
        self.assertEqual(session['controller_variables'], self.controller['variables'])
        view['label'] = 'Another immutable view'
        other = execute(self.root, 'twin-equipment-view', {'run_id': 'run-1', 'trial_id': self.trial_id,
            'view': view, 'owner': 'owner-1'})
        self.assertNotEqual(other['id'], version['id'])
        self.assertEqual(execute(self.root, 'twin-get', {'id': session['id'], 'owner': 'owner-1'})['equipment_view'], version)
        stored = self.root / 'twin-equipment-views' / (version['id'] + '.json')
        stored.write_bytes(stored.read_bytes() + b' ')
        with self.assertRaisesRegex(ValueError, 'integrity'):
            self.create(equipment_view_id=version['id'])
        # A frozen session is independent of subsequently damaged source version storage.
        self.assertEqual(execute(self.root, 'twin-get', {'id': session['id'], 'owner': 'owner-1'})['equipment_view'], version)

    def test_equipment_rejects_guessed_bindings_nonfinite_geometry_and_authority(self):
        for change in ({'variable_name': 'temperature'}, {'unit': 'F'}, {'role': 'mv'},
                       {'node_id': 'Other:pv'}, {'channel': 'command'}, {'animation': 'rotation'},
                       {'reference_value': 0}, {'approved': True}):
            view = self.equipment_view()
            view['bindings'][0].update(change)
            with self.subTest(change=change), self.assertRaises(ValueError):
                execute(self.root, 'twin-equipment-view', {'run_id': 'run-1', 'trial_id': self.trial_id,
                    'view': view, 'owner': 'owner-1'})
        for geometry in ([0, float('nan'), 0], [0, 0], [1001, 0, 0]):
            view = self.equipment_view()
            view['equipment'][0]['position'] = geometry
            with self.assertRaises(ValueError):
                execute(self.root, 'twin-equipment-view', {'run_id': 'run-1', 'trial_id': self.trial_id,
                    'view': view, 'owner': 'owner-1'})

    def test_mv_feedback_and_op_bindings_keep_distinct_units_and_nodes(self):
        for channel, unit, node in (('actual', '%', 'Fuel:sp'), ('op', 'A', 'Fuel:op'), ('mode', 'mode', 'Fuel:mode')):
            view = self.equipment_view()
            view['bindings'][0].update(role='mv', variable_name='Fuel', unit=unit, channel=channel, node_id=node)
            version = execute(self.root, 'twin-equipment-view', {'run_id': 'run-1', 'trial_id': self.trial_id,
                'view': view, 'owner': 'owner-1'})
            self.assertEqual(version['view']['bindings'][0]['unit'], unit)
        self.controller['variables']['mvs'][0]['node_ids']['applied_sp'] = 'Fuel:applied'
        self.save(self.trial_id + '-controller.json', self.controller)
        view['bindings'][0].update(channel='actual', unit='%', node_id='Fuel:sp')
        with self.assertRaisesRegex(ValueError, 'node differs'):
            execute(self.root, 'twin-equipment-view', {'run_id': 'run-1', 'trial_id': self.trial_id,
                'view': view, 'owner': 'owner-1'})
        view['bindings'][0]['node_id'] = 'Fuel:applied'
        accepted = execute(self.root, 'twin-equipment-view', {'run_id': 'run-1', 'trial_id': self.trial_id,
            'view': view, 'owner': 'owner-1'})
        self.assertEqual(accepted['view']['bindings'][0]['node_id'], 'Fuel:applied')

    def test_version_selection_rejects_cross_run_and_runtime_forgery(self):
        version = execute(self.root, 'twin-settings', {'run_id': 'run-1', 'trial_id': self.trial_id,
            'name': 'Baseline diagnostic', 'variables': self.controller['variables'], 'owner': 'owner-1'})
        other = self.root / 'runs' / 'run-2'
        other.mkdir()
        for path in self.source.iterdir():
            (other / path.name).write_bytes(path.read_bytes())
        run = dict(self.run_record, id='run-2')
        (other / 'run.json').write_text(json.dumps(run))
        with self.assertRaisesRegex(ValueError, 'another run'):
            execute(self.root, 'twin-create', {'run_id': 'run-2', 'trial_id': self.trial_id,
                'scenario': 'full', 'transport': 'direct', 'owner': 'owner-1', 'settings_version_id': version['id']})
        for extra in ({'binary': '/tmp/attacker'}, {'controller': self.controller}, {'approved': True}):
            with self.assertRaises(ValueError):
                execute(self.root, 'twin-settings', {'run_id': 'run-1', 'trial_id': self.trial_id,
                    'name': 'Rejected', 'variables': self.controller['variables'], 'owner': 'owner-1', **extra})

    def forecast(self, sample):
        return {'schema_version': 1, 'sample': sample, 'issued_at_seconds': sample * 10,
            'sample_time': 10, 'cv_count': 1, 'mv_count': 1, 'prediction_horizon': 3,
            'control_horizon': 1, 'layout': 'step-major', 'cv_first_offset_steps': 1,
            'mv_first_offset_steps': 0, 'cv': [51 + sample, 52 + sample, 53 + sample],
            'mv_base': [10], 'mv_delta': [0]}

    def native_response(self, sample):
        frame = {key: rows[sample] for key, rows in self.trace.items()}
        frame['solver_status'] = 'Solved'
        return {'status': 'paused', 'samples': sample + 1, 'frame': frame,
                'latest_forecast': self.forecast(sample)}

    def write_forecasts(self, directory, name):
        header = {key: value for key, value in self.forecast(0).items()
                  if key not in ('sample', 'issued_at_seconds', 'cv', 'mv_base', 'mv_delta')}
        content = json.dumps(header).encode() + b'\n'
        offsets = []
        for sample in range(3):
            offsets.append(len(content))
            row = {key: self.forecast(sample)[key] for key in ('sample', 'issued_at_seconds', 'cv', 'mv_base', 'mv_delta')}
            content += json.dumps(row).encode() + b'\n'
        (directory / name).write_bytes(content)
        (directory / (name + '.index.json')).write_text(json.dumps({
            'schema_version': 1, 'header': header, 'samples': 3, 'offsets': offsets}))

    def test_snapshot_persists_exact_forecast_and_rejects_stale_or_forged_horizons(self):
        session = self.create()
        payload = {'id': session['id'], 'owner': 'owner-1', 'response': self.native_response(0)}
        updated = execute(self.root, 'twin-update', payload)
        self.assertEqual(updated['latest_forecast'], self.forecast(0))
        for _ in range(2):
            snapshot = execute(self.root, 'twin-get', {'id': session['id'], 'owner': 'owner-1'})
            self.assertEqual(snapshot['samples'], 1)
            self.assertEqual(snapshot['latest_forecast'], self.forecast(0))
        for changes in ({'sample': 1}, {'issued_at_seconds': 10}, {'cv': [51]}, {'cv_count': True},
                        {'controller_sha256': '0' * 64}, {'mv_delta': [float('nan')]}):
            response = self.native_response(0)
            response['latest_forecast'].update(changes)
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                execute(self.root, 'twin-update', dict(payload, response=response))
        self.reconcile('new-owner')
        reopened = execute(self.root, 'twin-get', {'id': session['id'], 'owner': 'new-owner'})
        self.assertEqual(reopened['status'], 'interrupted')
        self.assertEqual(reopened['latest_forecast'], self.forecast(0))
        self.assertEqual(reopened['frame'], self.native_response(0)['frame'])

    def test_archived_frame_uses_exact_index_offsets_and_recorded_forecast(self):
        self.write_forecasts(self.source, self.trial_id + '.forecast.ndjson')
        payload = {'id': 'run-1', 'trial_id': self.trial_id, 'sample': 1, 'owner': 'owner-1'}
        frame = execute(self.root, 'twin-trial-frame', payload)
        self.assertEqual(frame['sample'], 1)
        self.assertEqual(frame['total_samples'], 3)
        self.assertEqual(frame['frame']['cv'], [51])
        self.assertEqual(frame['frame']['time_seconds'], 10)
        self.assertEqual(frame['latest_forecast'], self.forecast(1))
        self.assertEqual(frame['start'], '2026-01-01T00:16:40Z')
        self.assertEqual(frame['source_offset_seconds'], 1000)
        for sample in (-1, 3, 1.5, True, '1'):
            with self.assertRaises(ValueError):
                execute(self.root, 'twin-trial-frame', dict(payload, sample=sample))
        path = self.source / (self.trial_id + '.forecast.ndjson.index.json')
        index = json.loads(path.read_text())
        index['offsets'][1] += 1
        path.write_text(json.dumps(index))
        with self.assertRaisesRegex(ValueError, 'line boundary'):
            execute(self.root, 'twin-trial-frame', payload)

    def test_completed_session_frame_verifies_forecast_hash_and_old_missing_archive(self):
        old = execute(self.root, 'twin-trial-frame',
            {'id': 'run-1', 'trial_id': self.trial_id, 'sample': 0, 'owner': 'owner-1'})
        self.assertIsNone(old['latest_forecast'])
        session = self.create()
        payload = {'id': session['id'], 'sample': 2, 'owner': 'owner-1'}
        with self.assertRaisesRegex(ValueError, 'completed'):
            execute(self.root, 'twin-session-frame', payload)
        directory = self.root / 'twin-sessions' / session['id']
        self.write_forecasts(directory, 'forecast.ndjson')
        (directory / 'result.json').write_text(json.dumps({'status': 'valid', 'trace': self.trace}))
        response = dict(self.native_response(2), status='completed')
        execute(self.root, 'twin-update', {'id': session['id'], 'owner': 'owner-1', 'response': response})
        archived = execute(self.root, 'twin-session-frame', payload)
        self.assertEqual(archived['latest_forecast'], self.forecast(2))
        self.assertEqual(archived['source_offset_seconds'], 0)
        path = directory / 'forecast.ndjson'
        path.write_bytes(path.read_bytes() + b' ')
        with self.assertRaisesRegex(ValueError, 'integrity'):
            execute(self.root, 'twin-session-frame', payload)


if __name__ == '__main__':
    unittest.main()
