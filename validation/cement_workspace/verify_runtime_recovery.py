"""Owned-process faults with actual unknown outcomes and independent safe recovery.

The explicit acceptance command withholds ONLY acquisition publication from the
launcher-recorded causal fixture, whose physical clock and OPC client stay live.
The simulator itself bounds the outage, independently of verifier/HTTP progress.
We interrupt ONLY the supervisor-listed, ownership-verified native engine via
Linux pidfds during checked Exit, and request publication resume even on failure.
No journals, acknowledgments, source timestamps or operation outcomes are edited.
"""
from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import signal
import select
import time
import urllib.request
import uuid

from workspace import require, save
from verify_runtime import current, healthy, operation, stamp


def producer_handle(root):
    require(hasattr(os, 'pidfd_open') and hasattr(signal, 'pidfd_send_signal'),
            'Real fault acceptance requires Linux pidfd_open/pidfd_send_signal; unavailable is not pass')
    owner = json.loads((root / 'owner.json').read_text())
    require(owner['kind'] == 'authrity-disposable-cement-workspace', 'Fault injection requires this launcher-owned disposable root')
    expected = (root / 'source/apc_engine/target/release/replay_plant').resolve()
    for receipt_path in sorted(root.glob('launch-*.json'), reverse=True):
        receipt = json.loads(receipt_path.read_text())
        require(receipt['root'] == str(root), 'Launcher receipt root differs')
        for process in receipt['processes']:
            if process['name'] != 'cement-causal-plant':
                continue
            handle = None
            try:
                handle = os.pidfd_open(process['pid'])
                proc = Path('/proc') / str(process['pid'])
                args = (proc / 'cmdline').read_bytes().split(b'\0')
                outage_flag = b'--acquisition-outage-secs'
                require(args.count(outage_flag) == 1
                        and args[args.index(outage_flag) + 1:args.index(outage_flag) + 2] == [b'12'],
                        'Owned producer lacks exact --acquisition-outage-secs 12 opt-in; refusing signals')
                require((proc / 'exe').resolve() == expected and (proc / 'cwd').resolve() == root / 'runtime'
                        and str(root / 'runtime/profile.json').encode() in args
                        and b'--acknowledge-isolated-simulator' in args,
                        'Owned producer PID identity changed; refusing signals')
                require(not select.select([handle], [], [], 0)[0], 'Owned producer already exited; refusing signals')
                return handle, {'pid': process['pid'], 'executable': str(expected), 'receipt': str(receipt_path),
                                'acquisition_outage_secs': 12, 'fault_opt_in': '--acquisition-outage-secs 12'}
            except FileNotFoundError:
                if handle is not None:
                    os.close(handle)
            except Exception:
                if handle is not None:
                    os.close(handle)
                raise
    raise RuntimeError('No live verified launcher-owned causal producer; no fault was injected')


def engine_handle(api, root, profile, expected):
    config = json.loads((root / 'workspace.json').read_text())
    password = json.loads((root / 'test-credentials.json').read_text())['password']
    url = f'http://127.0.0.1:{config["supervisor_port"]}/api/controllers'
    request = urllib.request.Request(url, headers={'x-api-key': password})
    with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=30) as response:
        controllers = json.load(response)
    controller = next(row for row in controllers if row['id'] == profile['engine_id'])
    state = controller['state']
    require(isinstance(state, dict) and isinstance(state.get('Running'), int)
            and state['Running'] > 0 and controller['model_sha256'] == expected['version'],
            'Supervisor does not identify the exact running model for owned fault injection')
    pid = state['Running']
    handle = os.pidfd_open(pid)
    try:
        proc = Path('/proc') / str(pid)
        executable = (root / 'source/apc_engine/target/release/authrity-apc-engine').resolve()
        args = (proc / 'cmdline').read_bytes().split(b'\0')
        require((proc / 'exe').resolve() == executable and (proc / 'cwd').resolve().is_relative_to(root),
                'Native engine PID is outside the owned validation workspace')
        profile_path = Path(os.fsdecode(args[args.index(b'--commissioning-profile') + 1])).resolve()
        socket_path = Path(os.fsdecode(args[args.index(b'--authority-socket') + 1])).resolve()
        require(profile_path.is_relative_to((root / 'runtime/authority').resolve())
                and socket_path.parent == profile_path.parent
                and json.loads(profile_path.read_text())['profile_hash'] == profile['profile_hash'],
                'Native PID does not carry this exact private commissioning authority')
        observed = current(api, profile['profile_hash'])
        require(all(observed[key] == expected[key] for key in ('instance_id', 'version', 'epoch', 'authority'))
                and observed['native_status']['healthy'] is True and observed['native_status']['armed'] is True,
                'Native authority changed while binding the owned fault target')
        return handle, {'pid': pid, 'executable': str(executable), 'profile_path': str(profile_path),
                        'instance_id': observed['instance_id'], 'supervisor_inventory': url}
    except Exception:
        os.close(handle)
        raise


def source_frame(api, profile, deadline, handle, *, after=None, resumed_at=None):
    """Observe a committed fixture frame, not native health or physical progress."""
    tags = {profile['handoff']['acknowledgement_tag']}
    for variable in profile['variables']:
        tags.update(variable[key] for key in ('pv_tag', 'applied_tag', 'op_tag', 'mode_tag') if variable.get(key))
    tags.update(profile['performance']['sampled_tags'].values())
    tags.update(profile['performance']['regime_tags'])
    sequence_tag = profile['acquisition_sequence_node']
    while True:
        require(not select.select([handle], [], [], 0)[0], 'Same owned producer pidfd exited during publication recovery')
        row = current(api, profile['profile_hash'])
        signals = {sample['node_id']: sample for sample in row['signals']}
        required = tags | {sequence_tag}
        if required.issubset(signals):
            samples = [signals[tag] for tag in sorted(required)]
            times = {tag: datetime.fromisoformat(signals[tag]['timestamp'].replace('Z', '+00:00')) for tag in required}
            acquired_at = min(times[tag] for tag in tags)
            committed_at = times[sequence_tag]
            sequence = signals[sequence_tag]['value']
            now = datetime.now(timezone.utc)
            coherent = (all(sample['source_endpoint'] == profile['endpoint']
                            and sample['status'] & 0xc0000000 == 0 for sample in samples)
                        and all(times[tag] == acquired_at for tag in tags)
                        and 0 <= (committed_at - acquired_at).total_seconds() * 1000 <= profile['maximum_skew_ms']
                        and all(0 <= (now - value).total_seconds() * 1000 <= profile['maximum_age_ms'] for value in times.values())
                        and isinstance(sequence, (int, float)) and not isinstance(sequence, bool)
                        and 0 < sequence <= 9007199254740991 and sequence % 2 == 0)
            if coherent and (after is None or (
                    sequence > after['acquisition_sequence']
                    and acquired_at > datetime.fromisoformat(after['source_timestamp'].replace('Z', '+00:00'))
                    and committed_at > datetime.fromisoformat(after['sequence_source_timestamp'].replace('Z', '+00:00'))
                    and resumed_at is not None and acquired_at > resumed_at)):
                require(not select.select([handle], [], [], 0)[0], 'Owned producer exited while observing resumed source frame')
                return {'profile_hash': profile['profile_hash'], 'endpoint': profile['endpoint'],
                        'acquisition_sequence': sequence, 'source_timestamp': stamp(acquired_at),
                        'sequence_source_timestamp': signals[sequence_tag]['timestamp'],
                        'observed_at': stamp(now), 'same_producer_pidfd_live': True,
                        'good_coherent_source_frame': True, 'signals': samples,
                        'scope': 'Publication evidence only; not physical catch-up or completed simulation'}
        require(time.monotonic() < deadline, 'No fresh good/coherent committed frame from the exact profile endpoint')
        time.sleep(0.2)


def verify(api, root, output, profile, deadline):
    directory = output / 'fault-recovery'
    directory.mkdir()
    evidence = {'status': 'running', 'factory_qualification': False}
    handle = None
    engine = None
    outage_requested = False
    try:
        healthy(api, profile['profile_hash'], deadline)
        operation(api, profile['profile_hash'], {'kind': 'arm'}, deadline, directory)
        operation(api, profile['profile_hash'], {'kind': 'engage'}, deadline, directory)
        engaged = healthy(api, profile['profile_hash'], deadline)
        require(engaged['authority'] == 'engaged' and engaged['native_status']['dcs_handoff'] is False,
                'Fault test requires genuinely acquired native authority before withholding release acknowledgment')
        handle, evidence['owned_producer'] = producer_handle(root)
        evidence['source_before_outage'] = source_frame(api, profile, deadline, handle)
        engine, evidence['owned_engine'] = engine_handle(api, root, profile, engaged)
        evidence['publication_outage'] = {'begin_signal': 'SIGUSR1 via verified pidfd',
                                         'resume_signal': 'SIGUSR2 via same verified pidfd',
                                         'configured_maximum_seconds': 12,
                                         'bound_owner': 'simulator monotonic deadline, not HTTP timeout',
                                         'begin_requested_at': stamp()}
        signal.pidfd_send_signal(handle, signal.SIGUSR1)
        outage_requested = True
        intent = {'operation_id': 'cement-fault-exit-' + uuid.uuid4().hex, 'profile_hash': profile['profile_hash'],
                  'expected_version': engaged['version'], 'expected_epoch': engaged['epoch'],
                  'expected_instance': engaged['instance_id'],
                  'expires_at': stamp(datetime.now(timezone.utc) + timedelta(seconds=295)),
                  'reason': 'Explicit owned-fixture interrupted checked Exit; never fabricate original outcome', 'action': {'kind': 'exit'}}
        save(directory / 'interrupted-exit-intent.json', intent)
        accepted_intent = api.call('/api/runtime/operations', intent)
        require(accepted_intent['state'] == 'executing', 'Fault requires a durably executing checked operation, not an already settled command')
        evidence['durably_executing_exit'] = accepted_intent
        signal.pidfd_send_signal(engine, signal.SIGKILL)
        require(bool(select.select([engine], [], [], 10)[0]), 'Interrupted owned engine exit is unconfirmed')
        evidence['actual_native_process_fault'] = {'signal': 'SIGKILL via verified pidfd', 'exit_confirmed': True,
                                                   'original_operation_id': intent['operation_id']}
        # Withheld publication must produce a real active stale condition; mark-read
        # is exercised while active and cannot clear or acknowledge it.
        condition_deadline = min(deadline, time.monotonic() + 30)
        while True:
            events = api.call('/api/runtime/events')
            stale = next((row for row in events['events'] if row['profile_hash'] == profile['profile_hash']
                          and row.get('condition_key') == 'runtime.acquisition_stale' and row.get('condition_active') is True), None)
            if stale is not None:
                break
            require(time.monotonic() < condition_deadline, 'Withheld acquisition publication did not raise the real source-time stale condition')
            time.sleep(1)
        api.call('/api/runtime/events/' + str(stale['id']) + '/read', {'note': 'Acceptance read acknowledgment is not condition clearance or DCS alarm acknowledgment'})
        marked = next(row for row in api.call('/api/runtime/events')['events'] if row['id'] == stale['id'])
        require(marked['read_at'] is not None and marked['condition_active'] is True, 'Read acknowledgment improperly cleared active acquisition condition')
        evidence['active_stale_read_only'] = marked
        resumed_at = datetime.now(timezone.utc)
        signal.pidfd_send_signal(handle, signal.SIGUSR2)
        outage_requested = False
        evidence['publication_outage']['resume_requested_at'] = stamp(resumed_at)
        evidence['source_after_resume'] = source_frame(
            api, profile, deadline, handle, after=evidence['source_before_outage'], resumed_at=resumed_at)
        while True:
            original = api.call('/api/runtime/operations/' + intent['operation_id'])
            if original['state'] != 'executing':
                break
            require(time.monotonic() < deadline, 'Interrupted original operation never settled to an observable outcome')
            time.sleep(1)
        require(original['state'] == 'unknown' and original['blocking'] is True,
                'Actual interrupted operation did not remain unknown/blocking; no outcome will be fabricated: ' + json.dumps(original))
        evidence['unknown_before_clearance'] = original
        operation(api, profile['profile_hash'], {'kind': 'recover_monitor'}, deadline, directory, profile['model_sha256'])
        # Recovery starts Monitor only. Wait for genuinely sampled local PID mode,
        # release acknowledgment and cessation; never substitute warmup completion
        # for the independently observed safe baseline.
        while True:
            recovered = current(api, profile['profile_hash'])
            native = recovered.get('native_status') or {}
            if native.get('safe_baseline') is True and native.get('dcs_handoff') is True and native.get('armed') is False:
                break
            require(time.monotonic() < deadline, 'Recovered native process has not independently established safe DCS baseline')
            time.sleep(1)
        require(recovered['instance_id'] != engaged['instance_id'] and recovered['authority'] == 'monitor', 'Recovery did not create a distinct Monitor-only authority instance')
        clearance = {'clearance_id': 'cement-clearance-' + uuid.uuid4().hex, 'profile_hash': profile['profile_hash'],
                     'operation_ids': [intent['operation_id']], 'expected_instance': recovered['instance_id'],
                     'expected_epoch': recovered['epoch'], 'expected_version': recovered['version'],
                     'expires_at': stamp(datetime.now(timezone.utc) + timedelta(seconds=295)),
                     'reason': 'Independent current safe-baseline evidence clears only interlock; original outcome remains unknown'}
        save(directory / 'clearance-intent.json', clearance)
        receipt = api.call('/api/runtime/clearances', clearance)
        duplicate = api.call('/api/runtime/clearances', clearance)
        require(receipt['outcome_changed'] is False and duplicate['clearance_id'] == receipt['clearance_id'], 'Clearance changed history or lost durable idempotency')
        historical = api.call('/api/runtime/operations/' + intent['operation_id'])
        require(historical['state'] == 'unknown' and historical['blocking'] is False
                and historical['clearance_id'] == clearance['clearance_id'], 'Safe clearance must retain unknown history while releasing only current interlock')
        while True:
            condition = next(row for row in api.call('/api/runtime/events')['events'] if row['id'] == stale['id'])
            if condition['condition_active'] is False:
                require(condition['cleared_at'] is not None, 'Recovered condition lost independent clear timestamp')
                break
            require(time.monotonic() < deadline, 'Fresh source recovery did not independently clear stale acquisition condition')
            time.sleep(1)
        evidence.update(status='passed_real_fault_recovery', clearance=receipt, historical_operation=historical,
                        recovered_native=native, cleared_condition=condition)
        return evidence
    except Exception as error:
        evidence.update(status='failed', error=str(error))
        raise
    finally:
        if handle is not None:
            try:
                if outage_requested:
                    signal.pidfd_send_signal(handle, signal.SIGUSR2)
                    evidence['publication_outage']['finally_resume_requested_at'] = stamp()
            except OSError as error:
                evidence['publication_outage']['finally_resume_error'] = str(error)
            finally:
                os.close(handle)
        if engine is not None:
            os.close(engine)
        save(directory / 'manifest.json', evidence)
