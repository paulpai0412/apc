"""Owned loopback cement Runtime fixture, distinct from quarter research evidence.

Uses the actual replay_plant OPC adapter and native StepResponseTwin, NOT a
pre-recorded CV stream. Controller Ts=60s matches quarter candidates; the physical
plant acquires at 1s with ORIGINAL time constants and dead times in seconds.
The owned producer opts into a bounded acquisition-publication outage for fault
acceptance; physical simulation and the OPC client continue throughout it.
This ideal-actuator fixture cannot establish ABB/DCS commissioning or factory improvement.

The fixture author explicitly declares a synthetic closed-ball-mill schematic,
not measured factory geometry. Only source-owned ideal feed/separator OP drives
animation; the mill and fan remain static. The complete schema-v1 view is bound
to the generated baseline model before the native profile hash is calculated.
"""
from __future__ import annotations
import copy
from datetime import datetime, timedelta, timezone
import hashlib
import json
import math
from pathlib import Path
import shutil
import sys
import time
import urllib.error
import urllib.request
import uuid

from workspace import command, require, save, sha, tool

ENGINE = 'CementAcceptance'
NAMESPACE = 'urn:ModelPredictiveControlServer:Model'


def curve(path, count=48, sample_time=60.0):
    values = []
    for sample in range(count):
        t = max(0.0, sample * sample_time - path['delay_seconds'])
        a, b = path['tau1'], path['tau2']
        response = 1 - (1 + t / a) * math.exp(-t / a) if abs(a - b) < 1e-9 else 1 - (a * math.exp(-t / a) - b * math.exp(-t / b)) / (a - b)
        values.append(path['gain'] * response)
    return values


def certificate(root, directory, name, uri, env):
    directory.mkdir(parents=True, exist_ok=True)
    pem, key, der = directory / (name + '.pem'), directory / (name + '-key.pem'), directory / (name + '.der')
    command(root, [tool('openssl'), 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '30',
                  '-subj', '/CN=' + name, '-addext', 'subjectAltName=URI:' + uri + ',IP:127.0.0.1,DNS:localhost',
                  '-addext', 'keyUsage=critical,digitalSignature,keyEncipherment,dataEncipherment,keyCertSign',
                  '-addext', 'extendedKeyUsage=serverAuth,clientAuth', '-keyout', key, '-out', pem], env=env, label=name + '-local-certificate')
    command(root, [tool('openssl'), 'x509', '-in', pem, '-outform', 'DER', '-out', der], env=env, label=name + '-certificate-der')
    key.chmod(0o600)
    return der, key


def synthetic_equipment_view(model, plant):
    from twin_equipment import validate_view

    # This is a declaration for this fixture, never a template inferred from an
    # arbitrary deployed model. Fail closed if its engineering identity changes.
    equipment_id = 'cement-mill-acceptance'
    declared = {
        'cv': [('ProductFineness', 'm2/kg', equipment_id + '-separator'),
               ('MillDifferentialPressure', 'mbar', equipment_id)],
        'mv': [('FreshFeed', 't/h', equipment_id + '-feed'),
               ('SeparatorSpeed', 'rpm', equipment_id + '-separator')],
        'dv': [('ClinkerHardness', 'kWh/t', equipment_id + '-feed'),
               ('FeedMoisture', '%', equipment_id + '-feed')],
    }
    require(model['metadata']['name'] == ENGINE, 'Synthetic equipment requires the cement fixture controller')
    for role, expected in declared.items():
        require([(v['name'], v['units']) for v in model['variables'][role + 's']]
                == [(name, unit) for name, unit, _ in expected],
                'Synthetic equipment requires the exact cement fixture roles, names and units')

    view = {
        'schema_version': 1,
        'label': 'Synthetic cement ideal-actuator schematic; not factory geometry, commissioning or ABB emulation',
        'layout': 'closed_ball_mill', 'confirmed': True,
        'equipment': [
            {'id': equipment_id + '-feed', 'name': 'Synthetic fresh-feed actuator', 'kind': 'feeder',
             'position': [-9, 0, 0], 'size': [3, 3, 3], 'rotation': [0, 0, 0]},
            {'id': equipment_id, 'name': 'Synthetic ball mill (unmeasured rotation; static)', 'kind': 'ball_mill',
             'position': [0, 0, 0], 'size': [6, 4, 4], 'rotation': [0, 0, 0]},
            {'id': equipment_id + '-separator', 'name': 'Synthetic separator ideal actuator', 'kind': 'separator',
             'position': [8, 0, 0], 'size': [3, 6, 3], 'rotation': [0, 0, 0]},
            {'id': equipment_id + '-fan', 'name': 'Illustrative fan (unmeasured; static)', 'kind': 'fan',
             'position': [8, 0, 8], 'size': [3, 3, 3], 'rotation': [0, 0, 0]},
        ],
        'connections': [
            {'id': 'synthetic-feed-to-mill', 'from': equipment_id + '-feed', 'to': equipment_id,
             'kind': 'material', 'binding_id': 'mv-FreshFeed-op'},
            {'id': 'synthetic-mill-to-separator', 'from': equipment_id, 'to': equipment_id + '-separator',
             'kind': 'material'},
            {'id': 'synthetic-separator-return', 'from': equipment_id + '-separator', 'to': equipment_id,
             'kind': 'material'},
            {'id': 'synthetic-fan-to-mill', 'from': equipment_id + '-fan', 'to': equipment_id, 'kind': 'air'},
        ],
        'bindings': [],
    }
    channels = {'cv': [('actual', 'pv'), ('target', 'target')],
                'mv': [('actual', 'applied_sp'), ('command', 'sp'), ('op', 'op'), ('mode', 'mode')],
                'dv': [('actual', 'pv')]}
    for role, expected in declared.items():
        for index, (name, unit, component) in enumerate(expected):
            variable = model['variables'][role + 's'][index]
            for channel, node_key in channels[role]:
                point = {'id': f'{role}-{name}-{channel}', 'equipment_id': component,
                         'role': role, 'variable_name': name, 'unit': 'mode' if channel == 'mode' else unit,
                         'channel': channel, 'node_id': variable['node_ids'][node_key], 'animation': 'none'}
                if role == 'mv' and channel == 'op':
                    # replay_plant explicitly owns ideal OP in the same units as
                    # SP. Baseline is only a visual reference, not a live value.
                    point.update(animation='flow' if name == 'FreshFeed' else 'rotation',
                                 reference_value=plant['baseline_mv'][index])
                view['bindings'].append(point)
    return validate_view(view, model)


def prepare_runtime(root, source, config, env):
    sys.path.insert(0, str(source / 'validation'))
    from campaign import controller_template
    campaign = json.loads((root / 'data/campaign.json').read_text())
    model = controller_template(campaign)
    model['metadata'].update(name=ENGINE, description='Disposable synthetic cement ideal-actuator runtime; original physical seconds; not factory commissioning')
    model['tuning']['sample_time'] = 60.0
    for role, allowed in (('cvs', ('pv', 'target', 'prediction', 'limits')),
                          ('mvs', ('pv', 'sp', 'op', 'mode', 'mode_target', 'future_plan', 'limits')),
                          ('dvs', ('pv', 'limits'))):
        for variable in model['variables'][role]:
            variable['node_ids'] = {k: v for k, v in variable['node_ids'].items() if k in allowed}
            if role == 'mvs':
                variable['node_ids']['applied_sp'] = variable['name'] + ':AppliedSP'
            if role == 'dvs':
                variable['node_ids']['limits'] = {key: variable['node_ids']['limits'][key] for key in ('low', 'high')}
    model['physics'] = {'step_coefficients': [[curve(path) for path in row[:2]] for row in campaign['plant']['paths']],
                        'dv_coefficients': [[curve(path) for path in row[2:]] for row in campaign['plant']['paths']]}
    runtime = root / 'runtime'
    models = runtime / 'models' / ENGINE
    models.mkdir(parents=True)
    save(models / 'model.json', model)
    binding = {'schema_version': 1, 'controller_name': ENGINE, 'sample_time': 60.0,
               'prediction_horizon': 48, 'control_horizon': 4}
    for role in ('cvs', 'mvs', 'dvs'):
        binding[role] = [{k: copy.deepcopy(v[k]) for k in ('name', 'description', 'units', 'node_ids')} for v in model['variables'][role]]
    nodes = []
    def add(name, value=0.0, dtype='Double'):
        nodes.append({'nodeId': name, 'nodeClass': 'Variable', 'dataType': dtype, 'initialValue': value})
    for suffix, value, dtype in [('OperatingMode', 1, 'Int32'), ('Heartbeat', 0, 'Int32'),
        ('NextRun', 0, 'Int32'), ('ExecutionTimeMs', 0.0, 'Double'), ('SolverStatus', 0, 'Int32'),
        ('ObjectiveFunction', 0.0, 'Double'), ('AcquisitionSequence', 0.0, 'Double'),
        ('ControlObservation', '', 'String'), ('ForecastEnvelope', 'null', 'String'),
        ('ConfigurationStatus', '', 'String'), ('SaveConfiguration', False, 'Boolean'),
        ('DcsRelease', 1.0, 'Double'), ('DcsAcknowledgement', 0.0, 'Double'),
        ('AuthorityWatchdog', 0.0, 'Double')]:
        add(ENGINE + ':' + suffix, value, dtype)
    variables = []
    for role in ('cv', 'mv', 'dv'):
        for i, variable in enumerate(model['variables'][role + 's']):
            ids, limits, name = variable['node_ids'], variable['limits'], variable['name']
            baseline = campaign['plant']['baseline_' + role][i]
            add(ids['pv'], baseline)
            for key, limit in (('low', 'low'), ('high', 'high'), ('ll', 'low_low'), ('hh', 'high_high')):
                if key in ids['limits']:
                    add(ids['limits'][key], limits[limit])
            if role == 'cv':
                add(ids['target'], limits['target'])
                add(ids['prediction'], [], 'DoubleArray')
                add(name + ':Weight', variable['weight'])
                add(name + ':Alpha', variable['alpha'])
                add(name + ':Bias')
                add(name + ':SteadyState', baseline)
            elif role == 'mv':
                for key in ('sp', 'op', 'applied_sp'):
                    add(ids[key], baseline)
                for key in ('mode', 'mode_target'):
                    add(ids[key], 0, 'Int32')
                add(ids['future_plan'], [], 'DoubleArray')
                add(name + ':Weight', variable['weight_r'])
                add(name + ':LastMove')
                add(name + ':SteadyState', baseline)
            variables.append({'id': name, 'equipment_instance_id': 'cement-mill-acceptance', 'role': role.upper(), 'unit': variable['units'],
                'pv_tag': ids['pv'], 'command_tag': ids.get('sp'), 'applied_tag': ids.get('applied_sp'), 'op_tag': ids.get('op'),
                'mode_tag': ids.get('mode'), 'mode_target_tag': ids.get('mode_target'), 'target_tag': ids.get('target'),
                'low_tag': ids['limits']['low'], 'high_tag': ids['limits']['high'], 'hard_low': limits['low'], 'hard_high': limits['high'],
                'max_move': variable.get('max_move'), 'command_coordinate': 'pid_setpoint' if role == 'mv' else None,
                'feedback_coordinate': 'pid_setpoint' if role == 'mv' else None, 'remote_mode': 3.0 if role == 'mv' else None})
    sampled = {}
    for variable in variables:
        if variable['role'] == 'DV':
            continue
        for field in ('target_tag', 'low_tag', 'high_tag'):
            original = variable[field]
            if original:
                observed = original + ':Acquired'
                sampled[original] = observed
                add(observed)
    regimes = [ENGINE + ':' + name for name in ('ProductRegime', 'LoadRegime', 'MaterialRegime', 'ControlModeRegime')]
    for index, tag in enumerate(regimes):
        add(tag, 0.0 if index == 3 else 1.0)
    save(runtime / 'regimes.json', {'constant_regimes': {tag: 1.0 for tag in regimes[:3]},
         'control_mode_tag': regimes[3], 'sampled_settings': sampled})
    node_map = {'model_binding': binding, 'nodes': nodes}
    server = root / 'opc-server'
    (server / 'models').mkdir(parents=True)
    (server / 'config').mkdir()
    # JSON is a strict YAML subset; avoids a new YAML dependency.
    save(server / 'models' / (ENGINE + '.yaml'), node_map)
    save(runtime / 'config/model.json', node_map)
    save(root / 'research/config/model.json', node_map)
    endpoint = f'opc.tcp://127.0.0.1:{config["opc_port"]}/'
    profile = {'profile_hash': '', 'target': {'plant': 'disposable-local', 'line': 'cement-test', 'area': 'cement_grinding', 'group': ENGINE},
        'equipment_instance_ids': ['cement-mill-acceptance'], 'engine_id': ENGINE, 'model_sha256': sha(models / 'model.json'),
        'endpoint': endpoint, 'model_file': 'model.json', 'model_binding': binding,
        'equipment_view': synthetic_equipment_view(model, campaign['plant']),
        'forecast_tag': ENGINE + ':ForecastEnvelope', 'observation_tag': ENGINE + ':ControlObservation',
        'acquisition_sequence_node': ENGINE + ':AcquisitionSequence', 'history_capacity_samples': 8192,
        'variables': variables,
        'grants': [{'user_id': user_id, 'permissions': ['runtime.admin', 'runtime.approve', 'runtime.deploy', 'runtime.operate',
                    'runtime.publish', 'runtime.publish.grant', 'runtime.view'] + (['runtime.maintenance'] if user_id == 1 else [])} for user_id in (1, 2)],
        'approved_by': 'explicit-disposable-fixture-author',
        'commissioning_kind': 'synthetic_local',
        'commissioning_evidence': 'Synthetic ideal-actuator fixture only. Fresh application and DCS release must be observed; this file is not factory commissioning.',
        'operating_envelope': 'Synthetic cement engineering bounds only; original physical seconds; no physical authority',
        'maximum_age_ms': 5000, 'maximum_skew_ms': 1000,
        'application_tolerance': 0.001,
        'handoff': {'release_tag': ENGINE + ':DcsRelease', 'release_value': 1.0, 'acquire_value': 0.0,
                    'acknowledgement_tag': ENGINE + ':DcsAcknowledgement', 'acknowledgement_value': 1.0,
                    'timeout_ms': 120000, 'watchdog_tag': ENGINE + ':AuthorityWatchdog',
                    'watchdog_timeout_ms': 120000, 'local_mode': 0.0,
                    'watchdog_evidence': 'Disposable source-owned ideal actuator holds accepted SP on watchdog expiry and independently acknowledges release; must be exercised, not factory commissioning'},
        'performance': {'regime_tags': regimes, 'sampled_tags': sampled, 'sample_interval_ms': 1000, 'minimum_samples': 30}}
    save(runtime / 'profiles-unhashed.json', [profile])
    command(root, [source / 'hmi/target/release/authrity-hmi', '--hash-runtime-profiles', runtime / 'profiles-unhashed.json', runtime / 'profiles.json'], env=env, label='canonical-runtime-profile')
    save(runtime / 'profile.json', json.loads((runtime / 'profiles.json').read_text())[0])
    # Separate finite 36-hour live fixture: every physical second is computed.
    # It is NEVER substituted for the three-month 129600 native controller scans.
    import numpy as np
    original = json.loads((root / 'data/scenario.json').read_text())
    times = np.arange(129600, dtype=float)
    scenario = {'sample_time': 1.0, 'time_seconds': times.tolist(),
                'provenance': {'source_scenario_sha256': sha(root / 'data/scenario.json'),
                               'scope': '36_hour_live_fixture_original_physical_seconds',
                               'resampling': 'linear interpolation of frozen exogenous schedule only; CV is baseline initialization'}}
    for key in ('cv', 'mv', 'dv', 'targets', 'measurement_noise', 'process_noise', 'gain_scale'):
        values = np.asarray(original[key])
        scenario[key] = (np.interp(times, original['time_seconds'], values).tolist() if values.ndim == 1 else
                         np.column_stack([np.interp(times, original['time_seconds'], values[:, i]) for i in range(values.shape[1])]).tolist())
    physical_truth = copy.deepcopy(campaign['plant'])
    physical_truth['sample_time'] = 1.0
    save(runtime / 'physical-truth.json', physical_truth)
    save(runtime / 'physical-scenario.json', scenario)
    command(root, [config['binary'], 'replay', '--plant', runtime / 'physical-truth.json',
                  '--scenario', runtime / 'physical-scenario.json', '--output', runtime / 'plant-history.json'],
            env=env, label='genuine-one-second-physical-history')
    plant_model = copy.deepcopy(model)
    plant_model['tuning'].update(sample_time=1.0, prediction_horizon=2880)
    plant_model['physics'] = {'step_coefficients': [[curve(path, 2880, 1.0) for path in row[:2]] for row in campaign['plant']['paths']],
                              'dv_coefficients': [[curve(path, 2880, 1.0) for path in row[2:]] for row in campaign['plant']['paths']]}
    save(runtime / 'plant-model.json', plant_model)
    certs = root / 'certificates'
    server_der, server_key = certificate(root, certs, 'cement-server', 'urn:authrity:cement-test-server', env)
    client_der, client_key = certificate(root, certs, 'cement-replay', 'urn:authrity:isolated-replay-plant', env)
    # async-opcua 0.17 uses a flat trusted directory and its exact CN/SHA-1 filename.
    server_trust_name = f'CN=cement-server [{hashlib.sha1(server_der.read_bytes(), usedforsecurity=False).hexdigest()}].der'
    for base in (runtime / 'pki', runtime / 'engine-pki', runtime / 'replay-pki'):
        (base / 'trusted').mkdir(parents=True, exist_ok=True)
        shutil.copy2(server_der, base / 'trusted' / server_trust_name)
    credentials = json.loads((root / 'test-credentials.json').read_text())
    server_settings = {'application_name': 'Disposable Cement OPC UA', 'application_uri': 'urn:authrity:cement-test-server',
        'product_uri': 'urn:authrity:cement-test', 'create_sample_keypair': False,
        'certificate_path': str(server_der), 'private_key_path': str(server_key),
        'certificate_validation': {'trust_client_certs': True, 'check_time': False}, 'pki_dir': str(server / 'pki'),
        'tcp_config': {'hello_timeout': 5, 'host': '127.0.0.1', 'port': config['opc_port']},
        'limits': {'clients_can_modify_address_space': False, 'max_subscriptions': 100, 'max_monitored_items_per_sub': 1000,
                   'max_monitored_item_queue_size': 10, 'max_array_length': 10000, 'max_string_length': 1048576,
                   'max_byte_string_length': 1048576, 'min_sampling_interval': 0.1, 'min_publishing_interval': 0.1},
        'locale_ids': ['en'], 'user_tokens': {'cement': {'user': 'cement-test', 'pass': credentials['password']},
                                            'replay': {'user': 'replay', 'x509': str(client_der)}},
        'discovery_urls': [endpoint], 'default_endpoint': 'secure',
        'endpoints': {'secure': {'path': '/', 'security_policy': 'Basic256Sha256', 'security_mode': 'SignAndEncrypt',
                                 'security_level': 14, 'user_token_ids': ['cement', 'replay']}}}
    save(server / 'config/serversettings.env', server_settings)
    (server / 'config/config.toml').write_text(f'[api]\nbind_address = "127.0.0.1"\nport = {config["opc_port"] + 1}\n')
    save(runtime / 'fixture.json', {'engine_id': ENGINE, 'endpoint': endpoint, 'model': str(models / 'model.json'),
         'client_certificate': str(client_der), 'client_key': str(client_key), 'namespace_index': 2, 'namespace_uri': NAMESPACE,
         'scope': 'synthetic_ideal_actuator_controller60s_physics1s_original_seconds_not_quarter_evidence',
         'handoff_acceptance': 'requires_actual_independent_simulator_acknowledgement_not_claimed'})


def spawn_runtime(root, source, config, env, spawn):
    fixture = json.loads((root / 'runtime/fixture.json').read_text())
    server = spawn('opc-server', [source / 'opcua_server/target/release/authrity-opcua-server'], root / 'opc-server', env)
    deadline = time.monotonic() + 60
    import socket
    while True:
        require(server.poll() is None, 'Owned OPC server exited; inspect log')
        try:
            with socket.create_connection(('127.0.0.1', config['opc_port']), timeout=1):
                break
        except OSError:
            require(time.monotonic() < deadline, 'Owned OPC listener did not become ready')
            time.sleep(0.5)
    spawn('cement-causal-plant', [source / 'apc_engine/target/release/replay_plant', '--plant', root / 'runtime/plant-model.json',
          '--history', root / 'runtime/plant-history.json', '--opc', fixture['endpoint'], '--namespace-index', '2',
          '--namespace-uri', NAMESPACE, '--pki', root / 'runtime/replay-pki', '--certificate', fixture['client_certificate'],
          '--private-key', fixture['client_key'], '--acknowledge-isolated-simulator', '--acknowledge-ideal-actuator',
          '--commissioning-profile', root / 'runtime/profile.json', '--acquisition-outage-secs', '12',
          '--regime-config', root / 'runtime/regimes.json'], root / 'runtime', env)
    return fixture


def start_native(root, config, *, recover_monitor):
    fixture = json.loads((root / 'runtime/fixture.json').read_text())
    profile = json.loads((root / 'runtime/profiles.json').read_text())[0]
    password = json.loads((root / 'test-credentials.json').read_text())['password']
    operation_id = 'cement-start-' + uuid.uuid4().hex
    payload = {'commissioning_profile': profile, 'model_filename': 'model.json', 'opc_url': fixture['endpoint'],
               'security_policy': 'Basic256Sha256', 'message_mode': 'SignAndEncrypt', 'auth_mode': 'username',
               'username': 'cement-test', 'password': password, 'acquisition_sequence_node': ENGINE + ':AcquisitionSequence',
               'max_acquisition_age_secs': 3.0, 'max_read_to_command_secs': 1.0, 'command_confirmation_secs': 3.0,
               'command_confirmation_tolerance': 0.001, 'monitor_window_samples': 60, 'live_tuning': False}
    payload.update(operation_id=operation_id, expires_at=(datetime.now(timezone.utc) + timedelta(seconds=295)).isoformat(),
                   recover_monitor=recover_monitor)
    save(root / (operation_id + '-intent.json'), {key: value for key, value in payload.items() if key not in ('username', 'password')})
    url = f'http://127.0.0.1:{config["supervisor_port"]}/api/controllers/{ENGINE}/start'
    request = urllib.request.Request(url, data=json.dumps(payload).encode(), headers={'Content-Type': 'application/json', 'x-api-key': password})
    try:
        with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=30) as response:
            return {'operation_id': operation_id, 'status': response.status, 'body': response.read().decode()}
    except urllib.error.HTTPError as error:
        body = error.read().decode(errors='replace').replace(password, '[REDACTED]')
        save(root / (operation_id + '-rejection.json'), {'operation_id': operation_id, 'status': error.code, 'body': body})
        raise RuntimeError(f'Native start rejected ({error.code}): {body}') from error
