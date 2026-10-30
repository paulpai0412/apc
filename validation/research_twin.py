"""Immutable local twin inputs and archived charts. Native execution belongs to HMI."""
import copy
import hashlib
import json
import os
from datetime import datetime, timedelta
from pathlib import Path
import uuid

from common import load_json
from research_trends import process_trends
from research_trends import ForecastArchive, finite
from twin_equipment import (configuration, create_version, effective_variables, encoded,
                            load_version, validate_view)

ACTIVE = ('starting', 'paused', 'running')
TERMINAL = ('completed', 'failed', 'closed', 'interrupted')
MAX_INPUT_BYTES = 512 * 1024 * 1024
MAX_FORECAST_BYTES = 16 * 1024 * 1024 * 1024
MAX_HORIZON_BYTES = 16 * 1024 * 1024
MAX_SAMPLES = 2_000_000
DYNAMIC_FIELDS = {'status', 'samples', 'frame', 'recent_trace', 'latest_forecast', 'reason', 'error', 'updated_at', 'result_sha256', 'forecast_hashes',
                  'execution_status', 'sequence', 'native_sequence', 'batch_steps', 'operation', 'operations', 'pending_native',
                  'trace_journal_sha256', 'trace_journal_bytes'}


def artifact(directory, name):
    path = directory / name
    if path.is_symlink() or path.resolve().parent != directory.resolve() or not path.is_file():
        raise ValueError('Missing or unsafe frozen artifact: ' + name)
    maximum = MAX_FORECAST_BYTES if name == 'forecast.ndjson' or name.endswith(('.forecast.ndjson', '.trace.ndjson')) else MAX_INPUT_BYTES
    if path.stat().st_size > maximum:
        raise ValueError('Frozen artifact exceeds twin input limit')
    return path


def fingerprint(path):
    # A full-quarter forecast can exceed memory; integrity checks stay streaming.
    digest = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def trace_evidence(directory):
    path = directory / 'result.json.trace.ndjson'
    if not path.exists() and not path.is_symlink():
        return {}
    path = artifact(directory, path.name)
    return {'trace_journal_sha256': fingerprint(path), 'trace_journal_bytes': path.stat().st_size}


def selected_trial(root, run_id, trial_id):
    from research import load_run, run_dir, safe_id
    safe_id(trial_id)
    directory = run_dir(root, run_id)
    if directory.is_symlink() or directory.parent.is_symlink():
        raise ValueError('Unsafe run artifact directory')
    run = load_run(root, run_id)
    trial = next((t for t in run.get('trials', []) if t['id'] == trial_id), None)
    if trial is None or trial.get('valid') is not True:
        raise ValueError('Choose a valid trial recorded in this run manifest')
    # Never interpret a manifest field as a free-form filesystem path.
    if trial.get('controller') != trial_id + '-controller.json':
        raise ValueError('Trial controller does not match the recorded trial identity')
    controller = artifact(directory, trial['controller'])
    receipt_path = directory / (trial_id + '.command.json')
    if receipt_path.exists():
        receipt = load_json(artifact(directory, receipt_path.name))
        for field, path in [('controller_sha256', controller),
                            ('plant_sha256', artifact(directory, 'plant.json')),
                            ('scenario_sha256', artifact(directory, 'selection-scenario.json'))]:
            if receipt.get(field) and receipt[field] != fingerprint(path):
                raise ValueError('Recorded trial input integrity mismatch: ' + field)
    return run, directory, trial, controller


def scenario_context(run, scenario):
    config = run['study']['config']
    first = scenario.get('provenance', {}).get('source_first_sample', 0)
    if type(first) is not int or first < 0 or not finite(config['sample_time']) or config['sample_time'] <= 0:
        raise ValueError('Invalid recorded scenario source offset')
    offset = first * config['sample_time']
    start = config['start']
    if start != 'relative':
        date = datetime.fromisoformat(start.replace('Z', '+00:00'))
        if date.tzinfo is None:
            raise ValueError('Scenario start must include a UTC offset')
        start = (date + timedelta(seconds=offset)).isoformat().replace('+00:00', 'Z')
    return start, offset


def compatible(run, controller, plant, scenario):
    config = run['study']['config']
    dt = config['sample_time']
    if any(value != dt for value in (controller['tuning']['sample_time'], plant['sample_time'], scenario['sample_time'])):
        raise ValueError('Controller, plant and scenario sample times differ')
    for role in ('cv', 'mv', 'dv'):
        variables = config['variables'][role]
        count = len(variables)
        model_variables = controller['variables'].get(role + 's', [])
        if len(model_variables) != count or len(plant['baseline_' + role]) != count:
            raise ValueError('Frozen variable contracts differ: ' + role)
        if any(model.get('name') != variable['name'] or model.get('units') != variable['unit']
               for model, variable in zip(model_variables, variables)):
            raise ValueError('Frozen variable names, order or engineering units differ: ' + role)
        if any(len(row) != count for row in scenario[role]):
            raise ValueError('Frozen scenario variable dimensions differ: ' + role)
    times = scenario['time_seconds']
    if not times or len(times) > MAX_SAMPLES:
        raise ValueError('Twin scenario must contain 1..2000000 scans')
    if any(not finite(t) or t < 0 for t in times) or any(b <= a for a, b in zip(times, times[1:])):
        raise ValueError('Invalid frozen scenario timestamps')
    if any(len(scenario[role]) != len(times) for role in ('cv', 'mv', 'dv')):
        raise ValueError('Frozen scenario lengths differ')


def session_dir(root, session_id):
    from research import safe_id
    directory = root / 'twin-sessions' / safe_id(session_id)
    if directory.is_symlink() or directory.parent.is_symlink():
        raise ValueError('Unsafe twin session directory')
    return directory


def public(record, summary=False):
    hidden = {'owner', 'study', 'input_hashes', 'operations'}
    if summary:
        hidden |= {'frame', 'recent_trace', 'latest_forecast', 'equipment_view', 'controller_variables'}
    return {k: v for k, v in record.items() if k not in hidden}


def verify_frozen_inputs(directory, record):
    for name, digest in record['input_hashes'].items():
        if fingerprint(artifact(directory, name)) != digest:
            raise ValueError('Frozen twin inputs changed during execution')


def get_session(root, session_id, owner):
    # This is deliberately read-only, including after an owner restart.
    # Startup reconciliation is an explicit server lifecycle operation.
    directory = session_dir(root, session_id)
    record = load_json(artifact(directory, 'session.json'))
    frozen = load_json(artifact(directory, 'manifest.json'))
    if set(record) - set(frozen) - DYNAMIC_FIELDS or any(record.get(key) != value for key, value in frozen.items() if key not in DYNAMIC_FIELDS):
        raise ValueError('Twin immutable manifest differs from persisted state')
    if record.get('latest_forecast') is not None:
        verify_frozen_inputs(directory, record)
        validate_native_state(record, record['samples'], load_json(artifact(directory, 'controller.json')),
                              load_json(artifact(directory, 'scenario.json')))
    return record


def manifest(root, payload):
    from research import load_run, run_dir
    run = load_run(root, payload['id'])
    directory = run_dir(root, payload['id'])
    config = run['study']['config']
    plant = load_json(artifact(directory, 'plant.json')) if (directory / 'plant.json').exists() else None
    trials = []
    for trial in run.get('trials', []):
        row = dict(trial, diagnostic_only=not trial.get('eligible', False))
        if trial.get('valid'):
            try:
                _, _, _, controller = selected_trial(root, run['id'], trial['id'])
                row['controller_sha256'] = fingerprint(controller)
            except (ValueError, OSError) as error:
                row.update(valid=False, reason=str(error))
        trials.append(row)
    def optional(name):
        return load_json(artifact(directory, name)) if (directory / name).exists() else None
    identification = optional('identification.json') or optional('identification-development.json')
    selected = {m['method']: m['selected_trial'] for m in (run.get('results') or {}).get('methods', [])}
    selection = optional('selection.json')
    if selection:
        selected.update({method: row['best']['id'] for method, row in selection.get('winners', {}).items()})
    sessions = []
    for path in sorted((root / 'twin-sessions').glob('*/session.json')):
        record = get_session(root, path.parent.name, payload['owner'])
        if record['run_id'] == run['id']:
            sessions.append(public(record, summary=True))
    return dict(run_id=run['id'], scope=run.get('scope', config.get('plant_source')),
                run_status=run['status'], stage=run.get('stage', ''), variables=config['variables'],
                sample_time=config['sample_time'], start=config['start'],
                time_basis='relative' if config['start'] == 'relative' else 'utc', plant_source=config.get('plant_source'),
                plant_paths=plant['paths'] if plant else [], qualification=optional('plant-evidence.json'), identification=identification,
                trials=trials, selected_trials=selected, sessions=sessions)


def create_session(root, payload):
    from research import now, runtime_settings
    from twin_session_state import save_state
    required = {'run_id', 'trial_id', 'scenario', 'transport', 'owner'}
    if not required <= set(payload) or set(payload) - required - {'equipment_view_id', 'settings_version_id'}:
        raise ValueError('Twin creation accepts only scenario/transport and server-owned version IDs')
    for key in ('equipment_view_id', 'settings_version_id'):
        if key in payload and (not isinstance(payload[key], str) or len(payload[key]) != 64
                               or any(c not in '0123456789abcdef' for c in payload[key])):
            raise ValueError('Invalid immutable twin version ID')
    if payload['scenario'] not in ('selection', 'full') or payload['transport'] not in ('direct', 'opc-da'):
        raise ValueError('Unknown twin scenario or transport')
    run, source, trial, controller_path = selected_trial(root, payload['run_id'], payload['trial_id'])
    runtime = runtime_settings()  # Administrator configuration, never archived command argv.
    scenario_name = 'scenario.json' if payload['scenario'] == 'full' else 'selection-scenario.json'
    sources = {'controller.json': controller_path, 'plant.json': artifact(source, 'plant.json'),
               'scenario.json': artifact(source, scenario_name)}
    frozen = {name: path.read_bytes() for name, path in sources.items()}
    controller, plant, scenario = (json.loads(frozen[name]) for name in ('controller.json', 'plant.json', 'scenario.json'))
    original_controller = controller
    source_digest = fingerprint(controller_path)
    settings = None
    view = None
    if payload.get('settings_version_id') is not None:
        settings = load_version(root, 'settings', payload['settings_version_id'], run['id'], trial['id'], source_digest)
        controller = settings['controller']
        frozen['controller.json'] = encoded(controller)
        frozen['settings-version.json'] = encoded(settings)
        changes = [(i, cv['limits']['target']) for i, cv in enumerate(controller['variables']['cvs'])
                   if cv['limits']['target'] != original_controller['variables']['cvs'][i]['limits']['target']]
        if changes:
            targets = scenario.get('targets')
            if not targets:
                targets = [[cv['limits']['target'] for cv in original_controller['variables']['cvs']]
                           for _ in scenario['time_seconds']]
            if len(targets) != len(scenario['time_seconds']) or any(len(row) != len(controller['variables']['cvs']) for row in targets):
                raise ValueError('Frozen scenario target dimensions differ')
            for row in targets:
                for index, target in changes:
                    row[index] = target
            scenario['targets'] = targets
            frozen['scenario.json'] = encoded(scenario)
    if payload.get('equipment_view_id') is not None:
        view = load_version(root, 'equipment-views', payload['equipment_view_id'], run['id'], trial['id'], source_digest)
        validate_view(view['view'], controller)
        frozen['equipment-view.json'] = encoded(view)
    compatible(run, controller, plant, scenario)
    start, offset = scenario_context(run, scenario)
    session_id = uuid.uuid4().hex
    directory = session_dir(root, session_id)
    directory.mkdir(parents=True, exist_ok=False)
    hashes = {}
    for name, content in frozen.items():
        with (directory / name).open('xb') as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        hashes[name] = hashlib.sha256(content).hexdigest()
    study = copy.deepcopy(run['study'])
    study['config']['start'] = start
    study['config']['variables'] = effective_variables(run, controller)
    record = dict(id=session_id, run_id=run['id'], trial_id=trial['id'], method=trial['method'],
                  scenario=payload['scenario'], transport=payload['transport'], status='starting', samples=0,
                  execution_status='starting', sequence=0, native_sequence=0, batch_steps=128,
                  operation=None, operations={}, pending_native=None,
                  requested_samples=len(scenario['time_seconds']), sample_time=scenario['sample_time'], start=start,
                  source_offset_seconds=offset, time_basis='relative' if start == 'relative' else 'utc',
                  created_at=now(), owner=payload['owner'], study=study,
                  diagnostic_only=settings is not None or not trial.get('eligible', False), scope=run.get('scope'),
                  exploratory=payload['scenario'] == 'full', frame=None, recent_trace=None, latest_forecast=None,
                  equipment_view=view, settings_version_id=settings['id'] if settings else None,
                  variables=study['config']['variables'], controller_variables=controller['variables'],
                  controller_sha256=hashes['controller.json'], plant_sha256=hashes['plant.json'],
                  scenario_sha256=hashes['scenario.json'], binary_sha256=fingerprint(runtime['binary']),
                  bridge_sha256=fingerprint(runtime['bridge']) if payload['transport'] == 'opc-da' else None,
                  input_hashes=hashes)
    save_state(directory / 'manifest.json', record)
    save_state(directory / 'session.json', record)
    return public(record)


def update_session(root, payload):
    from twin_session_state import commit, settle_native
    directory = session_dir(root, payload['id'])
    record = get_session(root, payload['id'], payload['owner'])
    previous_samples = record['samples']
    if record.get('owner') != payload['owner'] or record['status'] not in ACTIVE:
        raise ValueError('Session is not owned by this active HMI')
    response = payload['response']
    allowed = {'request_id', 'status', 'samples', 'requested_samples', 'sample_time',
               'reason', 'error', 'frame', 'recent_trace', 'latest_forecast'}
    if not isinstance(response, dict) or set(response) - allowed:
        raise ValueError('Invalid or unsupported native response fields')
    for key in ('reason', 'error'):
        if response.get(key) is not None and (not isinstance(response[key], str) or len(response[key]) > 4096):
            raise ValueError('Invalid bounded native diagnostic message')
    status = response.get('status')
    samples = response.get('samples')
    if status not in ('paused', 'completed', 'failed', 'closed', 'interrupted') or type(samples) is not int or not record['samples'] <= samples <= record['requested_samples']:
        raise ValueError('Invalid native session state')
    if (type(response.get('requested_samples', record['requested_samples'])) is not int
            or not finite(response.get('sample_time', record['sample_time']))
            or response.get('requested_samples', record['requested_samples']) != record['requested_samples']
            or response.get('sample_time', record['sample_time']) != record['sample_time']):
        raise ValueError('Native scenario identity differs from frozen manifest')
    verify_frozen_inputs(directory, record)
    if status == 'completed':
        result = load_json(artifact(directory, 'result.json'))
        if result.get('status') != 'valid' or samples != record['requested_samples'] or len(result.get('trace', {}).get('time_seconds', [])) != samples:
            raise ValueError('Native completion does not have a complete valid archive')
        if result['trace']['time_seconds'] != load_json(artifact(directory, 'scenario.json'))['time_seconds']:
            raise ValueError('Native completion timeline differs from frozen scenario')
        record['result_sha256'] = fingerprint(directory / 'result.json')
        record['forecast_hashes'] = forecast_hashes(directory, 'forecast.ndjson')
        if record.get('pending_native') is not None:
            if not record['forecast_hashes']:
                raise ValueError('Native completion is missing its full forecast archive')
            with (directory / 'forecast.ndjson').open('rb') as stream:
                if len(stream.readline(8193)) > 8192:
                    raise ValueError('Native forecast header exceeds bounded schema')
            archive = ForecastArchive(directory / 'forecast.ndjson', record['sample_time'],
                                      len(record['controller_variables']['cvs']), len(record['controller_variables']['mvs']))
            if len(archive.offsets) != samples:
                raise ValueError('Native forecast archive does not cover every completed scan')
    controller = load_json(artifact(directory, 'controller.json'))
    scenario = load_json(artifact(directory, 'scenario.json'))
    validate_native_state(response, samples, controller, scenario)
    if record.get('pending_native') is not None and samples and status in ('paused', 'completed'):
        if response.get('frame') is None or response.get('latest_forecast') is None:
            raise ValueError('Confirmed native scans require their exact frame and full forecast')
    if record.get('pending_native') is not None and samples > record['samples'] and response.get('frame') is None:
        raise ValueError('Advancing known samples requires a matching confirmed frame')
    settle_native(record, response)
    for key in ('reason', 'error'):
        record.pop(key, None)
    for key in ('samples', 'reason', 'error'):
        if key in response:
            record[key] = response[key]
    for key in ('frame', 'recent_trace', 'latest_forecast'):
        # Failed/unknown outcomes retain the last confirmed state, not invented null scans.
        if response.get(key) is not None:
            record[key] = response[key]
        elif samples != previous_samples:
            record[key] = None  # Legacy archives must not attach an older frame to a newer sample.
    if status in TERMINAL:
        record.update(trace_evidence(directory))
    commit(root, record)
    return public(record)


def forecast_hashes(directory, name):
    names = (name, name + '.index.json')
    present = [(directory / filename).exists() or (directory / filename).is_symlink() for filename in names]
    if not any(present):
        return {}
    if not all(present):
        raise ValueError('Incomplete recorded forecast archive')
    return {filename: fingerprint(artifact(directory, filename)) for filename in names}


def validate_forecast(value, controller, sample, seconds):
    if value is None:
        return
    tuning = controller['tuning']
    nc, nm = (len(controller['variables'][role]) for role in ('cvs', 'mvs'))
    expected = dict(schema_version=1, sample=sample, issued_at_seconds=seconds,
                    sample_time=tuning['sample_time'], cv_count=nc, mv_count=nm,
                    prediction_horizon=tuning['prediction_horizon'], control_horizon=tuning['control_horizon'],
                    layout='step-major', cv_first_offset_steps=1, mv_first_offset_steps=0)
    if not isinstance(value, dict) or set(value) != set(expected) | {'cv', 'mv_base', 'mv_delta'}:
        raise ValueError('Invalid native forecast fields')
    if any(value.get(key) != expected_value or type(value.get(key)) is bool for key, expected_value in expected.items()):
        raise ValueError('Native forecast identity/time differs from frozen frame')
    if any(type(value[key]) is not int for key in ('schema_version', 'sample', 'cv_count', 'mv_count', 'prediction_horizon', 'control_horizon', 'cv_first_offset_steps', 'mv_first_offset_steps')):
        raise ValueError('Forecast dimensions must be integers')
    for key, count in (('cv', nc * tuning['prediction_horizon']), ('mv_base', nm),
                       ('mv_delta', nm * tuning['control_horizon'])):
        if count > 200_000 or not isinstance(value[key], list) or len(value[key]) != count or not all(finite(v) for v in value[key]):
            raise ValueError('Invalid or oversized native forecast horizon')


def validate_frame(frame, controller, seconds):
    if not isinstance(frame, dict) or frame.get('time_seconds') != seconds or not finite(frame.get('time_seconds')):
        raise ValueError('Native frame clock differs from frozen scenario')
    if set(frame) != {'time_seconds', 'cv', 'mv', 'dv', 'targets', 'command_mv', 'predicted_cv', 'quality', 'solver_status'}:
        raise ValueError('Invalid native frame fields')
    nc, nm, nd = (len(controller['variables'].get(role, [])) for role in ('cvs', 'mvs', 'dvs'))
    if nc + nm + nd > 4096:
        raise ValueError('Native frame exceeds bounded variable count')
    for key, count in (('cv', nc), ('mv', nm), ('dv', nd), ('targets', nc),
                       ('command_mv', nm), ('predicted_cv', nc), ('quality', nc + nm + nd)):
        values = frame.get(key)
        if not isinstance(values, list) or len(values) != count or not all(finite(v) for v in values):
            raise ValueError('Invalid native frame dimensions/values: ' + key)
        if key == 'quality' and any(type(v) is not int or not 0 <= v <= 65535 for v in values):
            raise ValueError('Invalid native quality code')
    if not isinstance(frame.get('solver_status'), str) or len(frame['solver_status']) > 80:
        raise ValueError('Invalid native solver status')


def validate_native_state(response, samples, controller, scenario):
    frame = response.get('frame')
    forecast = response.get('latest_forecast')
    if frame is not None:
        if not samples:
            raise ValueError('Native frame precedes the first completed scan')
        validate_frame(frame, controller, scenario['time_seconds'][samples - 1])
        targets = scenario.get('targets')
        expected_targets = targets[samples - 1] if targets else [cv['limits']['target'] for cv in controller['variables']['cvs']]
        if frame['targets'] != expected_targets:
            raise ValueError('Native frame targets differ from the frozen scenario')
    if forecast is not None:
        if frame is None:
            raise ValueError('Native forecast has no valid matching frame')
        validate_forecast(forecast, controller, samples - 1, frame['time_seconds'])
        nc = len(controller['variables']['cvs'])
        if forecast['cv'][:nc] != frame['predicted_cv']:
            raise ValueError('Native forecast differs from the recorded solve')
    recent = response.get('recent_trace')
    if recent is not None:
        times = recent.get('time_seconds') if isinstance(recent, dict) else None
        if not isinstance(times, list) or len(times) != min(samples, 256):
            raise ValueError('Invalid bounded native recent trace')
        if set(recent) - {'time_seconds', 'cv', 'mv', 'dv', 'targets', 'command_mv', 'predicted_cv', 'quality', 'solver_status', 'solve_ms'}:
            raise ValueError('Invalid native recent trace fields')
        if 'solve_ms' in recent and (not isinstance(recent['solve_ms'], list) or len(recent['solve_ms']) != len(times)
                                    or any(not finite(v) or v < 0 for v in recent['solve_ms'])):
            raise ValueError('Invalid native solver timings')
        expected_times = scenario['time_seconds'][samples - len(times):samples]
        if times != expected_times:
            raise ValueError('Native recent trace clock differs from frozen scenario')
        for key in ('cv', 'mv', 'dv', 'targets', 'command_mv', 'predicted_cv', 'quality', 'solver_status'):
            if not isinstance(recent.get(key), list) or len(recent[key]) != len(times):
                raise ValueError('Native recent trace dimensions differ')
        for index, seconds in enumerate(times):
            row = {key: recent[key][index] for key in ('cv', 'mv', 'dv', 'targets', 'command_mv', 'predicted_cv', 'quality', 'solver_status')}
            row['time_seconds'] = seconds
            validate_frame(row, controller, seconds)
            if index == len(times) - 1 and frame is not None and row != frame:
                raise ValueError('Native latest frame differs from recent trace')


def archive_source(root, payload, session):
    if session:
        record = get_session(root, payload['id'], payload['owner'])
        if record['status'] != 'completed':
            raise ValueError('Only completed twin sessions expose full archived curves')
        directory = session_dir(root, record['id'])
        verify_frozen_inputs(directory, record)
        result = artifact(directory, 'result.json')
        if record.get('result_sha256') and fingerprint(result) != record['result_sha256']:
            raise ValueError('Completed twin result integrity mismatch')
        run = {'id': record['run_id'], 'study': record['study'], 'results': {'splits': []}}
        controller_path = artifact(directory, 'controller.json')
        scenario = load_json(artifact(directory, 'scenario.json'))
        method, variant = record['method'], record['id']
        forecast_name = 'forecast.ndjson'
        for name, digest in record.get('forecast_hashes', {}).items():
            if fingerprint(artifact(directory, name)) != digest:
                raise ValueError('Completed twin forecast integrity mismatch')
        start, offset = record['start'], record['source_offset_seconds']
        source = 'Simulated COM DA' if record['transport'] == 'opc-da' else 'Simulated direct'
    else:
        run, directory, trial, controller_path = selected_trial(root, payload['id'], payload['trial_id'])
        result = artifact(directory, trial['id'] + '.json')
        forecast_name = trial['id'] + '.forecast.ndjson'
        receipt_path = directory / (trial['id'] + '.command.json')
        if receipt_path.exists():
            receipt = load_json(artifact(directory, receipt_path.name))
            for key, name in (('output_sha256', result.name), ('forecast_sha256', forecast_name),
                              ('forecast_index_sha256', forecast_name + '.index.json')):
                if receipt.get(key) and receipt[key] != fingerprint(artifact(directory, name)):
                    raise ValueError('Recorded trial result/forecast integrity mismatch')
        scenario = load_json(artifact(directory, 'selection-scenario.json'))
        start, offset = scenario_context(run, scenario)
        run = copy.deepcopy(run)
        run['study']['config']['start'] = start
        run['results'] = {'splits': []}
        method, variant = trial['method'], trial['id']
        source = 'Simulated direct'
    controller = load_json(controller_path)
    return dict(directory=directory, run=run, result=result, controller=controller, scenario=scenario,
                controller_sha256=fingerprint(controller_path), method=method, variant=variant,
                forecast=directory / forecast_name, source=source, start=start, offset=offset)


def archived_frame(root, payload, session=False):
    allowed = {'id', 'sample', 'owner'} | (set() if session else {'trial_id'})
    if set(payload) != allowed or type(payload.get('sample')) is not int or payload['sample'] < 0:
        raise ValueError('Frame requires one exact nonnegative integer sample')
    source = archive_source(root, payload, session)
    result = load_json(source['result'])
    trace = result.get('trace', {})
    times = trace.get('time_seconds', [])
    sample = payload['sample']
    if result.get('status') != 'valid' or not times or len(times) > MAX_SAMPLES or times != source['scenario']['time_seconds']:
        raise ValueError('Recorded frame timeline differs from complete frozen scenario')
    if any(not finite(t) or t < 0 for t in times) or any(b <= a for a, b in zip(times, times[1:])):
        raise ValueError('Invalid recorded frame timestamps')
    if sample >= len(times):
        raise ValueError('Frame sample is outside the recorded archive')
    frame = {'time_seconds': times[sample]}
    for key in ('cv', 'mv', 'dv', 'targets', 'command_mv', 'predicted_cv', 'quality'):
        rows = trace.get(key)
        if not isinstance(rows, list) or len(rows) != len(times):
            raise ValueError('Incomplete recorded frame: ' + key)
        frame[key] = rows[sample]
    statuses = trace.get('solver_status')
    if statuses is not None and (not isinstance(statuses, list) or len(statuses) != len(times)):
        raise ValueError('Incomplete recorded solver statuses')
    frame['solver_status'] = statuses[sample] if statuses is not None else 'unavailable'
    validate_frame(frame, source['controller'], times[sample])
    forecast = None
    path = source['forecast']
    present = [path.exists() or path.is_symlink(),
               path.with_name(path.name + '.index.json').exists() or path.with_name(path.name + '.index.json').is_symlink()]
    if any(present):
        if not all(present):
            raise ValueError('Incomplete recorded forecast archive')
        artifact(source['directory'], path.name)
        artifact(source['directory'], path.name + '.index.json')
        controller = source['controller']
        with path.open('rb') as stream:
            if len(stream.readline(8193)) > 8192:
                raise ValueError('Forecast header exceeds its bounded schema')
        archive = ForecastArchive(path, controller['tuning']['sample_time'],
                                  len(controller['variables']['cvs']), len(controller['variables']['mvs']))
        if len(archive.offsets) != len(times):
            raise ValueError('Forecast index does not cover the complete recorded trace')
        with path.open('rb') as stream:
            stream.seek(archive.offsets[sample])
            if len(stream.readline(MAX_HORIZON_BYTES + 1)) > MAX_HORIZON_BYTES:
                raise ValueError('Recorded forecast horizon exceeds response limit')
            row = archive.record(stream, sample)
        forecast = {key: archive.header[key] for key in ('schema_version', 'sample_time', 'cv_count', 'mv_count',
                    'prediction_horizon', 'control_horizon', 'layout', 'cv_first_offset_steps', 'mv_first_offset_steps')}
        forecast.update(row)
        validate_forecast(forecast, controller, sample, times[sample])
        if forecast['cv'][:len(controller['variables']['cvs'])] != frame['predicted_cv']:
            raise ValueError('Indexed forecast differs from recorded frame solve')
    return dict(sample=sample, total_samples=len(times), frame=frame, latest_forecast=forecast,
                sample_time=source['controller']['tuning']['sample_time'], start=source['start'],
                time_basis='relative' if source['start'] == 'relative' else 'utc', source_offset_seconds=source['offset'],
                controller_sha256=source['controller_sha256'],
                variables=effective_variables(source['run'], source['controller']))


def chart(root, payload, session=False):
    resolved = archive_source(root, payload, session)
    directory, run = resolved['directory'], resolved['run']
    method, variant = resolved['method'], resolved['variant']
    result, controller, forecast, source = (resolved[key] for key in ('result', 'controller', 'forecast', 'source'))
    data = process_trends(directory, run, method, variant, payload,
                          frozen_source={'result': result, 'controller': controller, 'forecast': forecast, 'source': source})
    data['provenance'] += ' ' + ('Independent frozen twin rerun; not approval or new validation.' if session else 'Recorded selection-validation trial only; fresh baseline initialization, not full-period trajectory.')
    return data


def execute(root, operation, payload):
    from twin_session_state import control, native_begin, operation_snapshot, reconcile, session_lock
    if operation == 'twin-reconcile':
        return reconcile(root, payload)
    if operation == 'twin-operation':
        return operation_snapshot(get_session(root, payload['id'], payload['owner']), payload['operation_id'])
    if operation in ('twin-control', 'twin-native-begin', 'twin-update'):
        with session_lock(root, payload['id']):
            handler = {'twin-control': control, 'twin-native-begin': native_begin, 'twin-update': update_session}[operation]
            return handler(root, payload)
    if operation == 'twin-config':
        return configuration(root, payload)
    if operation == 'twin-equipment-view':
        return create_version(root, payload)
    if operation == 'twin-settings':
        return create_version(root, payload, settings=True)
    if operation == 'twin-trial-frame':
        return archived_frame(root, payload)
    if operation == 'twin-session-frame':
        return archived_frame(root, payload, session=True)
    if operation == 'twin-manifest':
        return manifest(root, payload)
    if operation == 'twin-create':
        return create_session(root, payload)
    if operation == 'twin-get':
        return public(get_session(root, payload['id'], payload['owner']))
    if operation == 'twin-trial-series':
        return chart(root, payload)
    if operation == 'twin-session-series':
        return chart(root, payload, session=True)
    raise ValueError('Unknown twin operation')
