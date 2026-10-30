"""Strict local diagnostic configuration versions, separate from research evidence."""
import copy
import hashlib
import json
import math
import os
import re

from common import load_json, fingerprint

KINDS = {'silo', 'feeder', 'conveyor', 'ball_mill', 'vertical_mill', 'elevator', 'separator', 'collector', 'fan'}
CHANNELS = {'cv': {'actual': 'pv', 'target': 'target'},
            'mv': {'actual': 'applied_sp', 'command': 'sp', 'op': 'op', 'mode': 'mode', 'target': 'target'},
            'dv': {'actual': 'pv'}}
MAX_VERSIONS = 200


def encoded(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':'), allow_nan=False).encode('utf-8')


def fields(value, required, optional=()):
    if not isinstance(value, dict) or not set(required) <= set(value) or set(value) - set(required) - set(optional):
        raise ValueError('Missing or unsupported configuration fields')


def text(value, label, limit=160):
    if not isinstance(value, str) or not value.strip() or len(value) > limit or any(ord(c) < 32 for c in value):
        raise ValueError('Invalid ' + label)
    return value


def identifier(value):
    if not isinstance(value, str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_-]{0,95}', value):
        raise ValueError('Invalid configuration identifier')
    return value


def number(value, label, low=-1e12, high=1e12):
    if type(value) not in (int, float) or not math.isfinite(value) or not low <= value <= high:
        raise ValueError('Invalid finite ' + label)
    return value


def effective_variables(run, controller):
    result = copy.deepcopy(run['study']['config']['variables'])
    for role in ('cv', 'mv', 'dv'):
        for variable, model in zip(result[role], controller['variables'].get(role + 's', [])):
            variable.update({k: v for k, v in model['limits'].items() if k in ('low', 'high', 'target')})
            for key in ('weight', 'alpha', 'slack_weight', 'is_integrating', 'optimization_mode', 'weight_r', 'max_move', 'target', 'target_weight'):
                if key in model:
                    variable[key] = copy.deepcopy(model[key])
    return result


def settings_controller(original, variables):
    fields(variables, ('cvs', 'mvs', 'dvs'))
    allowed = {'cvs': {'limits', 'weight', 'alpha', 'slack_weight', 'is_integrating', 'optimization_mode'},
               'mvs': {'limits', 'weight_r', 'max_move', 'target', 'target_weight'}, 'dvs': set()}
    for role, mutable in allowed.items():
        rows, originals = variables[role], original['variables'].get(role, [])
        if not isinstance(rows, list) or len(rows) != len(originals):
            raise ValueError('Settings variable roles/counts differ')
        for row, before in zip(rows, originals):
            if not isinstance(row, dict) or {k: v for k, v in row.items() if k not in mutable} != {k: v for k, v in before.items() if k not in mutable}:
                raise ValueError('Settings cannot change variable identity, nodes or unsupported fields')
            if not (set(before) - {'target'}) <= set(row):
                raise ValueError('Settings must include the full controller variables')
            if role == 'dvs':
                continue  # Disturbance limits do not tune the native campaign controller.
            limits = row.get('limits')
            fields(limits, ('low', 'high', 'low_low', 'high_high'), ('target',) if role == 'cvs' else ())
            ll, low, high, hh = (number(limits[key], key) for key in ('low_low', 'low', 'high', 'high_high'))
            if not ll <= low < high <= hh:
                raise ValueError('Settings limits must be ordered')
            if role == 'cvs':
                target = number(limits.get('target'), 'CV target', low, high)
                for key in ('weight', 'slack_weight'):
                    number(row.get(key), key, 0)
                number(row.get('alpha'), 'alpha', 0, 1)
                if type(row.get('is_integrating')) is not bool:
                    raise ValueError('is_integrating must be boolean')
                mode = row.get('optimization_mode')
                if not isinstance(mode, dict) or mode.get('type') not in ('Target', 'Zone', 'Minimize', 'Maximize'):
                    raise ValueError('Invalid CV optimization mode')
                fields(mode, ('type', 'value') if mode['type'] == 'Target' else ('type',))
                if mode['type'] == 'Target' and number(mode['value'], 'optimization target') != target:
                    raise ValueError('Optimization target must match limits.target')
                if original.get('control_strategy', {}).get('method') == 'soft_band' and mode['type'] != 'Target':
                    raise ValueError('Soft-band controllers require Target optimization')
            else:
                if number(row.get('weight_r'), 'MV weight', 0) <= 0:
                    raise ValueError('Native campaign MV weight must be positive')
                if number(row.get('max_move'), 'max_move', 0) <= 0:
                    raise ValueError('max_move must be positive')
                weight = number(row.get('target_weight', 0), 'MV target weight', 0)
                if row.get('target') is not None:
                    number(row['target'], 'MV target', low, high)
                elif weight > 0:
                    raise ValueError('Diagnostic MV target weight requires an explicit target')
    result = copy.deepcopy(original)
    result['variables'] = copy.deepcopy(variables)
    return result


def validate_view(view, controller):
    fields(view, ('schema_version', 'label', 'layout', 'confirmed', 'equipment', 'connections', 'bindings'))
    if type(view['schema_version']) is not int or view['schema_version'] != 1 or view['layout'] not in ('closed_ball_mill', 'vertical_mill') or type(view['confirmed']) is not bool:
        raise ValueError('Unsupported equipment view schema/layout/confirmation')
    text(view['label'], 'view label')
    for key, maximum in (('equipment', 128), ('connections', 256), ('bindings', 512)):
        if not isinstance(view[key], list) or len(view[key]) > maximum:
            raise ValueError('Equipment view exceeds bounded item count')
    if not view['equipment']:
        raise ValueError('Equipment view requires equipment')
    ids = set()
    def unique(value):
        identifier(value)
        if value in ids:
            raise ValueError('Equipment view IDs must be unique')
        ids.add(value)
    equipment = set()
    for item in view['equipment']:
        fields(item, ('id', 'name', 'kind', 'position', 'size', 'rotation'))
        unique(item['id'])
        equipment.add(item['id'])
        text(item['name'], 'equipment name')
        if item['kind'] not in KINDS:
            raise ValueError('Unknown equipment kind')
        for key, bound in (('position', 1000), ('size', 100), ('rotation', math.tau)):
            values = item[key]
            if not isinstance(values, list) or len(values) != 3:
                raise ValueError('Geometry must contain three finite coordinates')
            for value in values:
                number(value, key, .01 if key == 'size' else -bound, bound)
    bindings = set()
    for binding in view['bindings']:
        fields(binding, ('id', 'equipment_id', 'role', 'variable_name', 'unit', 'channel', 'animation'), ('node_id', 'reference_value'))
        unique(binding['id'])
        bindings.add(binding['id'])
        if binding['equipment_id'] not in equipment or binding['animation'] not in ('none', 'rotation', 'flow'):
            raise ValueError('Invalid equipment binding or animation')
        role, channel = binding['role'], binding['channel']
        if role == 'telemetry':
            if channel not in ('actual', 'op', 'mode', 'running', 'fault'):
                raise ValueError('Unsupported telemetry channel')
            text(binding['variable_name'], 'telemetry name')
            if not isinstance(binding['unit'], str) or len(binding['unit']) > 80:
                raise ValueError('Invalid telemetry unit')
            if channel in ('running', 'fault') and binding['unit'] not in ('bool', 'boolean'):
                raise ValueError('Running/fault telemetry requires boolean units')
            if channel == 'mode' and binding['unit'] != 'mode':
                raise ValueError('Mode telemetry requires mode units')
            node = text(binding.get('node_id'), 'explicit telemetry node', 512)
            if '://' in node or '/' in node or '\\' in node or '..' in node:
                raise ValueError('Telemetry nodes cannot be URLs or file paths')
        elif role in CHANNELS and channel in CHANNELS[role]:
            matches = [v for v in controller['variables'].get(role + 's', []) if v['name'] == binding['variable_name']]
            if len(matches) != 1:
                raise ValueError('Binding requires exact unique controller role/name/unit')
            if channel == 'op':
                text(binding['unit'], 'separate OP engineering unit', 80)
            elif channel == 'mode':
                if binding['unit'] != 'mode':
                    raise ValueError('Mode binding requires mode units')
            elif matches[0]['units'] != binding['unit']:
                raise ValueError('Binding requires exact controller engineering unit')
            node_key = CHANNELS[role][channel]
            nodes = matches[0].get('node_ids', {})
            expected_node = nodes.get(node_key)
            if role == 'mv' and channel == 'actual' and not expected_node:
                expected_node = nodes.get('sp')  # Weaker SP register readback, never PID OP.
            if 'node_id' in binding and binding['node_id'] != expected_node:
                raise ValueError('Binding node differs from exact controller channel mapping')
            if 'node_id' in binding and not isinstance(binding['node_id'], str):
                raise ValueError('Binding node must be an explicit controller node')
        else:
            raise ValueError('Unsupported role/channel binding')
        if 'reference_value' in binding:
            if number(binding['reference_value'], 'animation reference', 0) <= 0:
                raise ValueError('Animation reference must be positive')
        if binding['animation'] != 'none' and ('reference_value' not in binding or channel not in ('actual', 'op')):
            raise ValueError('Animation requires a measured channel and positive reference')
    for connection in view['connections']:
        fields(connection, ('id', 'from', 'to', 'kind'), ('binding_id',))
        unique(connection['id'])
        if connection['from'] not in equipment or connection['to'] not in equipment or connection['from'] == connection['to'] or connection['kind'] not in ('material', 'air'):
            raise ValueError('Invalid equipment connection')
        if 'binding_id' in connection and connection['binding_id'] not in bindings:
            raise ValueError('Connection binding does not exist')
    return view


def version_directory(root, kind):
    directory = root / ('twin-' + kind)
    if directory.is_symlink():
        raise ValueError('Unsafe twin version directory')
    return directory


def save_version(root, kind, version, source_digest):
    directory = version_directory(root, kind)
    directory.mkdir(parents=True, exist_ok=True)
    if sum(1 for _ in directory.glob('*.json')) >= MAX_VERSIONS:
        raise ValueError('Twin version count limit reached')
    envelope = {'version': version, 'source_controller_sha256': source_digest}
    content = encoded(envelope)
    digest = hashlib.sha256(content).hexdigest()
    with (directory / (digest + '.json')).open('xb') as stream:
        stream.write(content)
        stream.flush()
        os.fsync(stream.fileno())
    if os.name != 'nt':
        descriptor = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    return dict(copy.deepcopy(version), id=digest)


def load_version(root, kind, version_id, run_id, trial_id, source_digest):
    from research_twin import artifact
    if not isinstance(version_id, str) or not re.fullmatch('[0-9a-f]{64}', version_id):
        raise ValueError('Invalid immutable twin version ID')
    path = artifact(version_directory(root, kind), version_id + '.json')
    if fingerprint(path) != version_id:
        raise ValueError('Twin version content integrity mismatch')
    envelope = load_json(path)
    version = envelope['version']
    if version['run_id'] != run_id or version['trial_id'] != trial_id or envelope['source_controller_sha256'] != source_digest:
        raise ValueError('Twin version belongs to another run/trial/controller')
    if kind == 'settings' and hashlib.sha256(encoded(version['controller'])).hexdigest() != version['controller_sha256']:
        raise ValueError('Twin settings controller integrity mismatch')
    return dict(version, id=version_id)


def configuration(root, payload):
    from research_twin import selected_trial
    run, _, trial, path = selected_trial(root, payload['id'], payload['trial_id'])
    digest = fingerprint(path)
    result = {'controller': load_json(path), 'controller_sha256': digest}
    for kind, key in (('equipment-views', 'equipment_views'), ('settings', 'settings_versions')):
        versions = []
        for item in sorted(version_directory(root, kind).glob('*.json')):
            # Read metadata only to filter; matching entries are fully hash-verified below.
            from research_twin import artifact
            envelope = load_json(artifact(item.parent, item.name))
            row = envelope.get('version', {})
            if row.get('run_id') == run['id'] and row.get('trial_id') == trial['id']:
                versions.append(load_version(root, kind, item.stem, run['id'], trial['id'], digest))
        result[key] = versions
    return result


def create_version(root, payload, settings=False):
    from research import now
    from research_twin import artifact, selected_trial
    fields(payload, ('run_id', 'trial_id', 'name', 'variables', 'owner') if settings else ('run_id', 'trial_id', 'view', 'owner'))
    run, directory, trial, path = selected_trial(root, payload['run_id'], payload['trial_id'])
    original = load_json(path)
    digest = fingerprint(path)
    version = dict(run_id=run['id'], trial_id=trial['id'], created_at=now())
    if settings:
        controller = settings_controller(original, payload['variables'])
        plant = load_json(artifact(directory, 'plant.json'))
        for variable, baseline in zip(controller['variables']['mvs'], plant['baseline_mv']):
            if not variable['limits']['low'] <= baseline <= variable['limits']['high']:
                raise ValueError('Diagnostic MV limits must contain the frozen physical baseline')
        version.update(name=text(payload['name'], 'settings name'), controller=controller,
                       controller_sha256=hashlib.sha256(encoded(controller)).hexdigest(),
                       variables=effective_variables(run, controller), diagnostic_only=True)
    else:
        version.update(view=validate_view(payload['view'], original), controller_sha256=digest)
    return save_version(root, 'settings' if settings else 'equipment-views', version, digest)
