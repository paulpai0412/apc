"""Real Runtime HTTP acceptance against the owned OPC/native/historian stack.

No force-arming, synthetic confirmations, fake historian rows, or shortened native
warmup. Immutable research candidates must already match commissioned mapping/Ts.
Every mutation has a unique durable operation ID; uncertain outcomes are queried,
never resent as a new operation. Independent candidate approval uses reviewer.
"""
from datetime import datetime, timedelta, timezone
import json
from pathlib import Path
import time
from urllib.parse import urlencode
import uuid

from workspace import require, save


def stamp(value=None):
    return (value or datetime.now(timezone.utc)).isoformat().replace('+00:00', 'Z')


def current(api, profile_hash):
    rows = api.call('/api/runtime/overview')['profiles']
    return next(row for row in rows if row['profile_hash'] == profile_hash)


def await_operation(api, operation_id, deadline):
    while True:
        row = api.call('/api/runtime/operations/' + operation_id)
        if row['state'] not in ('executing', 'unknown'):
            require(row['state'] == 'accepted', 'Native operation did not accept: ' + json.dumps(row))
            return row
        require(time.monotonic() < deadline, 'Native operation remains unresolved; query original ID ' + operation_id + ', do not resend')
        time.sleep(2)


def healthy(api, profile_hash, deadline):
    while True:
        row = current(api, profile_hash)
        native = row.get('native_status') or {}
        if row['health'] == 'good' and native.get('healthy') is True:
            return row
        require(time.monotonic() < deadline, 'Genuine native acquisition/history/solve qualification remains unavailable: ' + json.dumps(native))
        time.sleep(5)


def operation(api, profile_hash, action, deadline, output, launch_version=None):
    state = current(api, profile_hash)
    operation_id = 'runtime-acceptance-' + uuid.uuid4().hex
    payload = {'operation_id': operation_id, 'profile_hash': profile_hash,
               'expected_version': state['version'], 'expected_epoch': state['epoch'],
               'expected_instance': state['instance_id'],
               'expires_at': stamp(datetime.now(timezone.utc) + timedelta(seconds=295)),
               'reason': 'Explicit disposable cement API acceptance: ' + action['kind'], 'action': action}
    if action['kind'] in ('start', 'start_candidate', 'recover_monitor'):
        require(launch_version is not None, 'Cold start requires the original exact commissioned model digest')
        payload.update(expected_version=launch_version, expected_epoch=state['registry_epoch'], expected_instance=None)
    save(output / (operation_id + '-intent.json'), payload)
    api.call('/api/runtime/operations', payload)
    settled = await_operation(api, operation_id, deadline)
    duplicate = api.call('/api/runtime/operations', payload)
    require(duplicate['operation_id'] == settled['operation_id'] and duplicate['state'] == settled['state'],
            'Durable idempotent operation changed its settled outcome')
    save(output / (operation_id + '-result.json'), settled)
    require(settled['blocking'] is False, 'Accepted operation still blocks later independently authorized work')
    return settled


def performance(api, profile_hash, output):
    # Record two nonoverlapping actual source-time windows after the physical
    # producer is running. These are observational comparisons, not causal gains.
    baseline_start = datetime.now(timezone.utc) + timedelta(seconds=3)
    baseline_end = baseline_start + timedelta(seconds=65)
    candidate_start = baseline_end + timedelta(seconds=3)
    candidate_end = candidate_start + timedelta(seconds=65)
    wait_seconds = (candidate_end + timedelta(seconds=5) - datetime.now(timezone.utc)).total_seconds()
    time.sleep(max(0, wait_seconds))
    query = urlencode({'profile_hash': profile_hash, 'baseline_start': stamp(baseline_start),
                       'baseline_end': stamp(baseline_end), 'candidate_start': stamp(candidate_start),
                       'candidate_end': stamp(candidate_end)})
    evidence = api.call('/api/runtime/performance?' + query)
    save(output / 'actual-historian-performance.json', evidence)
    require(evidence['comparable'] is True, 'R5 lacks complete coherent comparable original-source-time samples; not an improvement claim')
    require(evidence['baseline']['valid_samples'] >= 30 and evidence['candidate']['valid_samples'] >= 30,
            'R5 performance omitted actual process samples')
    require(evidence.get('energy_intensity') is None and evidence.get('causal_improvement') is None,
            'Synthetic observational fixture must not invent measured energy/causal improvement')
    return evidence


def approve_install(research_api, admin, review, profile, run_id, method, deadline, directory):
    version = research_api.call('/api/research/versions', {'run_id': run_id, 'method': method,
        'deployment_profile_hash': profile['profile_hash'], 'target': profile['target'],
        'equipment_instance_ids': profile['equipment_instance_ids'], 'operating_envelope': profile['operating_envelope']})
    before = current(admin, profile['profile_hash'])
    payload = {'profile_hash': profile['profile_hash'], 'candidate_hash': version['id'],
               'expires_at': stamp(datetime.now(timezone.utc) + timedelta(days=1))}
    admin.call('/api/runtime/approvals', payload, expected_error=(403,))
    approval = review.call('/api/runtime/approvals', payload)
    require(approval['installed'] is False and approval['started'] is False and approval['armed'] is False,
            'Independent approval unexpectedly performed deployment/control')
    install_id = 'cement-install-' + uuid.uuid4().hex
    request = {'operation_id': install_id, 'profile_hash': profile['profile_hash'], 'approval_id': approval['approval_id'],
               'expected_epoch': before['registry_epoch'], 'expires_at': stamp(datetime.now(timezone.utc) + timedelta(seconds=55)),
               'reason': 'Explicit local immutable install; no control transition authorized'}
    save(directory / (install_id + '-intent.json'), request)
    admin.call('/api/control/versions/' + version['id'] + '/install', request)
    installed = await_operation(admin, install_id, deadline)
    after = current(admin, profile['profile_hash'])
    require(after['version'] == before['version'] and after['authority'] == before['authority'],
            'Install silently changed running controller authority/version')
    return {'version': version, 'approval': approval, 'installation': installed}


def verify(research_api, root, output, record, deadline):
    from verify_api import Api
    config = json.loads((root / 'workspace.json').read_text())
    credentials = json.loads((root / 'test-credentials.json').read_text())
    reviewer = json.loads((root / 'reviewer-credentials.json').read_text())
    url = f'http://127.0.0.1:{config["runtime_port"]}'
    admin = Api(url, {key: credentials[key] for key in ('username', 'password')})
    review = Api(url, {key: reviewer[key] for key in ('username', 'password')})
    profile = json.loads((root / 'runtime/profile.json').read_text())
    profile_hash = profile['profile_hash']
    evidence = {'status': 'running', 'profile_hash': profile_hash, 'factory_qualification': False,
                'controller_sample_time': 60, 'physical_acquisition_sample_time': 1, 'versions': []}
    directory = output / 'runtime'
    directory.mkdir()
    try:
        require(admin.call('/api/workspace')['research_enabled'] is False, 'Runtime stack must not enable research')
        require(research_api.call('/api/workspace')['runtime_enabled'] is False, 'Research stack must not expose field writes')
        row = healthy(admin, profile_hash, deadline)
        require(row['native_status']['sample_time'] == 60, 'Runtime controller clock differs from original quarter')
        # Current authority is observed, never changed by navigating a workspace.
        evidence['initial_native'] = row['native_status']
        if row['authority'] != 'monitor':
            operation(admin, profile_hash, {'kind': 'exit'}, deadline, directory)
            operation(admin, profile_hash, {'kind': 'monitor'}, deadline, directory)
        healthy(admin, profile_hash, deadline)
        candidates = [row for row in record['results']['methods'] if row['recommendation'] == 'candidate_for_review']
        require(len(candidates) >= 2, 'Two actually qualified immutable candidates are required to exercise switch and rollback; baseline/no candidate is not fabricated into eligibility')
        for candidate in candidates[:2]:
            evidence['versions'].append(approve_install(research_api, admin, review, profile, record['id'],
                                                       candidate['method'], deadline, directory))
        first, second = evidence['versions']
        operation(admin, profile_hash, {'kind': 'stop'}, deadline, directory)
        operation(admin, profile_hash, {'kind': 'start_candidate', 'candidate_hash': first['version']['id'],
                  'approval_id': first['approval']['approval_id']}, deadline, directory, profile['model_sha256'])
        started = healthy(admin, profile_hash, deadline)
        require(started['authority'] == 'monitor' and started['native_status']['armed'] is False
                and started['version'] == first['version']['model_sha256'],
                'Cold candidate start must select exact digest in Monitor without arming')
        evidence['cold_candidate_start'] = started['native_status']
        operation(admin, profile_hash, {'kind': 'arm'}, deadline, directory)
        operation(admin, profile_hash, {'kind': 'engage'}, deadline, directory)
        engaged = healthy(admin, profile_hash, deadline)
        require(engaged['authority'] == 'engaged', 'Checked native Engage did not establish authority')
        from verify_maintenance import blocked as maintenance_blocked, reconnect as maintenance_reconnect
        evidence['maintenance_denials'] = maintenance_blocked(admin, review, root, directory, profile)
        for label, entry in (('second-candidate', second), ('rollback-to-approved-first', first)):
            operation(admin, profile_hash, {'kind': 'switch_version', 'candidate_hash': entry['version']['id'],
                      'approval_id': entry['approval']['approval_id']}, deadline, directory)
            switched = healthy(admin, profile_hash, deadline)
            require(switched['version'] == entry['version']['model_sha256'], 'Native active digest did not become the approved immutable model')
            evidence.setdefault('switches', []).append({'label': label, 'native_status': switched['native_status']})
        evidence['performance'] = performance(admin, profile_hash, directory)
        from verify_publisher import verify as verify_publisher
        evidence['publisher'] = verify_publisher(research_api, admin, review, root, directory, profile, record, deadline)
        operation(admin, profile_hash, {'kind': 'exit'}, deadline, directory)
        exited = current(admin, profile_hash)
        require(exited['authority'] != 'engaged' and exited['native_status']['dcs_handoff'] is True,
                'Normal exit lacks independently confirmed DCS release')
        evidence['exit_native'] = exited['native_status']
        operation(admin, profile_hash, {'kind': 'stop'}, deadline, directory)
        operation(admin, profile_hash, {'kind': 'recover_monitor'}, deadline, directory, profile['model_sha256'])
        recovered = current(admin, profile_hash)
        require(recovered['authority'] == 'monitor' and recovered['native_status']['armed'] is False,
                'Recovery must restore Monitor only, not candidate control')
        evidence['recovery_native'] = recovered['native_status']
        from verify_runtime_recovery import verify as verify_recovery
        evidence['fault_recovery'] = verify_recovery(admin, root, directory, profile, deadline)
        evidence['maintenance_reconnect'] = maintenance_reconnect(admin, review, root, directory, profile, deadline)
        evidence['status'] = 'passed_runtime_api_only'
        return evidence
    except Exception as error:
        evidence.update(status='failed', error=str(error))
        raise
    finally:
        save(directory / 'manifest.json', evidence)
