"""Durable serial command ledger for the HMI-owned native twin (never a runner)."""
from contextlib import contextmanager
import copy
import json
import os
import uuid

from common import load_json
from twin_equipment import fields, identifier

MAX_OPERATIONS = 4096


def save_state(path, record):
    """Commit intent to stable storage before the HMI may issue a native command."""
    temporary = path.with_name(path.name + '.' + uuid.uuid4().hex + '.tmp')
    try:
        with temporary.open('x', encoding='utf-8') as stream:
            json.dump(record, stream, ensure_ascii=False, allow_nan=False, separators=(',', ':'))
            stream.flush()
            os.fsync(stream.fileno())
        temporary.replace(path)
        if os.name != 'nt':
            descriptor = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
    finally:
        temporary.unlink(missing_ok=True)


@contextmanager
def session_lock(root, session_id):
    from research_twin import session_dir
    directory = session_dir(root, session_id)
    if not directory.is_dir():
        raise ValueError('Missing twin session')
    path = directory / 'owner.lock'
    if path.is_symlink():
        raise ValueError('Unsafe twin owner lock')
    with path.open('a+b') as stream:
        if os.name == 'nt':
            import msvcrt
            if stream.tell() == 0:
                stream.write(b'\0')
                stream.flush()
            stream.seek(0)
            msvcrt.locking(stream.fileno(), msvcrt.LK_LOCK, 1)
        else:
            import fcntl
            fcntl.flock(stream, fcntl.LOCK_EX)
        try:
            yield
        finally:
            if os.name == 'nt':
                stream.seek(0)
                msvcrt.locking(stream.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                fcntl.flock(stream, fcntl.LOCK_UN)


def commit(root, record):
    from research import now
    from research_twin import session_dir
    record['sequence'] = record.get('sequence', 0) + 1
    record['updated_at'] = now()
    save_state(session_dir(root, record['id']) / 'session.json', record)


def operation_snapshot(record, operation_id):
    from research_twin import public
    identifier(operation_id)
    operation = record.get('operations', {}).get(operation_id)
    if operation is None:
        raise ValueError('Unknown twin operation ID; no command has been replayed')
    result = public(record)
    result['operation'] = copy.deepcopy(operation)
    return result


def control(root, payload):
    from research import now
    from research_twin import ACTIVE, get_session, public
    fields(payload, ('id', 'owner', 'command', 'operation_id'), ('batch_steps',))
    operation_id = identifier(payload['operation_id'])
    command = payload['command']
    if command not in ('start', 'pause', 'step', 'close'):
        raise ValueError('Unsupported twin control command')
    if command != 'start' and 'batch_steps' in payload:
        raise ValueError('Only start accepts batch_steps')
    batch_steps = payload.get('batch_steps', 128)
    if type(batch_steps) is not int or not 1 <= batch_steps <= 512:
        raise ValueError('Continuous batch_steps must be an integer 1..512')
    identity = {'command': command}
    if command == 'start':
        identity['batch_steps'] = batch_steps
    record = get_session(root, payload['id'], payload['owner'])
    operations = record.setdefault('operations', {})
    existing = operations.get(operation_id)
    if existing is not None:
        if existing['request'] != identity:
            raise ValueError('Operation ID already identifies a different command')
        return {'session': operation_snapshot(record, operation_id), 'dispatch': False}
    if len(operations) >= MAX_OPERATIONS:
        raise ValueError('Twin command ledger is full; completed operations are never evicted or replayed')
    operation = dict(id=operation_id, command=command, request=identity,
                     status='completed', accepted_at=now(), samples_before=record['samples'])
    reason = None
    if record['status'] not in ACTIVE:
        reason = 'Session is terminal; create a new independent run to execute more scans'
    elif record.get('owner') != payload['owner']:
        reason = 'Native owner is unavailable; startup reconciliation is required'
    elif record.get('pending_native') is not None:
        reason = 'A native batch is pending; pause is acknowledged only after that batch'
    elif command == 'step' and record.get('execution_status') == 'running':
        reason = 'Pause continuous execution before requesting one step'
    dispatch = False
    if reason:
        operation.update(status='rejected', reason=reason)
    elif command in ('step', 'close'):
        operation['status'] = 'accepted'
        dispatch = True
    else:
        record['execution_status'] = 'running' if command == 'start' else 'paused'
        record['status'] = record['execution_status']
        if command == 'start':
            record['batch_steps'] = batch_steps
    # Only the mutable command ledger changes; frozen manifests and archives do not.
    operations[operation_id] = operation
    record['operation'] = operation
    commit(root, record)
    return {'session': public(record), 'dispatch': dispatch}


def native_begin(root, payload):
    from research_twin import ACTIVE, get_session, public
    fields(payload, ('id', 'owner', 'expected_sequence', 'command'), ('operation_id',))
    record = get_session(root, payload['id'], payload['owner'])
    if record.get('pending_native') is not None:
        raise ValueError('A native command is already pending; never replay an unknown outcome')
    if record['status'] not in ACTIVE or record.get('owner') != payload['owner']:
        raise ValueError('Session has no active native owner')
    if type(payload['expected_sequence']) is not int or payload['expected_sequence'] != record.get('sequence', 0):
        raise ValueError('Native intent sequence is stale')
    command = copy.deepcopy(payload['command'])
    name = command.get('command') if isinstance(command, dict) else None
    if name == 'advance':
        fields(command, ('command', 'expected_samples', 'count'))
        if type(command['expected_samples']) is not int or command['expected_samples'] != record['samples']:
            raise ValueError('Native intent sample is stale')
        if type(command['count']) is not int or not 1 <= command['count'] <= 512:
            raise ValueError('Native intent count exceeds bounded batch')
    elif name in ('snapshot', 'close'):
        fields(command, ('command',))
    else:
        raise ValueError('Unsupported native intent')
    operation_id = payload.get('operation_id')
    if operation_id is not None:
        identifier(operation_id)
        operation = record.get('operations', {}).get(operation_id)
        if operation is None or operation['status'] != 'accepted':
            raise ValueError('Native command has no accepted operation')
        if (operation['command'], name) not in (('step', 'advance'), ('close', 'close')):
            raise ValueError('Native command differs from accepted operation')
        if operation['command'] == 'step' and command['count'] != 1:
            raise ValueError('Step must execute exactly one scan')
    record['native_sequence'] = record.get('native_sequence', 0) + 1
    command['request_id'] = str(record['native_sequence'])
    record['pending_native'] = dict(command, operation_id=operation_id)
    commit(root, record)
    return public(record)


def settle_native(record, response):
    """Called only after native payload/archive validation, before one atomic commit."""
    pending = record.get('pending_native')
    interrupted = response['status'] == 'interrupted'
    if pending and not interrupted and response.get('request_id') != pending['request_id']:
        raise ValueError('Native response does not match durable request identity')
    if pending and not interrupted:
        before = pending.get('expected_samples', record['samples'])
        maximum = before + pending.get('count', 0)
        if pending['command'] == 'advance' and response['samples'] > maximum:
            raise ValueError('Native response exceeds the accepted batch')
        if response['status'] in ('paused', 'completed'):
            expected = min(maximum, record['requested_samples']) if pending['command'] == 'advance' else before
            if response['samples'] != expected:
                raise ValueError('Native response did not complete the exact accepted batch')
    if pending and pending.get('operation_id'):
        operation = record['operations'][pending['operation_id']]
        operation['status'] = 'unknown' if interrupted else ('rejected' if response['status'] == 'failed' else 'completed')
        operation['samples_after'] = response['samples']
        if interrupted:
            operation['reason'] = response.get('reason', 'Native outcome is unknown; no replay')
        record['operation'] = operation
    record['pending_native'] = None
    terminal = response['status'] in ('completed', 'closed', 'failed', 'interrupted')
    record['execution_status'] = response['status'] if terminal else (
        'running' if record.get('execution_status') == 'running' else 'paused')
    record['status'] = record['execution_status']

    if interrupted:
        for operation in record.get('operations', {}).values():
            if operation['status'] == 'accepted':
                operation.update(status='unknown', reason=response.get('reason', 'Native outcome is unknown; no replay'))
        if record.get('operation'):
            record['operation'] = record.get('operations', {}).get(record['operation']['id'], record['operation'])


def recovered_snapshot(directory, record, result, scenario, hashes):
    from research_twin import MAX_HORIZON_BYTES, artifact, validate_native_state
    from research_trends import ForecastArchive
    controller = load_json(artifact(directory, 'controller.json'))
    trace, count = result['trace'], record['requested_samples']
    keys = ('time_seconds', 'cv', 'mv', 'dv', 'targets', 'command_mv', 'predicted_cv', 'quality')
    if any(not isinstance(trace.get(key), list) or len(trace[key]) != count for key in keys):
        raise ValueError('Finalized trace is incomplete')
    recent = {key: trace[key][-256:] for key in keys}
    statuses = trace.get('solver_status')
    if statuses is not None and (not isinstance(statuses, list) or len(statuses) != count):
        raise ValueError('Finalized solver status trace is incomplete')
    recent['solver_status'] = statuses[-256:] if statuses is not None else ['unavailable'] * min(count, 256)
    frame = {key: values[-1] for key, values in recent.items()}
    forecast = None
    if hashes:
        path = artifact(directory, 'forecast.ndjson')
        with path.open('rb') as stream:
            if len(stream.readline(8193)) > 8192:
                raise ValueError('Finalized forecast header exceeds bounded schema')
        archive = ForecastArchive(path, record['sample_time'],
                                  len(controller['variables']['cvs']), len(controller['variables']['mvs']))
        if len(archive.offsets) != count:
            raise ValueError('Finalized forecast archive does not cover every scan')
        with path.open('rb') as stream:
            stream.seek(archive.offsets[-1])
            if len(stream.readline(MAX_HORIZON_BYTES + 1)) > MAX_HORIZON_BYTES:
                raise ValueError('Finalized forecast exceeds bounded snapshot')
            row = archive.record(stream, count - 1)
        forecast = {key: archive.header[key] for key in ('schema_version', 'sample_time', 'cv_count', 'mv_count',
                    'prediction_horizon', 'control_horizon', 'layout', 'cv_first_offset_steps', 'mv_first_offset_steps')}
        forecast.update(row)
    elif record.get('native_sequence', 0):
        raise ValueError('Finalized native session is missing full forecasts')
    snapshot = dict(frame=frame, recent_trace=recent, latest_forecast=forecast, status='completed')
    validate_native_state(snapshot, count, controller, scenario)
    return snapshot


def reconcile(root, payload):
    """Startup-only reconciliation; GET and archive reads never call this function."""
    from research_twin import ACTIVE, artifact, fingerprint, forecast_hashes, get_session, session_dir, trace_evidence, verify_frozen_inputs
    fields(payload, ('owner',))
    reconciled = []
    directory = root / 'twin-sessions'
    if directory.is_symlink():
        raise ValueError('Unsafe twin sessions directory')
    for path in sorted(directory.glob('*/session.json')):
        initial = get_session(root, path.parent.name, payload['owner'])
        if initial['status'] not in ACTIVE or initial.get('owner') == payload['owner']:
            continue
        with session_lock(root, path.parent.name):
            record = get_session(root, path.parent.name, payload['owner'])
            if record['status'] not in ACTIVE or record.get('owner') == payload['owner']:
                continue
            frozen = session_dir(root, record['id'])
            try:
                verify_frozen_inputs(frozen, record)
                result = load_json(artifact(frozen, 'result.json'))
                scenario = load_json(artifact(frozen, 'scenario.json'))
                complete = result.get('status') == 'valid' and result.get('trace', {}).get('time_seconds') == scenario['time_seconds']
                hashes = forecast_hashes(frozen, 'forecast.ndjson') if complete else {}
                recovered = recovered_snapshot(frozen, record, result, scenario, hashes) if complete else {}
            except (ValueError, OSError):
                complete = False
            for operation in record.get('operations', {}).values():
                if operation['status'] == 'accepted':
                    operation.update(status='unknown', reason='HMI restarted without a native checkpoint; command is not replayed')
            if record.get('operation'):
                record['operation'] = record.get('operations', {}).get(record['operation']['id'], record['operation'])
            record['pending_native'] = None
            record['status'] = record['execution_status'] = 'completed' if complete else 'interrupted'
            record['reason'] = ('Recovered exact finalized native result; no command replay' if complete else
                                'HMI restarted; native state cannot be resumed or silently replayed')
            if complete:
                record.update(result_sha256=fingerprint(frozen / 'result.json'), forecast_hashes=hashes)
                record.update(recovered, samples=record['requested_samples'])
            record.update(trace_evidence(frozen))
            commit(root, record)
            reconciled.append(record['id'])
    return {'reconciled': reconciled}
