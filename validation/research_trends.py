"""Read-only chart DTOs from frozen simulation evidence, never controller recomputation."""
import bisect
import json
import math
from datetime import datetime
from pathlib import Path
from statistics import median


class ArchiveUnavailable(ValueError):
    pass


def finite(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def number(value, label):
    if isinstance(value, bool):
        raise ValueError(f'{label} must be a finite nonnegative number')
    try:
        result = float(value)
    except (TypeError, ValueError):
        raise ValueError(f'{label} must be a finite nonnegative number') from None
    if not math.isfinite(result) or result < 0:
        raise ValueError(f'{label} must be a finite nonnegative number')
    return result


def lead_steps(value):
    result = number(value, 'lead_steps')
    if not result.is_integer() or not 1 <= result <= 1000000:
        raise ValueError('lead_steps must be an integer from 1 to 1000000')
    return int(result)


class ForecastArchive:
    """Indexed NDJSON reader; snapshots seek only the requested recorded horizon."""
    def __init__(self, path, sample_time, cv_count, mv_count):
        self.path = Path(path)
        index_path = Path(str(path) + '.index.json')
        if not self.path.is_file() or not index_path.is_file():
            raise ArchiveUnavailable('Complete forecast archive unavailable for this run; old evidence is not reconstructed.')
        try:
            index = json.loads(index_path.read_text())
            with self.path.open('rb') as handle:
                line = handle.readline()
                self.header = json.loads(line)
            header = self.header
            self.offsets = index['offsets']
            if (index['schema_version'] != 1 or header['schema_version'] != 1 or index['header'] != header
                    or header['layout'] != 'step-major' or header['cv_first_offset_steps'] != 1
                    or header['mv_first_offset_steps'] != 0 or header['sample_time'] != sample_time
                    or header['cv_count'] != cv_count or header['mv_count'] != mv_count
                    or any(type(header[key]) is not int or header[key] < 1 for key in ('prediction_horizon', 'control_horizon'))
                    or not isinstance(self.offsets, list) or index['samples'] != len(self.offsets)):
                raise ValueError('inconsistent header/index')
            previous = len(line) - 1
            size = self.path.stat().st_size
            for offset in self.offsets:
                if type(offset) is not int or not previous < offset < size:
                    raise ValueError('invalid record offsets')
                previous = offset
        except (OSError, ValueError, KeyError, TypeError) as error:
            raise ArchiveUnavailable(f'Forecast archive corrupt or unreadable: {error}') from error

    def record(self, handle, position):
        try:
            offset = self.offsets[position]
            handle.seek(offset - 1)
            if handle.read(1) != b'\n':
                raise ValueError('record offset is not a line boundary')
            row = json.loads(handle.readline())
            h = self.header
            if (type(row['sample']) is not int or row['sample'] < 0
                    or not finite(row['issued_at_seconds']) or row['issued_at_seconds'] < 0):
                raise ValueError('invalid scan time/sample')
            for key, count in (('cv', h['prediction_horizon'] * h['cv_count']),
                               ('mv_base', h['mv_count']), ('mv_delta', h['control_horizon'] * h['mv_count'])):
                if not isinstance(row[key], list) or len(row[key]) != count or not all(finite(v) for v in row[key]):
                    raise ValueError(f'invalid {key} horizon')
            return row
        except (OSError, ValueError, KeyError, TypeError, IndexError) as error:
            raise ArchiveUnavailable(f'Forecast archive corrupt or unreadable: {error}') from error

    def at(self, seconds):
        with self.path.open('rb') as handle:
            low, high = 0, len(self.offsets)
            while low < high:
                middle = (low + high) // 2
                if self.record(handle, middle)['issued_at_seconds'] <= seconds:
                    low = middle + 1
                else:
                    high = middle
            if low == 0:
                raise ArchiveUnavailable('No recorded forecast was issued at or before this anchor.')
            return self.record(handle, low - 1)

    def rows(self):
        with self.path.open('rb') as handle:
            previous = -1
            for position in range(len(self.offsets)):
                row = self.record(handle, position)
                if row['issued_at_seconds'] <= previous:
                    raise ArchiveUnavailable('Forecast archive corrupt: scan times are not increasing.')
                previous = row['issued_at_seconds']
                yield row


def cell(trace, key, row, column):
    values = trace.get(key, [])
    if row >= len(values) or not isinstance(values[row], list) or column >= len(values[row]):
        return None
    value = values[row][column]
    return value if finite(value) else None


def quality(value, code):
    if value is None:
        return 'missing'
    if type(code) is not int or not 0 <= code <= 65535:
        return 'unknown'
    return {192: 'good', 64: 'uncertain', 0: 'bad'}.get(code & 192, 'unknown')


def compress(points, step=False, buckets=250):
    """Per-signal extrema plus both sides of all gaps, quality and step changes."""
    if len(points) <= buckets * 4:
        return points
    keep = {0, len(points) - 1}
    width = max(1, math.ceil(len(points) / buckets))
    for start in range(0, len(points), width):
        end = min(len(points), start + width)
        keep.update((start, end - 1))
        valid = [i for i in range(start, end) if points[i]['value'] is not None]
        if valid:
            keep.add(min(valid, key=lambda i: points[i]['value']))
            keep.add(max(valid, key=lambda i: points[i]['value']))
    for i in range(1, len(points)):
        before, current = points[i - 1], points[i]
        if (before['quality'] != current['quality'] or (before['value'] is None) != (current['value'] is None)
                or (step and before['value'] != current['value'])):
            keep.update((i - 1, i))
    return [points[i] for i in sorted(keep)]


def window_metrics(trace, times, indices, role, column, quality_column, variable, limits, dt, group_id):
    """Sample-weighted physical statistics, never statistics of display representatives."""
    result = {'seriesId': f'{group_id}:{role}:{column}:actual', 'variableId': f'{role}:{column}',
              'groupId': group_id, 'samples': len(indices), 'goodSamples': 0,
              'mean': None, 'variance': None, 'stddev': None, 'bias': None, 'biasSamples': 0,
              'specViolationSeconds': None, 'specObservedSeconds': 0,
              'operatingViolationSeconds': None, 'operatingObservedSeconds': 0,
              'commandMovement': None, 'commandPairs': 0}
    count, mean, m2, bias = 0, 0.0, 0.0, 0.0
    previous = None
    command_supported = role == 'mv' and 'command_mv' in trace
    spec = (variable.get('spec_low'), variable.get('spec_high'))
    spec_sides = [role == 'cv' and finite(v) for v in spec]
    operating_sides = [role != 'dv' and (f'{role}_{side}' in trace or finite(limits.get(side))) for side in ('low', 'high')]
    has_spec, has_operating = any(spec_sides), any(operating_sides)
    for i in indices:
        value = cell(trace, role, i, column)
        code = cell(trace, 'quality', i, quality_column)
        good = quality(value, code) == 'good'
        if command_supported:
            command = cell(trace, 'command_mv', i, column)
            valid = quality(command, code) == 'good'
            if valid and previous is not None and math.isclose(times[i] - previous[0], dt, rel_tol=1e-9, abs_tol=1e-9):
                if result['commandMovement'] is None:
                    result['commandMovement'] = 0.0
                result['commandMovement'] += abs(command - previous[1])
                result['commandPairs'] += 1
            previous = (times[i], command) if valid else None
        if not good:
            continue
        count += 1
        delta = value - mean
        mean += delta / count
        m2 += delta * (value - mean)
        target = cell(trace, 'targets', i, column) if role == 'cv' else None
        if target is not None:
            bias += value - target
            result['biasSamples'] += 1
        operating = tuple(cell(trace, f'{role}_{side}', i, column) if f'{role}_{side}' in trace else limits.get(side)
                          for side in ('low', 'high'))
        for prefix, bounds, supported in (('spec', spec, has_spec), ('operating', operating, has_operating)):
            if not supported:
                continue
            # A missing recorded side is unknown, not an implicit one-sided relaxation.
            configured = spec_sides if prefix == 'spec' else operating_sides
            if not all(not needed or finite(bound) for needed, bound in zip(configured, bounds)):
                continue
            result[prefix + 'ObservedSeconds'] += dt
            if result[prefix + 'ViolationSeconds'] is None:
                result[prefix + 'ViolationSeconds'] = 0.0
            if (finite(bounds[0]) and value < bounds[0]) or (finite(bounds[1]) and value > bounds[1]):
                result[prefix + 'ViolationSeconds'] += dt
    result['goodSamples'] = count
    if count:
        result.update(mean=mean, variance=max(0.0, m2 / count), stddev=math.sqrt(max(0.0, m2 / count)))
    if result['biasSamples']:
        result['bias'] = bias / result['biasSamples']
    return result


def overview_series(trace, times, role, column, quality_column, dt, origin, group_id, label):
    """Temporal buckets retain real Good observations only; absent scans stay empty."""
    points = []
    if times:
        width = max(dt, math.ceil((times[-1] - times[0] + dt) / (250 * dt)) * dt)
        count = min(250, math.ceil((times[-1] - times[0] + dt) / width))
        values = [[] for _ in range(count)]
        counts = [0] * count
        for i, seconds in enumerate(times):
            bucket = min(count - 1, int((seconds - times[0]) / width))
            counts[bucket] += 1
            value = cell(trace, role, i, column)
            if quality(value, cell(trace, 'quality', i, quality_column)) == 'good':
                values[bucket].append(value)
        for i, samples in enumerate(values):
            first = origin + (times[0] + i * width) * 1000
            last = origin + min(times[0] + (i + 1) * width, times[-1] + dt) * 1000
            points.append({'time': (first + last) / 2, 'start': first, 'end': last,
                           'min': min(samples) if samples else None, 'max': max(samples) if samples else None,
                           'median': median(samples) if samples else None, 'count': counts[i], 'goodCount': len(samples)})
    return {'seriesId': f'{group_id}:{role}:{column}:actual', 'variableId': f'{role}:{column}',
            'groupId': group_id, 'label': label, 'points': points}


def overview_splits(run, config, origin, dt):
    recorded = (run.get('results') or {}).get('splits')
    if recorded is None and (config.get('data_window') is not None or config.get('months') == 3):
        # Reuse the actual study calendar/data-window policy, including month lengths.
        from research_design import study_splits
        recorded = study_splits(config)
    return [{'name': row['name'], 'start': origin + row['first'] * dt * 1000,
             'end': origin + row['last'] * dt * 1000} for row in (recorded or [])]


def process_trends(directory, run, method, variant, payload, snapshot=False, *, frozen_source=None):
    """Convert an explicitly resolved frozen trajectory; callers own artifact whitelisting."""
    directory = Path(directory)
    stem = f'{method}-{variant}'
    path = Path(frozen_source['result']) if frozen_source else directory / (stem + '.json')
    if not path.is_file():
        raise ValueError('This trajectory has not completed yet')
    trace = json.loads(path.read_text())['trace']
    study = run['study']
    config = study['config']
    dt = config['sample_time']
    if not finite(dt) or dt <= 0:
        raise ValueError('Invalid recorded sample time')
    times = trace['time_seconds']
    if any(not finite(t) or t < 0 for t in times) or any(b <= a for a, b in zip(times, times[1:])):
        raise ValueError('Invalid recorded trace timestamps')
    relative = config['start'] == 'relative'
    origin = 0
    if not relative:
        recorded_start = datetime.fromisoformat(config['start'].replace('Z', '+00:00'))
        if recorded_start.tzinfo is None:
            raise ValueError('Recorded scenario start must include its UTC offset')
        origin = recorded_start.timestamp() * 1000
    start = number(payload.get('start') if payload.get('start') is not None else (times[0] if times else 0), 'start')
    end = number(payload.get('end') if payload.get('end') is not None else (times[-1] if times else start), 'end')
    if end < start:
        raise ValueError('Invalid chart time window: end precedes start')
    lead = lead_steps(payload.get('lead_steps', 1))
    anchor = number(payload.get('at'), 'at') if snapshot else None
    groups = config['variables']
    warnings = []
    source = 'Simulated COM DA' if variant == 'transport' else 'Simulated direct'
    if frozen_source is not None:
        source = frozen_source.get('source', source)
    group_id = f'{method}:{variant}'
    data = {'schemaVersion': 1, 'timeBasis': 'relative' if relative else 'utc',
            'originTime': origin, 'variables': [], 'series': [], 'events': [],
            'range': {'start': origin + start * 1000, 'end': origin + end * 1000},
            'sampleTimeMs': dt * 1000, 'rawSamples': len(times), 'displaySamples': 0,
            'warnings': warnings, 'provenance': f'{source}; frozen local run {run["id"]}; not factory measurements. Display-only extrema reduction; metrics use full traces.'}
    data['groups'] = [{'id': group_id, 'label': f'{method} · {variant}', 'method': method, 'variant': variant}]
    data['overview'] = {'range': {'start': origin + (times[0] if times else 0) * 1000,
                                  'end': origin + (times[-1] + dt if times else 0) * 1000},
                        'series': [], 'splits': overview_splits(run, config, origin, dt)}
    if not snapshot:
        data['windowSummary'] = {'range': dict(data['range']), 'rows': [],
            'basis': '完整解析度、視窗內（含兩端）Good 有限實際樣本；平均與母體變異數／標準差採樣本等權，偏差僅配對同時目標。'
                     '違規與涵蓋秒數為可比較樣本數 × 掃描週期（非插值持續時間，末樣本亦計一週期）；缺規格或無可比較樣本時違規秒數為 null。'
                     'MV 命令移動量為視窗內相鄰且相隔一掃描週期、兩端 Good 命令之絕對差總和，不跨缺值／品質中斷；'
                     '無有效相鄰配對時為 null，有效不變命令配對才可為零。'}
    indices = range(bisect.bisect_left(times, start), bisect.bisect_right(times, end))

    def point(seconds, value, state='good', issued=None):
        result = {'time': origin + seconds * 1000, 'value': value if finite(value) else None,
                  'quality': state if finite(value) else 'missing'}
        if issued is not None:
            result['issuedAt'] = origin + issued * 1000
        return result

    def add(variable, kind, points, unavailable=None, provenance=None, step=False, lead_seconds=None, reduce=True):
        if not points and unavailable is None:
            unavailable = 'No recorded samples are available in this time window.'
        if snapshot and step and points and kind not in ('command', 'plan'):
            points = [*points, {**points[-1], 'time': data['range']['end']}]
            provenance = (provenance or source) + '; anchor reference held across forecast, not future observations'
        points = compress(points, step and kind not in ('command', 'plan')) if reduce else points
        row = {'id': f'{method}:{variant}:{variable}:{kind}', 'variableId': variable, 'groupId': group_id,
               'label': f'{method} · {variant}' + (' · 起算時參考' if snapshot and step and kind not in ('command', 'plan') else ''),
               'kind': kind, 'points': points,
               'source': provenance or source, 'method': method, 'interpolation': 'step' if step else 'linear',
               'available': unavailable is None}
        if unavailable:
            row['unavailableReason'] = unavailable
        if lead_seconds is not None:
            row['leadSeconds'] = lead_seconds
        data['series'].append(row)

    def history(key, column, quality_column=None, fixed=None, transform=None, observations=False):
        points = []
        previous = None
        selected = range(bisect.bisect_left(times, anchor), bisect.bisect_right(times, finish)) if observations and snapshot and archive else indices
        for i in selected:
            t = times[i]
            if previous is not None and t - previous > dt * 1.5:
                points.append(point(previous + dt, None))
            value = cell(trace, key, i, column) if fixed is None else fixed
            if transform is not None and value is not None:
                value = transform(value)
            code = cell(trace, 'quality', i, quality_column) if quality_column is not None else 192
            points.append(point(t, value, quality(value, code)))
            previous = t
        return points

    def events(points, kind, variable):
        for before, current in zip(points, points[1:]):
            if before['value'] is not None and current['value'] is not None and before['value'] != current['value']:
                data['events'].append({'time': current['time'], 'kind': kind, 'variableId': variable, 'groupId': group_id,
                                       'label': f'{variable} {kind}: {before["value"]} → {current["value"]}'})

    time_indices = {origin + seconds * 1000: i for i, seconds in enumerate(times)}

    def observation_events(points, role, column, variable, limits):
        previous_quality = None
        spec = [variable.get('spec_' + side) for side in ('low', 'high')]
        configured = {'品質規格': [role == 'cv' and finite(v) for v in spec],
                      '操作限制': [role != 'dv' and (f'{role}_{side}' in trace or finite(limits.get(side)))
                                   for side in ('low', 'high')]}
        states = {label: None for label, sides in configured.items() if any(sides)}
        quality_labels = {'good': '良好', 'bad': '不良', 'uncertain': '不確定', 'unknown': '未知', 'missing': '缺值'}
        for p in points:
            if p['quality'] != previous_quality:
                if previous_quality is not None or p['quality'] != 'good':
                    data['events'].append({'time': p['time'], 'kind': 'quality', 'variableId': f'{role}:{column}',
                        'groupId': group_id, 'label': f'{variable["name"]} 品質：{quality_labels.get(previous_quality, "起始")} → {quality_labels[p["quality"]]}'})
                previous_quality = p['quality']
            i = time_indices.get(p['time'])
            for label in states:
                needed = configured[label]
                if label == '品質規格':
                    bounds = spec
                else:
                    bounds = [cell(trace, f'{role}_{side}', i, column) if f'{role}_{side}' in trace and i is not None
                              else limits.get(side) for side in ('low', 'high')]
                if (p['quality'] != 'good'
                        or not all(not required or finite(bound) for required, bound in zip(needed, bounds))):
                    states[label] = None
                    continue
                violated = ((needed[0] and p['value'] < bounds[0]) or (needed[1] and p['value'] > bounds[1]))
                if (violated and states[label] is not True) or (not violated and states[label] is True):
                    data['events'].append({'time': p['time'], 'kind': 'violation', 'variableId': f'{role}:{column}',
                        'groupId': group_id, 'label': f'{variable["name"]} {label}：{"超限" if violated else "恢復範圍內"}'})
                states[label] = violated

    # The receipt identifies the exact immutable controller used by this trajectory.
    # Old runs without a receipt retain the frozen engineering configuration as source.
    controller = None
    receipt_path = directory / (stem + '.command.json')
    if frozen_source is not None:
        controller = frozen_source.get('controller')
    elif receipt_path.is_file():
        receipt = json.loads(receipt_path.read_text())
        argv = receipt.get('argv', [])
        if '--controller' in argv and argv.index('--controller') + 1 < len(argv):
            controller_path = directory / Path(argv[argv.index('--controller') + 1]).name
            if controller_path.is_file():
                controller = json.loads(controller_path.read_text())

    archive = None
    archive_error = None
    archived = []
    if snapshot or lead > 1:
        try:
            archive_path = Path(frozen_source['forecast']) if frozen_source else directory / (stem + '.forecast.ndjson')
            archive = ForecastArchive(archive_path, dt, len(groups['cv']), len(groups['mv']))
            if not snapshot and lead > archive.header['prediction_horizon']:
                raise ArchiveUnavailable('Requested lead exceeds the recorded prediction horizon.')
            if snapshot:
                archived = [archive.at(anchor)]
            else:
                count = len(groups['cv'])
                archived = [{'issued_at_seconds': row['issued_at_seconds'],
                             'cv': row['cv'][(lead - 1) * count:lead * count]}
                            for row in archive.rows()
                            if times and start <= row['issued_at_seconds'] + lead * dt <= min(end, times[-1])]
        except (ArchiveUnavailable, OSError) as error:
            archive_error = str(error)
            warnings.append(archive_error)
            archive = None
    if snapshot and archive:
        anchor = archived[0]['issued_at_seconds']
        finish = anchor + max(archive.header['prediction_horizon'], archive.header['control_horizon'] - 1) * dt
        data['range'] = {'start': origin + anchor * 1000, 'end': origin + finish * 1000}
        anchor_index = bisect.bisect_left(times, anchor)
        indices = [anchor_index] if anchor_index < len(times) and times[anchor_index] == anchor else []
    elif snapshot:
        data['range'] = {'start': origin + anchor * 1000, 'end': origin + anchor * 1000}
        indices = []

    quality_column = 0
    for role in ('cv', 'mv', 'dv'):
        for column, variable in enumerate(groups[role]):
            variable_id = f'{role}:{column}'
            data['variables'].append({'id': variable_id, 'role': role.upper(), 'name': variable['name'],
                                      'unit': variable.get('unit') or 'unknown'})
            actual = history(role, column, quality_column, observations=True)
            add(variable_id, 'actual', actual)
            data['overview']['series'].append(overview_series(trace, times, role, column, quality_column,
                                                              dt, origin, group_id, variable['name']))
            if role == 'mv':
                commands = history('command_mv', column, quality_column, observations=True)
                add(variable_id, 'command', commands, step=True,
                    provenance=f'{source} recorded controller command; quality gated by corresponding MV channel, not MV process value')
                movements = []
                previous = None
                for p in commands:
                    value = None
                    if (previous is not None and previous['quality'] == p['quality'] == 'good'
                            and math.isclose(p['time'] - previous['time'], dt * 1000, rel_tol=1e-9, abs_tol=1e-6)):
                        value = p['value'] - previous['value']
                    movements.append({**p, 'value': value, 'quality': 'good' if value is not None else 'missing'})
                    previous = p
                movement_reason = None if any(p['quality'] == 'good' for p in movements) else 'No consecutive Good command pair with corresponding MV quality is available in this window.'
                add(variable_id, 'movement', movements if movement_reason is None else [], unavailable=movement_reason,
                    provenance=f'{source} signed consecutive recorded command delta; no crossing missing scans or non-Good MV quality')
            limits = variable
            if controller:
                limits = controller['variables'][role + 's'][column]['limits']
            if not snapshot:
                data['windowSummary']['rows'].append(window_metrics(trace, times, indices, role, column, quality_column,
                                                                    variable, limits, dt, group_id))
            observation_events(actual, role, column, variable, limits)
            quality_column += 1
            for side in ('low', 'high'):
                key = f'{role}_{side}'
                recorded = key in trace
                points = history(key, column, fixed=None if recorded else limits.get(side))
                kind = ('valid_' if role == 'dv' else 'operating_') + side
                events(points, 'limit', variable_id)
                add(variable_id, kind, points, provenance=f'{source} recorded limit history' if recorded else 'Frozen run controller limits' if controller else 'Frozen run engineering limits', step=True)
            if role == 'cv':
                targets = history('targets', column)
                events(targets, 'target', variable_id)
                add(variable_id, 'target', targets, step=True)
                for side in ('low', 'high'):
                    value = variable.get('spec_' + side)
                    add(variable_id, 'spec_' + side, history('', column, fixed=value) if value is not None else [],
                        unavailable=None if value is not None else 'No display-only quality specification was configured.',
                        provenance='Frozen study display-only quality specification; not a controller constraint', step=True)
                if method == 'soft_band':
                    half_width = study['research']['bands'][column]['half_width']
                    for side, sign in (('low', -1), ('high', 1)):
                        add(variable_id, 'soft_' + side, history('targets', column, transform=lambda value, sign=sign: value + sign * half_width),
                            provenance='Frozen controller soft band around recorded target; not quality specification', step=True)
                predictions = []
                if archive:
                    for row in archived:
                        steps = range(1, archive.header['prediction_horizon'] + 1) if snapshot else [lead]
                        for step in steps:
                            valid = row['issued_at_seconds'] + step * dt
                            if snapshot or (times and start <= valid <= min(end, times[-1])):
                                value_index = (step - 1) * len(groups['cv']) + column if snapshot else column
                                predictions.append(point(valid, row['cv'][value_index], issued=row['issued_at_seconds']))
                elif not snapshot and lead == 1 and not archive_error:
                    for i, issued in enumerate(times):
                        valid = issued + dt
                        if start <= valid <= min(end, times[-1]):
                            predictions.append(point(valid, cell(trace, 'predicted_cv', i, column), issued=issued))
                reason = archive_error
                if not any(p['value'] is not None for p in predictions) and not reason:
                    reason = 'No recorded forecast is available in this time window.'
                # Missing issue scans remain visible gaps instead of joining distant predictions.
                gapped = []
                for p in predictions:
                    if gapped and p['time'] - gapped[-1]['time'] > dt * 1500:
                        gapped.append({'time': gapped[-1]['time'] + dt * 1000, 'value': None, 'quality': 'missing'})
                    gapped.append(p)
                add(variable_id, 'prediction', gapped if not reason else [], unavailable=reason,
                    provenance=f'{source} archived solver horizon' if archive else f'{source} recorded one-step forecast (shifted to valid time)',
                    lead_seconds=None if snapshot else lead * dt, reduce=not snapshot)
                actual_by_time = {p['time']: p for p in actual}
                residuals = []
                for p in gapped:
                    observed = actual_by_time.get(p['time'])
                    valid = observed is not None and observed['quality'] == p['quality'] == 'good'
                    residuals.append({**p, 'value': observed['value'] - p['value'] if valid else None,
                                      'quality': 'good' if valid else 'missing'})
                add(variable_id, 'residual', residuals if not reason else [], unavailable=reason,
                    provenance=f'{source} Good actual minus same-group forecast at exact valid time; no interpolation',
                    lead_seconds=None if snapshot else lead * dt, reduce=not snapshot)
            elif role == 'dv':
                add(variable_id, 'prediction', [], unavailable='DV forecasts are not recorded; measured disturbances are not forecasts.')
            if role == 'mv':
                plan = []
                if snapshot and archive:
                    row = archived[0]
                    value = row['mv_base'][column]
                    for step in range(archive.header['control_horizon']):
                        value += row['mv_delta'][step * len(groups['mv']) + column]
                        plan.append(point(anchor + step * dt, value, issued=anchor))
                add(variable_id, 'plan', plan, unavailable=None if plan else archive_error or 'Select a recorded forecast snapshot to view the complete MV plan.', step=True, reduce=False)
    if not snapshot:
        for i in indices:
            if i and times[i] - times[i - 1] > dt * 1.5:
                gap_time = origin + (times[i - 1] + dt) * 1000
                if data['range']['start'] <= gap_time <= data['range']['end']:
                    data['events'].append({'time': gap_time, 'kind': 'gap', 'groupId': group_id, 'label': '缺少記錄掃描'})
    data['events'].sort(key=lambda event: event['time'])
    data['displaySamples'] = len({p['time'] for row in data['series'] for p in row['points']})
    return data
