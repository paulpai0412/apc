"""Consumer-visible chart evidence: timestamps, quality, extrema and archived horizons."""
import json
from pathlib import Path
import tempfile
import unittest

from research import execute


class ResearchTrendTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.directory = self.root / 'runs' / 'run-1'
        self.directory.mkdir(parents=True)
        self.config = {'start': '2026-01-01T00:00:00Z', 'sample_time': 10, 'variables': {
            'cv': [{'name': 'Temperature', 'unit': 'C', 'low': 10, 'high': 90, 'spec_low': 30, 'spec_high': 60}],
            'mv': [{'name': 'Fuel', 'unit': '%', 'low': 0, 'high': 100}],
            'dv': [{'name': 'Feed', 'unit': 't/h', 'low': 1, 'high': 20}]}}
        self.run = {'id': 'run-1', 'started_at': '2026-10-01T00:00:00Z', 'study': {
            'config': self.config, 'research': {'bands': [{'half_width': 2}]}}}
        self.trace = {'time_seconds': [0, 10, 20, 40], 'cv': [[40], [41], [None], [44]],
                      'mv': [[10], [11], [12], [13]], 'dv': [[5], [6], [7], [8]],
                      'command_mv': [[11], [12], [13], [14]], 'targets': [[50], [50], [55], [55]],
                      'predicted_cv': [[42], [43], [45], [99]], 'quality': [[192, 192, 192], [64, 0, 192], [192, 192, 192], [192, 192, 192]]}
        self.save()

    def save(self):
        (self.directory / 'run.json').write_text(json.dumps(self.run))
        (self.directory / 'soft_band-transport.json').write_text(json.dumps({'trace': self.trace}))

    def query(self, operation='series', **kwargs):
        return execute(self.root, operation, {'id': 'run-1', 'method': 'soft_band', **kwargs})

    def signal(self, data, variable, kind):
        return next(row for row in data['series'] if row['variableId'] == variable and row['kind'] == kind)

    def test_one_step_forecast_matches_valid_time_and_never_current_measurement(self):
        data = self.query()
        prediction = self.signal(data, 'cv:0', 'prediction')
        origin = data['originTime']
        self.assertEqual(origin, 1767225600000, 'Chart dates come from the scenario, not the wall-clock execution date')
        by_time = {p['time'] - origin: p for p in prediction['points']}
        self.assertEqual(by_time[10000]['value'], 42)
        self.assertEqual(by_time[10000]['issuedAt'], origin)
        self.assertEqual(by_time[20000]['value'], 43)
        self.assertNotIn(50000, by_time)
        self.assertFalse(any(p['value'] == 99 for p in prediction['points']))
        self.assertEqual(self.signal(data, 'cv:0', 'actual')['points'][1]['quality'], 'uncertain')
        self.assertEqual(self.signal(data, 'mv:0', 'actual')['points'][1]['quality'], 'bad')
        self.assertTrue(any(p['value'] is None and p['quality'] == 'missing' for p in self.signal(data, 'cv:0', 'actual')['points']))
        self.assertTrue(any(e['kind'] == 'gap' for e in data['events']))
        extended = self.signal(self.query(end=100), 'cv:0', 'prediction')
        self.assertFalse(any(p['value'] == 99 for p in extended['points']))
        window = self.signal(self.query(start=10, end=10), 'cv:0', 'prediction')
        self.assertEqual(window['points'][0]['value'], 42)

    def test_relative_history_keeps_elapsed_time_and_forecast_lead_without_inventing_a_date(self):
        self.config['start'] = 'relative'
        self.save()
        data = self.query(start=10, end=40)
        self.assertEqual(data['timeBasis'], 'relative')
        self.assertEqual(data['originTime'], 0)
        actual = self.signal(data, 'cv:0', 'actual')['points']
        self.assertEqual([(p['time'], p['value']) for p in actual if p['value'] is not None], [(10000, 41), (40000, 44)])
        self.archive()
        forecast = self.query('forecast', at=19)
        self.assertEqual(forecast['range'], {'start': 10000, 'end': 40000})
        points = self.signal(forecast, 'cv:0', 'prediction')['points']
        self.assertEqual([(p['time'], p['issuedAt'], p['value']) for p in points], [(20000, 10000, 43), (30000, 10000, 47), (40000, 10000, 49)])

    def test_absent_forecast_and_quality_are_not_fabricated(self):
        del self.trace['predicted_cv']
        del self.trace['quality']
        self.save()
        data = self.query()
        prediction = self.signal(data, 'cv:0', 'prediction')
        self.assertFalse(prediction['available'])
        self.assertEqual(prediction['points'], [])
        self.assertEqual(self.signal(data, 'cv:0', 'actual')['points'][0]['quality'], 'unknown')

    def test_unfinalized_or_corrupt_index_never_returns_a_snapshot(self):
        path = self.archive()
        index = Path(str(path) + '.index.json')
        for content in ('', '{}', '{"schema_version":1,"offsets":[-1],"samples":1}'):
            index.write_text(content)
            data = self.query('forecast', at=10)
            self.assertFalse(self.signal(data, 'mv:0', 'plan')['available'])
            self.assertFalse(self.signal(data, 'cv:0', 'prediction')['available'])
            self.assertTrue(data['warnings'])

    def test_snapshot_before_first_record_and_excessive_lead_are_unavailable(self):
        path = self.archive()
        content = path.read_text().replace('"issued_at_seconds": 0', '"issued_at_seconds": 5')
        path.write_text(content)
        data = self.query('forecast', at=0)
        self.assertFalse(self.signal(data, 'cv:0', 'prediction')['available'])
        data = self.query(lead_steps=4)
        self.assertIn('exceeds', self.signal(data, 'cv:0', 'prediction')['unavailableReason'])

    def test_complete_horizons_keep_step_major_variables_and_cumulative_mv_coordinates(self):
        self.config['variables']['cv'].append({'name': 'Pressure', 'unit': 'bar', 'low': 1, 'high': 200})
        self.config['variables']['mv'].append({'name': 'Valve', 'unit': '%', 'low': 0, 'high': 100})
        self.run['study']['research']['bands'].append({'half_width': 1})
        for key in ('cv', 'mv', 'targets', 'command_mv'):
            for row in self.trace[key]:
                row.append(20)
        self.save()
        header = {'schema_version': 1, 'sample_time': 10, 'cv_count': 2, 'mv_count': 2,
                  'prediction_horizon': 3, 'control_horizon': 2, 'layout': 'step-major',
                  'cv_first_offset_steps': 1, 'mv_first_offset_steps': 0}
        row = {'sample': 0, 'issued_at_seconds': 0, 'cv': [42, 101, 46, 102, 48, 103],
               'mv_base': [10, 20], 'mv_delta': [2, -1, -1, 3]}
        path = self.directory / 'soft_band-transport.forecast.ndjson'
        first_line = (json.dumps(header) + '\n').encode()
        path.write_bytes(first_line + (json.dumps(row) + '\n').encode())
        Path(str(path) + '.index.json').write_text(json.dumps({
            'schema_version': 1, 'header': header, 'offsets': [len(first_line)], 'samples': 1}))
        data = self.query('forecast', at=0)
        self.assertEqual([p['value'] for p in self.signal(data, 'cv:1', 'prediction')['points']], [101, 102, 103])
        self.assertEqual([p['value'] for p in self.signal(data, 'mv:0', 'plan')['points']], [12, 11])
        self.assertEqual([p['value'] for p in self.signal(data, 'mv:1', 'plan')['points']], [19, 22])

    def test_display_decimation_preserves_each_signal_extrema_and_quality_gaps(self):
        count = 4000
        self.trace = {key: [[1] for _ in range(count)] for key in ('cv', 'mv', 'dv', 'targets', 'command_mv', 'predicted_cv')}
        self.trace['time_seconds'] = [10 * i for i in range(count)]
        self.trace['quality'] = [[192, 192, 192] for _ in range(count)]
        for key, index, value in [('mv', 103, 71), ('dv', 105, 72), ('predicted_cv', 107, 73), ('command_mv', 109, 74)]:
            self.trace[key][index][0] = value
        self.trace['quality'][111][0] = 0
        self.trace['cv'][113][0] = None
        self.trace['cv_low'] = [[10 if i < 115 else 12] for i in range(count)]
        self.save()
        data = self.query()
        for variable, kind, spike in [('mv:0', 'actual', 71), ('dv:0', 'actual', 72), ('cv:0', 'prediction', 73), ('mv:0', 'command', 74)]:
            self.assertIn(spike, [p['value'] for p in self.signal(data, variable, kind)['points']])
        self.assertIn('bad', [p['quality'] for p in self.signal(data, 'cv:0', 'actual')['points']])
        self.assertIn(None, [p['value'] for p in self.signal(data, 'cv:0', 'actual')['points']])
        self.assertEqual({p['value'] for p in self.signal(data, 'cv:0', 'operating_low')['points']}, {10, 12})
        self.assertTrue(any(e['kind'] == 'limit' for e in data['events']))

    def archive(self):
        header = {'schema_version': 1, 'sample_time': 10, 'cv_count': 1, 'mv_count': 1,
                  'prediction_horizon': 3, 'control_horizon': 2, 'layout': 'step-major',
                  'cv_first_offset_steps': 1, 'mv_first_offset_steps': 0}
        path = self.directory / 'soft_band-transport.forecast.ndjson'
        offsets = []
        with path.open('wb') as handle:
            handle.write((json.dumps(header) + '\n').encode())
            for sample, issued, cv, base in [(0, 0, [42, 46, 48], 10), (1, 10, [43, 47, 49], 11)]:
                offsets.append(handle.tell())
                handle.write((json.dumps({'sample': sample, 'issued_at_seconds': issued, 'cv': cv, 'mv_base': [base], 'mv_delta': [2, -1]}) + '\n').encode())
        Path(str(path) + '.index.json').write_text(json.dumps({'schema_version': 1, 'header': header, 'offsets': offsets, 'samples': 2}))
        return path

    def test_snapshot_reads_nearest_issued_full_horizon_and_cumulative_plan(self):
        self.archive()
        data = self.query('forecast', at=19)
        origin = data['originTime']
        cv = self.signal(data, 'cv:0', 'prediction')['points']
        self.assertEqual([(p['time'] - origin, p['value'], p['issuedAt'] - origin) for p in cv], [(20000, 43, 10000), (30000, 47, 10000), (40000, 49, 10000)])
        mv = self.signal(data, 'mv:0', 'plan')['points']
        self.assertEqual([(p['time'] - origin, p['value']) for p in mv], [(10000, 13), (20000, 12)])
        actual = self.signal(data, 'cv:0', 'actual')['points']
        self.assertEqual([(p['time'] - origin, p['value']) for p in actual], [(10000, 41), (20000, None), (30000, None), (40000, 44)])
        target = self.signal(data, 'cv:0', 'target')['points']
        self.assertEqual([p['value'] for p in target], [50, 50], 'The issued forecast reference must not acquire subsequent target changes')
        self.assertFalse(self.signal(data, 'dv:0', 'prediction')['available'])
        lead = self.signal(self.query(lead_steps=2), 'cv:0', 'prediction')
        self.assertEqual([(p['time'] - origin, p['value']) for p in lead['points'] if p['value'] is not None], [(20000, 46), (30000, 47)])

    def test_missing_and_corrupt_archive_explicitly_unavailable_without_mutation(self):
        before = (self.directory / 'soft_band-transport.json').read_bytes()
        for operation, args in [('forecast', {'at': 10}), ('series', {'lead_steps': 2})]:
            data = self.query(operation, **args)
            self.assertFalse(self.signal(data, 'cv:0', 'prediction')['available'])
            self.assertTrue(data['warnings'])
        path = self.archive()
        path.write_text(path.read_text().replace('"cv": [42, 46, 48]', '"cv": [42]'))
        data = self.query('forecast', at=0)
        self.assertFalse(self.signal(data, 'cv:0', 'prediction')['available'])
        self.assertIn('corrupt', ' '.join(data['warnings']).lower())
        self.assertEqual((self.directory / 'soft_band-transport.json').read_bytes(), before)

    def test_specs_are_distinct_from_frozen_operating_limits_and_soft_band(self):
        data = self.query()
        self.assertEqual(self.signal(data, 'cv:0', 'spec_low')['points'][0]['value'], 30)
        self.assertEqual(self.signal(data, 'cv:0', 'operating_low')['points'][0]['value'], 10)
        self.assertEqual([p['value'] for p in self.signal(data, 'cv:0', 'soft_low')['points']], [48, 48, 53, None, 53])

    def test_window_statistics_use_good_raw_samples_and_consecutive_commands(self):
        data = self.query()
        rows = {row['variableId']: row for row in data['windowSummary']['rows']}
        cv, mv = rows['cv:0'], rows['mv:0']
        self.assertEqual((cv['samples'], cv['goodSamples']), (4, 2))
        self.assertEqual((cv['mean'], cv['variance'], cv['stddev']), (42, 4, 2))
        self.assertEqual((cv['bias'], cv['biasSamples']), (-10.5, 2))
        self.assertEqual((cv['specViolationSeconds'], cv['specObservedSeconds']), (0, 20))
        self.assertEqual((cv['operatingViolationSeconds'], cv['operatingObservedSeconds']), (0, 20))
        self.assertIsNone(mv['specViolationSeconds'])
        self.assertEqual((mv['commandMovement'], mv['commandPairs']), (None, 0))
        self.assertEqual(cv['seriesId'], self.signal(data, 'cv:0', 'actual')['id'])
        self.assertEqual(cv['groupId'], 'soft_band:transport')
        self.assertEqual({row['groupId'] for row in data['series']}, {'soft_band:transport'})
        self.trace['quality'] = [[192, 192, 192] for _ in self.trace['time_seconds']]
        self.save()
        window = self.query(start=10, end=20)
        mv = next(row for row in window['windowSummary']['rows'] if row['variableId'] == 'mv:0')
        self.assertEqual((mv['commandMovement'], mv['commandPairs']), (1, 1))
        movement = self.signal(window, 'mv:0', 'movement')['points']
        self.assertEqual([p['value'] for p in movement], [None, 1])

    def test_full_resolution_statistics_survive_display_reduction_and_exclude_invalid_spikes(self):
        count = 4001
        self.trace = {key: [[1] for _ in range(count)] for key in ('cv', 'mv', 'dv', 'targets', 'command_mv')}
        self.trace['time_seconds'] = [10 * i for i in range(count)]
        self.trace['quality'] = [[192, 192, 192] for _ in range(count)]
        self.trace['cv'][103][0] = 101
        self.trace['cv'][105][0] = 99999
        self.trace['quality'][105][0] = 0
        self.save()
        data = self.query()
        cv = next(row for row in data['windowSummary']['rows'] if row['variableId'] == 'cv:0')
        self.assertLess(len(self.signal(data, 'cv:0', 'actual')['points']), count)
        self.assertEqual(cv['goodSamples'], 4000)
        self.assertAlmostEqual(cv['mean'], 1.025)
        self.assertAlmostEqual(cv['variance'], 2.499375)
        self.assertAlmostEqual(cv['bias'], 0.025)
        self.assertEqual(cv['operatingViolationSeconds'], 40000)
        self.assertEqual(cv['specViolationSeconds'], 40000)
        self.assertEqual(cv['specObservedSeconds'], 40000)

    def test_overview_uses_full_temporal_bins_good_median_extrema_and_empty_buckets(self):
        times = [i * 10 for i in range(1000) if not 8 <= i < 12]
        self.trace = {key: [[1] for _ in times] for key in ('cv', 'mv', 'dv', 'targets', 'command_mv')}
        self.trace['time_seconds'] = times
        self.trace['quality'] = [[192, 192, 192] for _ in times]
        self.trace['cv'][:4] = [[1], [2], [100], [4]]
        self.save()
        full = self.query()
        window = self.query(start=2000, end=2100)
        self.assertEqual(window['overview'], full['overview'])
        overview = full['overview']
        points = overview['series'][0]['points']
        self.assertLessEqual(len(points), 250)
        self.assertEqual((points[0]['min'], points[0]['max'], points[0]['median'], points[0]['count'], points[0]['goodCount']),
                         (1, 100, 3, 4, 4))
        self.assertEqual((points[2]['min'], points[2]['max'], points[2]['median'], points[2]['count'], points[2]['goodCount']),
                         (None, None, None, 0, 0))
        self.trace['quality'][2][0] = 0
        self.save()
        first = self.query()['overview']['series'][0]['points'][0]
        self.assertEqual((first['min'], first['max'], first['median'], first['count'], first['goodCount']), (1, 4, 2, 4, 3))

    def test_residual_pairs_exact_valid_time_quality_and_archived_leads(self):
        data = self.query()
        residual = self.signal(data, 'cv:0', 'residual')
        self.assertTrue(all(p['value'] is None for p in residual['points']))
        self.trace['quality'][1][0] = 192
        self.trace['cv'][2][0] = 50
        self.save()
        data = self.query(start=10, end=20)
        residual = self.signal(data, 'cv:0', 'residual')
        origin = data['originTime']
        self.assertEqual([(p['time'] - origin, p['issuedAt'] - origin, p['value']) for p in residual['points']],
                         [(10000, 0, -1), (20000, 10000, 7)])
        self.assertEqual(residual['leadSeconds'], 10)
        self.archive()
        lead = self.signal(self.query(lead_steps=2), 'cv:0', 'residual')
        self.assertEqual([(p['time'] - origin, p['value']) for p in lead['points']], [(20000, 4), (30000, None)])
        self.assertEqual(lead['leadSeconds'], 20)
        snapshot = self.query('forecast', at=19)
        self.assertNotIn('windowSummary', snapshot)
        residual = self.signal(snapshot, 'cv:0', 'residual')
        self.assertEqual([(p['time'] - origin, p['issuedAt'] - origin, p['value']) for p in residual['points']],
                         [(20000, 10000, 7), (30000, 10000, None), (40000, 10000, -5)])

    def test_command_movement_keeps_signed_steps_without_crossing_quality_or_time_gaps(self):
        self.trace['command_mv'] = [[20], [10], [13], [99]]
        self.trace['quality'] = [[192, 192, 192] for _ in self.trace['time_seconds']]
        self.save()
        data = self.query()
        self.assertEqual([p['value'] for p in self.signal(data, 'mv:0', 'movement')['points']], [None, -10, 3, None, None])
        row = next(row for row in data['windowSummary']['rows'] if row['variableId'] == 'mv:0')
        self.assertEqual((row['commandMovement'], row['commandPairs']), (13, 2))
        self.trace['quality'][1][1] = 64
        self.save()
        data = self.query()
        self.assertFalse(self.signal(data, 'mv:0', 'movement')['available'])
        row = next(row for row in data['windowSummary']['rows'] if row['variableId'] == 'mv:0')
        self.assertEqual((row['commandMovement'], row['commandPairs']), (None, 0))

    def test_independent_spec_and_operating_coverage_and_transition_events(self):
        self.trace['cv'] = [[40], [70], [95], [45]]
        self.trace['quality'] = [[192, 192, 192] for _ in self.trace['time_seconds']]
        self.trace['time_seconds'] = [0, 10, 20, 30]
        self.trace['cv_low'] = [[10], [10], [None], [10]]
        self.save()
        data = self.query()
        row = data['windowSummary']['rows'][0]
        self.assertEqual((row['specViolationSeconds'], row['specObservedSeconds']), (20, 40))
        self.assertEqual((row['operatingViolationSeconds'], row['operatingObservedSeconds']), (0, 30))
        events = [e for e in data['events'] if e['kind'] == 'violation' and e['variableId'] == 'cv:0']
        self.assertEqual([e['time'] - data['originTime'] for e in events], [10000, 30000])
        self.assertTrue(all(e['groupId'] == 'soft_band:transport' for e in events))
        del self.trace['cv_low']
        self.save()
        restored = self.query()
        row = restored['windowSummary']['rows'][0]
        self.assertEqual((row['operatingViolationSeconds'], row['operatingObservedSeconds']), (10, 40))
        events = [e for e in restored['events'] if e['kind'] == 'violation' and e['variableId'] == 'cv:0']
        self.assertEqual([e['time'] - data['originTime'] for e in events], [10000, 20000, 30000, 30000])
        del self.config['variables']['cv'][0]['spec_low']
        del self.config['variables']['cv'][0]['spec_high']
        self.save()
        row = self.query()['windowSummary']['rows'][0]
        self.assertIsNone(row['specViolationSeconds'])
        self.assertEqual(row['specObservedSeconds'], 0)
        self.trace['quality'][1][0] = 0
        self.save()
        changes = [e for e in self.query()['events'] if e['kind'] == 'quality' and e['variableId'] == 'cv:0']
        self.assertEqual([e['time'] - data['originTime'] for e in changes], [10000, 20000])

    def test_window_inclusive_bounds_relative_units_and_authoritative_splits(self):
        self.config['start'] = 'relative'
        self.config['data_window'] = {'train_end': 2, 'validation_end': 4, 'samples': 6}
        self.save()
        data = self.query(start=10, end=20)
        self.assertEqual(data['range'], {'start': 10000, 'end': 20000})
        self.assertEqual(data['windowSummary']['range'], data['range'])
        self.assertEqual(data['windowSummary']['rows'][0]['samples'], 2)
        self.assertEqual(data['overview']['range'], {'start': 0, 'end': 50000})
        self.assertEqual(data['overview']['splits'], [
            {'name': 'train', 'start': 0, 'end': 20000},
            {'name': 'validation', 'start': 20000, 'end': 40000},
            {'name': 'test', 'start': 40000, 'end': 60000}])
        self.run['results'] = {'splits': [{'name': 'train', 'first': 0, 'last': 3},
                                        {'name': 'validation', 'first': 3, 'last': 6}]}
        self.config['start'] = '2026-01-01T08:00:00+08:00'
        self.save()
        absolute = self.query(start=10, end=10)
        self.assertEqual(absolute['originTime'], 1767225600000)
        self.assertEqual(absolute['windowSummary']['rows'][0]['samples'], 1)
        self.assertEqual(absolute['overview']['splits'][0]['end'], 1767225630000)

    def test_unavailable_legacy_quality_and_command_do_not_become_zero_evidence(self):
        del self.trace['quality']
        self.save()
        data = self.query()
        cv, mv = data['windowSummary']['rows'][:2]
        self.assertEqual(cv['goodSamples'], 0)
        self.assertIsNone(cv['mean'])
        self.assertIsNone(cv['bias'])
        self.assertEqual(cv['specObservedSeconds'], 0)
        self.assertIsNone(cv['specViolationSeconds'])
        self.assertIsNone(cv['operatingViolationSeconds'])
        self.assertIsNone(mv['commandMovement'])
        self.assertFalse(self.signal(data, 'mv:0', 'movement')['available'])
        self.assertTrue(all(p['value'] is None for p in self.signal(data, 'cv:0', 'residual')['points']))
        del self.trace['command_mv']
        self.trace['quality'] = [[192, 192, 192] for _ in self.trace['time_seconds']]
        self.save()
        data = self.query()
        self.assertIsNone(data['windowSummary']['rows'][1]['commandMovement'])
        self.assertFalse(self.signal(data, 'mv:0', 'movement')['available'])

    def test_no_coverage_is_null_but_observed_nonviolations_and_constant_commands_are_zero(self):
        self.trace['quality'] = [[0, 0, 0] for _ in self.trace['time_seconds']]
        self.save()
        data = self.query()
        cv, mv = data['windowSummary']['rows'][:2]
        for prefix in ('spec', 'operating'):
            self.assertIsNone(cv[prefix + 'ViolationSeconds'])
            self.assertEqual(cv[prefix + 'ObservedSeconds'], 0)
        self.assertEqual((mv['commandMovement'], mv['commandPairs']), (None, 0))
        self.assertFalse(self.signal(data, 'mv:0', 'movement')['available'])

        self.trace['quality'] = [[192, 192, 192] for _ in self.trace['time_seconds']]
        self.trace['cv_low'] = [[None] for _ in self.trace['time_seconds']]
        self.trace['command_mv'] = [[12] for _ in self.trace['time_seconds']]
        self.save()
        data = self.query(start=0, end=10)
        cv, mv = data['windowSummary']['rows'][:2]
        self.assertEqual((cv['operatingViolationSeconds'], cv['operatingObservedSeconds']), (None, 0))
        self.assertEqual((cv['specViolationSeconds'], cv['specObservedSeconds']), (0, 20))
        self.assertEqual((mv['commandMovement'], mv['commandPairs']), (0, 1))
        movement = self.signal(data, 'mv:0', 'movement')
        self.assertTrue(movement['available'])
        self.assertEqual([p['value'] for p in movement['points']], [None, 0])

        isolated = self.query(start=10, end=10)
        mv = isolated['windowSummary']['rows'][1]
        self.assertEqual((mv['commandMovement'], mv['commandPairs']), (None, 0))
        self.assertFalse(self.signal(isolated, 'mv:0', 'movement')['available'])

    def test_calendar_splits_follow_real_month_boundaries(self):
        self.config['months'] = 3
        self.save()
        splits = self.query()['overview']['splits']
        self.assertEqual(splits, [
            {'name': 'train', 'start': 1767225600000, 'end': 1769904000000},
            {'name': 'validation', 'start': 1769904000000, 'end': 1772323200000},
            {'name': 'test', 'start': 1772323200000, 'end': 1775001600000}])

    def test_invalid_ranges_leads_and_snapshot_anchor_rejected(self):
        for args in ({'start': 'nan'}, {'start': -1}, {'start': 20, 'end': 10}, {'lead_steps': 0}, {'lead_steps': 1.5}, {'lead_steps': True}):
            with self.assertRaises(ValueError):
                self.query(**args)
        for anchor in (None, -1, 'inf', True):
            with self.assertRaises(ValueError):
                self.query('forecast', at=anchor)


if __name__ == '__main__':
    unittest.main()
