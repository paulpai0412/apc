"""R7 actual resource observations and independently gated OPC reconnection.

Only the disposable administrator receives runtime.maintenance. Reviewer and a
real Viewer login remain denied. Queued HTTP202 is never connection evidence.
"""
from datetime import datetime, timezone
import os
import secrets
import time
import uuid

from workspace import require, save
from verify_api import Api
from verify_runtime import current

PATH = '/api/runtime/maintenance'
RECONNECT = '/api/infrastructure/opc-reconnect'


def resources(snapshot, root):
    storage = snapshot['storage']
    database = storage['database']
    require(database['status'] == 'observed' and database['page_count'] > 0 and database['page_size'] > 0,
            'R7 did not observe actual SQLite capacity')
    require(database['allocated_bytes'] == database['page_count'] * database['page_size']
            and database['free_page_bytes'] == database['freelist_pages'] * database['page_size'],
            'R7 SQLite capacity arithmetic does not describe observed pages')
    require(database['main_file'] == str(root / 'identities.db'), 'R7 database is not the owned shared identity/runtime database')
    require(storage['recursive_scan'] is False and storage['directory_total_bytes'] is None,
            'R7 must not infer recursive directory usage or remote retention')
    profile_path = root / 'runtime/profiles.json'
    observed = next(row for row in storage['paths'] if row['name'] == 'commissioning_profile')
    require(observed['configured_path'] == str(profile_path) and observed['kind'] == 'file'
            and observed['file_bytes'] == profile_path.stat().st_size, 'R7 commissioning metadata differs from actual immutable local file')
    capacity = observed['filesystem']
    actual = os.statvfs(profile_path)
    require(capacity['status'] == 'observed' and capacity['total_bytes'] == actual.f_blocks * actual.f_frsize
            and 0 <= capacity['available_bytes'] <= capacity['total_bytes'], 'R7 filesystem capacity is unavailable or not actual local filesystem evidence')
    require(snapshot['retention']['status'] == 'not_configured_in_hmi' and snapshot['retention']['historian'] is None,
            'R7 invented an external historian retention policy')
    connection = snapshot['connection']
    require(0 <= connection['command_channel_available_capacity'] <= connection['command_channel_max_capacity']
            and connection['command_channel_max_capacity'] > 0, 'R7 omitted actual bounded worker channel capacity')
    require(snapshot['limits']['historian']['enabled'] is True and snapshot['limits']['profile_count'] == 1,
            'R7 resource/configuration observations differ from isolated runtime fixture')


def blocked(admin, reviewer, root, output, profile):
    evidence = {'status': 'running'}
    directory = output / 'maintenance'
    directory.mkdir()
    try:
        snapshot = admin.call(PATH)
        resources(snapshot, root)
        native_before = current(admin, profile['profile_hash'])
        require(native_before['authority'] == 'engaged', 'R7 rejection must be exercised during real native Engage')
        require(snapshot['can_reconnect'] is False and snapshot['blockers'], 'Maintenance overview incorrectly permits reconnect while engaged')
        evidence['engaged_rejection'] = admin.call(RECONNECT, {}, expected_error=(409,))
        connection_after = admin.call(PATH)['connection']
        native_after = current(admin, profile['profile_hash'])
        require(connection_after['worker']['generation'] == snapshot['connection']['worker']['generation'],
                'Rejected maintenance request changed the OPC connection generation')
        require(all(native_after[key] == native_before[key] for key in ('instance_id', 'version', 'authority')),
                'Rejected maintenance request changed native ownership, version or authority')
        evidence['unchanged_after_rejection'] = {'connection': connection_after, 'native': native_after}
        evidence['ungranted_admin_read_rejection'] = reviewer.call(PATH, expected_error=(403,))
        evidence['ungranted_admin_mutation_rejection'] = reviewer.call(RECONNECT, {}, expected_error=(403,))
        # Exercise a real lower base role as well as the independent administrator
        # without scoped grant. Never modify/revoke the reviewer used elsewhere.
        username = 'cement-viewer-' + uuid.uuid4().hex[:12]
        initial, password = secrets.token_urlsafe(32), secrets.token_urlsafe(32)
        admin.call('/api/admin/users', {'username': username, 'password': initial, 'role': 'viewer'})
        viewer = Api(admin.url, {'username': username, 'password': initial})
        viewer.call('/api/auth/change-password', {'current_password': initial, 'new_password': password})
        viewer = Api(admin.url, {'username': username, 'password': password})
        save(directory / 'owned-viewer-credentials.json', {'username': username, 'password': password, 'scope': 'disposable_test_only'})
        evidence['viewer_read_rejection'] = viewer.call(PATH, expected_error=(403,))
        evidence['viewer_mutation_rejection'] = viewer.call(RECONNECT, {}, expected_error=(403,))
        evidence.update(status='passed_engaged_and_scope_denials', actual_resources=snapshot)
        return evidence
    except Exception as error:
        evidence.update(status='failed', error=str(error))
        raise
    finally:
        save(directory / 'denials.json', evidence)


def reconnect(admin, reviewer, root, output, profile, deadline):
    evidence = {'status': 'running'}
    directory = output / 'maintenance'
    try:
        while True:
            before = admin.call(PATH)
            if before['can_reconnect']:
                break
            require(time.monotonic() < deadline, 'R7 reconnect lacks independently safe live Monitor baseline: ' + str(before['blockers']))
            time.sleep(1)
        resources(before, root)
        native_before = current(admin, profile['profile_hash'])
        require(native_before['authority'] == 'monitor' and native_before['native_status']['armed'] is False
                and native_before['native_status']['safe_baseline'] is True
                and native_before['native_status']['dcs_handoff'] is True,
                'R7 reconnect requires fresh independently confirmed cessation and DCS release, not merely process absence')
        # Missing scope remains forbidden even when all safety gates are healthy.
        reviewer.call(RECONNECT, {}, expected_error=(403,))
        queued = admin.call(RECONNECT, {}, expected_status=202)
        require(queued['state'] == 'queued' and queued['confirmed'] is False and queued['connected'] is None,
                'Queue acceptance was mislabeled as connection success')
        evidence['queued_not_confirmed'] = queued
        baseline_generation = queued['baseline_connection']['generation']
        queued_at = datetime.fromisoformat(queued['queued_at'].replace('Z', '+00:00'))
        while True:
            observed = admin.call(PATH)
            connection = observed['connection']
            worker = connection['worker']
            endpoints = [row for row in connection['observed_endpoints'] if row['endpoint'] == profile['endpoint']]
            if worker['reported_connected'] is True and worker['generation'] > baseline_generation and worker['observed_at'] and endpoints:
                connected_at = datetime.fromisoformat(worker['observed_at'].replace('Z', '+00:00'))
                source_at = endpoints[0]['last_source_timestamp']
                if source_at:
                    source_at = datetime.fromisoformat(source_at.replace('Z', '+00:00'))
                    age_ms = (datetime.now(timezone.utc) - source_at).total_seconds() * 1000
                    if connected_at >= queued_at and -profile['maximum_skew_ms'] <= age_ms <= profile['maximum_age_ms']:
                        break
            require(time.monotonic() < deadline, 'R7 reconnect remains unconfirmed; no new connected generation with actual fresh source observations')
            time.sleep(1)
        after = current(admin, profile['profile_hash'])
        require(after['instance_id'] == native_before['instance_id'] and after['version'] == native_before['version']
                and after['authority'] == 'monitor' and after['native_status']['armed'] is False
                and after['native_status']['dcs_handoff'] is True,
                'OPC maintenance reconnect altered native controller ownership/version or automatically armed control')
        resources(observed, root)
        evidence.update(status='passed_actual_reconnect', before=before, independently_observed_connection=observed,
                        unchanged_native=after['native_status'])
        return evidence
    except Exception as error:
        evidence.update(status='failed', error=str(error))
        raise
    finally:
        save(directory / 'reconnect.json', evidence)
