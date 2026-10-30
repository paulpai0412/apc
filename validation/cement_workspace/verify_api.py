"""Real authenticated HTTP workflow and full-quarter native cement acceptance.

No samples/trials are shortened. Manual candidate selection is an explicit API
request before sealed validation (the script selects the minimum eligible frozen
validation score, or retains baseline). UI/browser/video acceptance is additional.
Use --until-selection to leave the real server job waiting for browser review;
use --resume-run ID after the browser makes its selection to verify that run.
"""
from __future__ import annotations
import argparse
import copy
import http.cookiejar
import json
from pathlib import Path
import sys
import time
import urllib.error
import urllib.request
import uuid

from workspace import SAMPLES, command, private_env, require, save, sha


class Api:
    def __init__(self, url, credentials):
        require(url.startswith('http://127.0.0.1:'), 'Only loopback acceptance endpoints are allowed')
        self.url = url
        self.credentials = credentials
        self.csrf = None
        self.jar = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), urllib.request.HTTPCookieProcessor(self.jar))
        login = self.call('/api/auth/login', credentials)
        self.csrf = login['csrf_token']
        identity = self.call('/api/auth/me')
        require(identity['authenticated'] is True and identity['username'] == credentials['username']
                and identity['role'] == login['role'] and identity['csrf_token'] == self.csrf,
                'Login response did not establish the actual cookie-backed authenticated identity')

    def call(self, path, body=None, *, expected_error=None, expected_status=None):
        data = json.dumps(body, allow_nan=False).encode() if body is not None else None
        headers = {'Content-Type': 'application/json', 'Connection': 'close'}
        if self.csrf:
            headers['x-csrf-token'] = self.csrf
        request = urllib.request.Request(self.url + path, data=data, headers=headers)
        try:
            with self.opener.open(request, timeout=660) as response:
                require(expected_status is None or response.status == expected_status,
                        f'{path}: expected HTTP {expected_status}, received {response.status}')
                content = response.read()
                result = json.loads(content) if content else {}
        except urllib.error.HTTPError as error:
            content = error.read().decode()
            try:
                result = json.loads(content)
            except json.JSONDecodeError:
                result = {'error': content}
            if expected_error is not None:
                require(error.code in expected_error, f'{path}: unexpected rejection {error.code}: {result}')
                return result
            raise RuntimeError(f'{path}: HTTP {error.code}: {result}') from error
        require(expected_error is None, f'{path}: expected rejection but operation succeeded')
        return result


def identifier(response):
    return response.get('id') or response['dataset']['id']


def wait_run(api, run_id, stop_at_selection, evidence, deadline):
    previous = None
    while True:
        record = api.call('/api/research/runs/' + run_id)
        stage = record.get('stage')
        if stage != previous:
            print(json.dumps({'run_id': run_id, 'status': record['status'], 'stage': stage}), flush=True)
            previous = stage
        require(record['status'] not in ('failed', 'cancelled'), f'Research did not complete: {record.get("error", record["status"])}')
        if record['status'] == 'completed':
            return record
        if stage == 'awaiting_selection':
            if stop_at_selection:
                return record
            selections = {}
            for method in record['study']['research']['methods']:
                trials = [t for t in record['trials'] if t['method'] == method]
                eligible = [t for t in trials if t.get('eligible')]
                best = min(eligible, key=lambda t: t['score']) if eligible else next(t for t in trials if t['round'] == 0 and t['valid'])
                selections[method] = best['id']
            payload = {'selections': selections,
                       'reason': 'Explicit acceptance operator decision: lowest eligible frozen development score, otherwise retain baseline; sealed holdout not consulted.'}
            decision = api.call('/api/research/runs/' + run_id + '/selection', payload)
            duplicate = api.call('/api/research/runs/' + run_id + '/selection', payload)
            require(decision == duplicate, 'Same manual selection was not idempotent')
            evidence['manual_selection'] = decision
        require(time.monotonic() < deadline, 'Acceptance wall-clock deadline exceeded; server job is NOT cancelled and remains observable by run ID')
        time.sleep(2)


def postgres_import(api, output):
    sources = api.call('/api/research/sources')['sources']
    require(any(source['id'] == 'cement-quarter' for source in sources), 'Disposable PostgreSQL source is not allowlisted')
    imported = api.call('/api/research/import-postgres', {'source_id': 'cement-quarter',
        'name': 'Owned PostgreSQL cement quarter', 'start': '2026-01-01T00:00:00Z', 'end': '2026-04-01T00:00:00Z'})
    require(imported['row_count'] == SAMPLES + 1 and imported['source']['kind'] == 'postgres',
            'Real PostgreSQL import did not preserve the full raw quarter and provenance')
    require(imported['source']['source_id'] == 'cement-quarter', 'PostgreSQL provenance source identity mismatch')
    save(output / 'postgres-import.json', imported)
    return imported


def import_identify(api, root, output):
    content = (root / 'data/cement-2026-q1-raw.csv').read_bytes().decode('utf-8')
    raw = api.call('/api/research/import', {'name': 'Synthetic cement Q1 native raw with documented defects', 'format': 'csv', 'content': content})
    raw_id = identifier(raw)
    require(raw['source_sha256'] == sha(root / 'data/cement-2026-q1-raw.csv'),
            'HTTP import did not preserve the actual source bytes')
    if json.loads((root / 'workspace.json').read_text())['postgres']:
        postgres_import(api, output)
    cleaning = json.loads((root / 'data/cleaning.json').read_text())
    payload = {'dataset_id': raw_id, 'cleaning': cleaning}
    preview = api.call('/api/research/preview', payload)
    require(preview['usable'], 'Full cleaning preview remains unusable')
    cleaned = api.call('/api/research/clean', payload)
    dataset_id = identifier(cleaned)
    store = root / 'research-store'
    history_path = store / 'datasets' / dataset_id / 'history.json'
    history = json.loads(history_path.read_text())
    require(history['usable'] and len(history['time_seconds']) == SAMPLES, 'Cleaning shortened the full quarter')
    require(history['cv'][120] == history['cv'][119], 'Bad quality was not causally filled')
    require(history['cv'][180][0] == history['cv'][179][0], 'Missing CV was not causally filled')
    bootstrap = api.call('/api/research')
    # Explicit independent synthetic physics is the evaluation truth. We do NOT
    # derive a new truth from the controller being evaluated or label data causal
    # merely because its filename says synthetic.
    config = bootstrap['defaults']['campaign']
    fixture = json.loads((root / 'data/campaign.json').read_text())
    require(config['plant'] == fixture['plant'], 'Server default plant differs from captured native source physics')
    require(config['sample_time'] == 60 and config['months'] == 3, 'Quarter contract changed')
    profile = json.loads((root / 'runtime/profile.json').read_text())
    config['id'] = profile['engine_id']
    for role in ('cv', 'mv', 'dv'):
        bindings = {row['name']: row for row in profile['model_binding'][role + 's']}
        for variable in config['variables'][role]:
            variable['node_ids'] = copy.deepcopy(bindings[variable['name']]['node_ids'])
    # Bind before identification and immutable study freeze. Never rewrite an
    # already-qualified controller to masquerade as commissioned feedback.
    research = bootstrap['defaults']['research']
    research['selection_mode'] = 'manual'
    study = api.call('/api/research/studies', {'name': 'Cement full-quarter independent synthetic acceptance',
            'dataset_id': dataset_id, 'config': config, 'research': research})
    require(study['dataset_sha256'] == sha(history_path), 'Study does not freeze the actual cleaned bytes')
    model = api.call('/api/research/identify', {'study_id': study['id']})
    require(model['qualification']['historical_prediction'] is True, 'Model did not qualify on chronological development data')
    require(model['qualification']['test_accessed'] is False and model['qualification']['factory_authority'] is False,
            'Identification improperly claims sealed/factory qualification')
    require(model['dataset_sha256'] == sha(history_path), 'Frozen model dataset lineage mismatch')
    study = api.call('/api/research/studies', {'name': 'Cement frozen qualified model tuning', 'id': study['id'],
            'dataset_id': dataset_id, 'model_id': model['id'], 'config': study['config'], 'research': research})
    save(output / 'import-clean-identify.json', {'raw_id': raw_id, 'dataset_id': dataset_id,
         'study_id': study['id'], 'model_id': model['id'], 'raw_sha256': sha(root / 'data/cement-2026-q1-raw.csv'),
         'history_sha256': sha(history_path), 'source_provenance': json.loads((root / 'data/provenance.json').read_text()),
         'preview_summary': preview['summary'], 'model_qualification': model['qualification']})
    return study


def forecast_check(snapshot):
    require(snapshot['samples'] > 0, 'Forecast requires a native completed scan')
    forecast, frame = snapshot['latest_forecast'], snapshot['frame']
    require(forecast is not None and frame is not None, 'Native forecast/frame is missing')
    require(forecast['sample'] == snapshot['samples'] - 1, 'Forecast sample is not current acknowledged sample')
    require(forecast['issued_at_seconds'] == frame['time_seconds'], 'Forecast issue time differs from native frame')
    require(len(forecast['cv']) == forecast['prediction_horizon'] * forecast['cv_count'], 'Full prediction horizon missing')
    require(len(forecast['mv_delta']) == forecast['control_horizon'] * forecast['mv_count'], 'Full move horizon missing')
    require(forecast['cv'][:forecast['cv_count']] == frame['predicted_cv'], 'First prediction differs from native solve')


def session(api, root, output, run_id, trial_id, transport, deadline):
    created = api.call('/api/research/twin/sessions', {'run_id': run_id, 'trial_id': trial_id,
                      'scenario': 'full', 'transport': transport})
    session_id = created['id']
    path = '/api/research/twin/sessions/' + session_id
    require(created['status'] == 'paused' and created['samples'] == 0, 'Session must begin paused without pretending to have a frame')
    require(created['requested_samples'] == SAMPLES, 'Session scenario is not the complete 129600-scan quarter')
    print(json.dumps({'session_id': session_id, 'run_id': run_id, 'transport': transport, 'requested_samples': SAMPLES}), flush=True)
    step_id = 'acceptance-step-' + uuid.uuid4().hex
    stepped = api.call(path + '/step', {'operation_id': step_id})
    require(stepped['samples'] == 1, 'Exactly one native scan required for step')
    forecast_check(stepped)
    duplicate = api.call(path + '/step', {'operation_id': step_id})
    require(duplicate['samples'] == 1 and duplicate['sequence'] == stepped['sequence'], 'Idempotent step executed twice')
    lookup = api.call(path + '/operations/' + step_id)
    require(lookup['operation']['id'] == step_id, 'Operation outcome cannot be reconciled by ID')
    api.call(path + '/start', {'operation_id': step_id}, expected_error=(422,))
    frozen = api.call(path)
    require(frozen['operation']['id'] == step_id and frozen['operation']['command'] == 'step'
            and frozen['samples'] == 1, 'Rejected operation-ID reuse changed the original command or advanced scans')
    time.sleep(1)
    same = api.call(path)
    require(same['samples'] == frozen['samples'], 'Read-only GET advanced a paused session')
    start_id = 'acceptance-start-' + uuid.uuid4().hex
    api.call(path + '/start', {'operation_id': start_id, 'batch_steps': 128})
    # Every request closes its TCP connection. Drop all client session state and
    # issue NO poll/advance requests during this interval: owner must continue.
    del api
    time.sleep(5)
    creds = json.loads((root / 'test-credentials.json').read_text())
    port = json.loads((root / 'workspace.json').read_text())['research_port']
    api = Api(f'http://127.0.0.1:{port}', {k: creds[k] for k in ('username', 'password')})
    resumed = api.call(path)
    require(resumed['samples'] > 1, 'Server-owned job did not continue after client disconnect')
    paused = api.call(path + '/pause', {'operation_id': 'acceptance-pause-' + uuid.uuid4().hex})
    require(paused['status'] in ('paused', 'completed'), 'Pause did not acknowledge a settled native batch')
    time.sleep(1)
    still = api.call(path)
    require(still['samples'] == paused['samples'], 'Native scans continued after acknowledged pause')
    if paused['status'] != 'completed':
        api.call(path + '/start', {'operation_id': 'acceptance-resume-' + uuid.uuid4().hex, 'batch_steps': 512})
    last = -1
    while True:
        snapshot = api.call(path)
        require(snapshot['samples'] >= last, 'Acknowledged native sequence went backwards')
        last = snapshot['samples']
        require(snapshot['status'] not in ('failed', 'closed', 'interrupted'), 'Full-quarter session failed: ' + str(snapshot.get('reason', snapshot['status'])))
        if snapshot['samples']:
            forecast_check(snapshot)
        if snapshot['status'] == 'completed':
            break
        require(time.monotonic() < deadline, 'Session deadline exceeded; native owner remains observable, not falsely passed')
        time.sleep(2)
    require(snapshot['samples'] == snapshot['requested_samples'] == SAMPLES, 'Terminal session is incomplete')
    directory = root / 'research-store/twin-sessions' / session_id
    result = json.loads((directory / 'result.json').read_text())
    require(result['status'] == 'valid' and result['samples'] == SAMPLES, 'Native archive is not complete valid output')
    require(result['solver']['calls'] == SAMPLES and result['solver']['failures'] == 0, 'Not one successful native solve per scan')
    if transport == 'opc-da':
        stats = result['transport_stats']
        require(result['transport'] == 'opc-da' and stats['write_calls'] == SAMPLES and stats['update_calls'] == SAMPLES and stats['read_calls'] >= 2 * SAMPLES,
                'Real COM DA did not exchange every native scan')
    from research_trends import ForecastArchive
    archive = ForecastArchive(directory / 'forecast.ndjson', 60.0, len(result['trace']['cv'][0]), len(result['trace']['mv'][0]))
    require(len(archive.offsets) == SAMPLES, 'Archived forecasts omitted scans')
    for sample in (0, 44640, 84960, SAMPLES - 1):
        forecast = archive.at(sample * 60)
        require(forecast['sample'] == sample, 'Archived forecast index mismatch')
        api.call(path + '/frame?sample=' + str(sample))
    api.call(path + '/series?start=0&end=' + str((SAMPLES - 1) * 60))
    evidence = {'session_id': session_id, 'transport': transport, 'samples': SAMPLES,
                'result_sha256': sha(directory / 'result.json'), 'forecast_sha256': sha(directory / 'forecast.ndjson'),
                'trace_journal_sha256': snapshot.get('trace_journal_sha256'),
                'checks': ['one-step', 'idempotent-step', 'operation-lookup', 'conflicting-id-rejected',
                           'read-only-snapshot', 'client-disconnect-server-progress', 'pause-settles-batch',
                           'resume', 'full-quarter', 'native-forecast', 'complete-archive']}
    save(output / (trial_id + '-' + transport + '-session.json'), evidence)
    return result, evidence


def sensitivity(root, output, run_id, config):
    """Paired native plant interventions: hold noise/DV/time fixed, vary each MV."""
    directory = root / 'research-store/runs' / run_id
    plant = directory / 'plant.json'
    original = json.loads((directory / 'scenario.json').read_text())
    baseline_file = output / 'sensitivity-baseline.json'
    command(root, [config['binary'], 'replay', '--plant', plant, '--scenario', directory / 'scenario.json', '--output', baseline_file], label='full-quarter-sensitivity-baseline')
    baseline = json.loads(baseline_file.read_text())
    require(len(baseline['cv']) == SAMPLES, 'Sensitivity baseline omitted scans')
    import numpy as np
    changes = []
    campaign = json.loads((root / 'data/campaign.json').read_text())
    for index, variable in enumerate(campaign['variables']['mv']):
        changed = copy.deepcopy(original)
        amount = (variable['high'] - variable['low']) * 0.01
        for row in changed['mv']:
            row[index] = min(variable['high'], row[index] + amount)
        scenario_path = output / (f'sensitivity-mv-{index}-scenario.json')
        result_path = output / (f'sensitivity-mv-{index}.json')
        save(scenario_path, changed)
        command(root, [config['binary'], 'replay', '--plant', plant, '--scenario', scenario_path, '--output', result_path], label=f'full-quarter-mv-{index}-sensitivity')
        result = json.loads(result_path.read_text())
        require(len(result['cv']) == SAMPLES, 'MV intervention shortened the native timeline')
        delta = np.max(np.abs(np.asarray(result['cv']) - np.asarray(baseline['cv'])), axis=0)
        require(np.any(delta > 1e-9), f'Native CV did not respond to independent {variable["name"]} command intervention')
        changes.append({'mv': variable['name'], 'command_delta': amount, 'maximum_absolute_cv_response': delta.tolist(),
                        'scenario_sha256': sha(scenario_path), 'output_sha256': sha(result_path)})
    return {'scope': 'paired_simulated_MV_intervention_not_factory_causality', 'samples_each': SAMPLES, 'interventions': changes}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', required=True, type=lambda x: Path(x).resolve())
    parser.add_argument('--until-selection', action='store_true')
    parser.add_argument('--resume-run')
    parser.add_argument('--deadline-hours', type=float, default=168, help='Wall-clock timeout only; never reduces native samples')
    parser.add_argument('--acknowledge-owned-faults', action='store_true',
                        help='Required for complete acceptance: withhold only verified opted-in simulator acquisition publication within its monotonic bound and interrupt only owned loopback engine; preserve real unknown outcome')
    args = parser.parse_args()
    root = args.root
    config = json.loads((root / 'workspace.json').read_text())
    require(config['transport'] == 'opc-da', 'Requested full research requires real COM transport; unavailable is NOT pass')
    require(args.deadline_hours > 0, 'Positive wall-clock deadline required')
    require(args.until_selection or args.acknowledge_owned_faults,
            'Full acceptance includes real owned-process faults. Supply --acknowledge-owned-faults; omitting recovery is not a passing narrower run.')
    require(args.until_selection or config['postgres'],
            'Full all-source acceptance requires preparation with --postgres; an omitted PostgreSQL import is not pass.')
    source = root / 'source'
    sys.path.insert(0, str(source / 'validation'))
    output = root / ('acceptance-' + str(time.time_ns()))
    output.mkdir()
    evidence = {'status': 'running', 'scope': 'full_quarter_synthetic_cement_software_acceptance',
                'samples_required': SAMPLES, 'ui_video': 'not_exercised', 'runtime_live': 'not_exercised',
                'commands_root': str(root / 'commands'), 'sessions': []}
    save(output / 'invocation.json', {'argv': sys.argv, 'workspace': config, 'python': sys.version})
    try:
        credentials = json.loads((root / 'test-credentials.json').read_text())
        api = Api(f'http://127.0.0.1:{config["research_port"]}', {k: credentials[k] for k in ('username', 'password')})
        if args.resume_run:
            run_id = args.resume_run
            if config['postgres']:
                evidence['postgres_import'] = postgres_import(api, output)
        else:
            study = import_identify(api, root, output)
            started = api.call('/api/research/runs', {'study_id': study['id']})
            run_id = started['id']
        evidence['run_id'] = run_id
        save(output / 'run-identity.json', {'run_id': run_id, 'root': str(root), 'artifact_directory': str(root / 'research-store/runs' / run_id)})
        deadline = time.monotonic() + args.deadline_hours * 3600
        record = wait_run(api, run_id, args.until_selection, evidence, deadline)
        if record['status'] != 'completed':
            evidence['status'] = 'awaiting_manual_selection_not_passed'
            print(f'BROWSER REVIEW READY run_id={run_id}; server job remains running. Resume: {config["python"]} {source / "validation/cement_workspace/verify_api.py"} --root {root} --resume-run {run_id} --acknowledge-owned-faults', flush=True)
            return
        require(record['study']['research']['selection_mode'] == 'manual', 'Acceptance must exercise explicit manual candidate freeze')
        require(args.acknowledge_owned_faults, 'Run already completed; full acceptance requires explicit --acknowledge-owned-faults')
        require(config['postgres'], 'Completed full-source acceptance requires an actual configured disposable PostgreSQL source')
        require(record['selection_decision']['selections'], 'Manual candidate selection receipt missing')
        from verify_autoresearch import check_run
        evidence['research'] = check_run(root / 'research-store', run_id)
        require(evidence['research']['samples_per_method'] == SAMPLES, 'Full research checker did not cover quarter')
        from verify_twin import compare
        for row in record['results']['methods']:
            direct, direct_evidence = session(api, root, output, run_id, row['selected_trial'], 'direct', deadline)
            da, da_evidence = session(api, root, output, run_id, row['selected_trial'], 'opc-da', deadline)
            differences = compare(direct, da)
            evidence['sessions'].extend([direct_evidence, da_evidence])
            evidence.setdefault('direct_da_equivalence', {})[row['method']] = differences
        evidence['mv_cv_sensitivity'] = sensitivity(root, output, run_id, config)
        # Reuse native session edge-case verifier on the same frozen full inputs.
        command(root, [config['python'], source / 'validation/verify_twin.py', '--binary', config['binary'],
                      '--run-directory', root / 'research-store/runs' / run_id, '--output', output / 'native-equivalence',
                      '--bridge', config['wine'], '--bridge-arg=' + config['bridge']],
                env=private_env(root), label='full-native-equivalence')
        from verify_runtime import verify as verify_runtime
        evidence['runtime_live'] = verify_runtime(api, root, output, record, deadline)
        evidence['status'] = 'passed_api_native_runtime_ui_video_not_exercised'
    except Exception as error:
        evidence.update(status='failed', error=str(error))
        raise
    finally:
        evidence['files'] = {str(p.relative_to(output)): sha(p) for p in output.rglob('*') if p.is_file()}
        save(output / 'manifest.json', evidence)
        print(json.dumps({'status': evidence['status'], 'manifest': str(output / 'manifest.json'), 'run_id': evidence.get('run_id')}), flush=True)


if __name__ == '__main__':
    main()
