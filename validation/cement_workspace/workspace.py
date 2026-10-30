"""User-run isolated cement acceptance workspace (Python 3.11+).

prepare creates a NEW root; it never builds in or updates locks in the checkout.
No installers or source downloads. --allow-dependency-downloads explicitly permits
Cargo/npm/pip dependency resolution. Without it all dependency operations are offline.
start owns child processes in the foreground; Ctrl-C terminates only its children.
Full research acceptance requires the real MinGW/Wine COM fixture. --transport direct
prepares native data/build evidence only, never claims DA or full research passed.
"""
from __future__ import annotations
import argparse
import csv
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import secrets
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.request

REPO = Path(__file__).resolve().parents[2]
CRATES = ('apc_engine', 'controller_host', 'opcua_server', 'hmi')
SAMPLES = 129600


def save(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('x', encoding='utf-8') as stream:
        json.dump(value, stream, indent=2, allow_nan=False)
        stream.write('\n')


def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def command(root, argv, *, cwd=None, env=None, label=None, expected_codes=(0,)):
    directory = root / 'commands'
    directory.mkdir(exist_ok=True)
    number = len(list(directory.glob('*.json')))
    stem = f'{number:04d}-{label or Path(str(argv[0])).name}'
    argv = list(map(str, argv))
    print('+ ' + ' '.join(argv), flush=True)
    started = time.time()
    with (directory / (stem + '.log')).open('xb') as log:
        result = subprocess.run(argv, cwd=cwd, env=env, stdout=log, stderr=subprocess.STDOUT)
    save(directory / (stem + '.json'), {'argv': argv, 'cwd': str(cwd or Path.cwd()),
         'started_unix': started, 'finished_unix': time.time(), 'returncode': result.returncode,
         'log': stem + '.log', 'expected_codes': list(expected_codes)})
    require(result.returncode in expected_codes, f'Command failed ({result.returncode}); evidence: {directory / (stem + ".log")}')
    return directory / (stem + '.log')


def tool(name):
    found = shutil.which(name)
    require(found is not None, f'Missing prerequisite on PATH: {name}; no installer was run')
    # Preserve argv[0] for rustup's cargo/rustc symlink proxies.
    return os.path.abspath(found)


def private_env(root):
    env = os.environ.copy()
    env.update(WINEPREFIX=str(root / 'wine'), WINEARCH='win64', WINEDEBUG='-all',
               PYTHONDONTWRITEBYTECODE='1', PYTHONNOUSERSITE='1',
               NO_PROXY='127.0.0.1,localhost,::1', no_proxy='127.0.0.1,localhost,::1')
    # No inherited production connection or research runtime is allowed.
    for name in ('AUTHRITY_HMI_CONFIG', 'AUTHRITY_RESEARCH_RUNTIME', 'DATABASE_URL', 'HOST_API_KEY'):
        env.pop(name, None)
    return env


def generate(root, source, binary, python, seed=None):
    # Import the copied current implementation, not an old installed package.
    sys.path.insert(0, str(source / 'validation'))
    from campaign import generate_scenario, calendar_splits
    config = json.loads((source / 'validation/examples/cement_campaign.json').read_text())
    if seed is not None:
        config['seed'] = seed
    scenario = generate_scenario(config)
    require(len(scenario['time_seconds']) == SAMPLES, 'Fixture must contain the full 2026 Q1: 129600 native scans')
    data = root / 'data'
    save(data / 'campaign.json', config)
    save(data / 'plant.json', config['plant'])
    save(data / 'scenario.json', scenario)
    command(root, [binary, 'replay', '--plant', data / 'plant.json', '--scenario', data / 'scenario.json',
                   '--output', data / 'native-history.json'], label='quarter-native-history')
    history = json.loads((data / 'native-history.json').read_text())
    require(len(history['time_seconds']) == SAMPLES, 'Native history omitted quarter samples')
    require(history['sample_time'] == 60.0 and history['time_seconds'] == scenario['time_seconds'],
            'Native generation changed quarter clock/sample spacing')
    signals = []
    for role in ('cv', 'mv', 'dv'):
        for variable in config['variables'][role]:
            signals.append({'column': variable['name'], 'name': variable['name'], 'role': role,
                            'unit': variable['unit'], 'scale': 1.0, 'offset': 0.0,
                            'quality_column': 'quality', 'min': variable['low'], 'max': variable['high']})
    with (data / 'cement-2026-q1-raw.csv').open('x', newline='') as stream:
        writer = csv.writer(stream)
        writer.writerow(['time_seconds', 'quality'] + [s['column'] for s in signals])
        rows = []
        for i, seconds in enumerate(history['time_seconds']):
            row = [seconds, 192, *history['cv'][i], *history['command_mv'][i], *history['dv'][i]]
            if i == 120:
                row[1] = 0
            if i == 180:
                row[2] = ''
            rows.append(row)
        rows[250], rows[251] = rows[251], rows[250]
        rows.insert(301, list(rows[300]))
        writer.writerows(rows)
    splits = calendar_splits(config)
    save(data / 'cleaning.json', {'time_column': 'time_seconds', 'time_format': 'seconds',
         'sample_time': config['sample_time'], 'train_end': splits[0]['last'] * config['sample_time'],
         'validation_end': splits[1]['last'] * config['sample_time'], 'duplicate_policy': 'last',
         'missing_policy': 'forward_fill', 'max_gap_seconds': 2 * config['sample_time'],
         'outlier_policy': 'none', 'sigma': 4.0, 'signals': signals})
    save(data / 'provenance.json', {'scope': 'synthetic_software_acceptance_not_factory_validation',
         'source': 'current_native_replay_independent_declared_cement_plant', 'native_samples': SAMPLES,
         'raw_rows': len(rows), 'start': config['start'], 'months': 3, 'sample_time': 60,
         'excitation': 'seeded_exogenous_PRBS_ramps_and_regimes_from_frozen_campaign',
         'mv_coordinate': 'command_mv_pid_setpoint_not_actuator_output',
         'defects': {'bad_quality_sample': 120, 'missing_cv_sample': 180,
                     'swapped_samples': [250, 251], 'duplicate_sample': 300},
         'binary_sha256': sha(binary), 'files': {p.name: sha(p) for p in data.iterdir() if p.is_file()}})


def settings(root, source, kind, port, python, binary, wine, bridge, password, supervisor_port, opc_port, questdb_http_port, questdb_ilp_port):
    # JSON strings are valid TOML basic strings for these local scalar paths.
    q = lambda value: json.dumps(str(value))
    base = root / kind
    base.mkdir()
    (base / 'config').mkdir()
    (base / 'pki').mkdir()
    save(base / 'config/hosts.json', {
        'supervisors': [{'id': 'cement-test-supervisor', 'name': 'Disposable cement supervisor',
                         'url': f'http://127.0.0.1:{supervisor_port}'}],
        'opc_servers': [{'id': 'cement-test-opc', 'name': 'Disposable cement OPC UA',
                         'url': f'http://127.0.0.1:{opc_port + 1}',
                         'opc_endpoint': f'opc.tcp://127.0.0.1:{opc_port}/'}]})
    text = f'''[web]
bind_address = "127.0.0.1:{port}"
assets_directory = {q(source / 'hmi/frontend/dist')}
local_test_mode = true
[opcua]
endpoint_url = "opc.tcp://127.0.0.1:{opc_port}/"
security_policy = "Basic256Sha256"
message_mode = "SignAndEncrypt"
namespace_index = 2
[identity]
app_name = "Disposable cement acceptance {kind}"
app_uri = "urn:authrity:local-test:{kind}:{root.name}"
auto_create_keys = true
trust_server_certs = true
[auth]
mode = "username"
username = "cement-test"
password = {q(password)}
[runtime]
model_path = {q(base / 'config/model.json')}
connect_opcua = {'true' if kind == 'runtime' else 'false'}
profiles_file = {q(root / 'runtime/profiles.json')}
reconnect_delay_sec = 2
[services]
supervisor_url = "http://127.0.0.1:{supervisor_port}"
supervisor_api_key = {q(password)}
opc_hot_reload_url = "http://127.0.0.1:{opc_port + 1}"
[paths]
pki_dir = {q(base / 'pki')}
cert_path = {q(base / 'pki/own/cert.der')}
key_path = {q(base / 'pki/own/private.pem')}
[historian]
enabled = {'true' if kind == 'runtime' else 'false'}
host = "127.0.0.1"
ilp_port = {questdb_ilp_port}
http_port = {questdb_http_port}
rest_port = {questdb_http_port}
table_name = "disposable_cement"
batch_size = 100
flush_interval_ms = 1000
snapshot_interval_sec = 60
[hmi_auth]
database_url = {q('sqlite:' + str(root / 'identities.db'))}
session_inactivity_minutes = 1440
test_bootstrap_password = {q(password)}
[reports]
directory = {q(base / 'reports')}
[research]
enabled = {'true' if kind == 'research' else 'false'}
directory = {q(root / 'research-store')}
python = {q(python)}
script = {q(source / 'validation/research.py')}
binary = {q(binary)}
bridge = {q(wine or '')}
bridge_args = {json.dumps([str(bridge)] if bridge else [])}
command_timeout_seconds = 600
native_timeout_seconds = 3600
'''
    (base / 'settings.toml').write_text(text)


def prepare(args):
    root = args.root
    require(root != REPO and REPO not in root.parents, 'Choose a disposable root outside the repository')
    root.mkdir(parents=True, exist_ok=False, mode=0o700)
    save(root / 'owner.json', {'schema_version': 1, 'kind': 'authrity-disposable-cement-workspace',
                             'created_unix': time.time(), 'repository': str(REPO), 'argv': sys.argv})
    report = {'status': 'preparing', 'transport_requested': args.transport, 'samples_required': SAMPLES}
    try:
        require(sys.version_info >= (3, 11), 'Python 3.11+ is required')
        require(len(str(root / 'runtime/authority').encode()) + 44 < 104,
                'Use a shorter new root (for example /tmp/cement-001): native Unix authority sockets must fit 104 bytes')
        tools = {name: tool(name) for name in ('cargo', 'rustc', 'npm', 'node', 'openssl')}
        if args.transport == 'opc-da':
            tools.update({name: tool(name) for name in ('make', args.cxx, args.wine, 'wineboot', 'winepath', 'wineserver')})
        if args.postgres:
            tools.update({name: tool(name) for name in ('initdb', 'pg_ctl', 'psql')})
        save(root / 'tools.json', tools)
        source = root / 'source'
        source.mkdir()
        for name in (*CRATES, 'validation', 'opc_da_simulator'):
            shutil.copytree(REPO / name, source / name,
                ignore=shutil.ignore_patterns('target', 'node_modules', 'dist', '__pycache__', '.git', 'runs', 'verified-runs', 'build'))
        snapshots = {str(p.relative_to(source)): sha(p) for p in source.rglob('*') if p.is_file()}
        save(root / 'source-before.json', snapshots)
        env = private_env(root)
        from historian_fixture import prepare_historian
        prepare_historian(root, args, env)
        for name in ('rustc', 'cargo', 'node', 'npm'):
            command(root, [tools[name], '--version'], env=env, label=name + '-version')
        python = Path(sys.executable)
        if args.allow_dependency_downloads:
            command(root, [python, '-m', 'venv', root / 'venv'], label='python-venv')
            python = root / 'venv/bin/python'
            command(root, [python, '-m', 'pip', 'install', '-r', source / 'validation/requirements.txt'], label='python-dependencies')
        else:
            require(importlib.util.find_spec('numpy') is not None, 'Offline prerequisite missing: numpy; use an existing Python environment or explicitly opt in to dependency downloads')
        frontend = source / 'hmi/frontend'
        npm = [tools['npm'], 'install', '--no-audit', '--no-fund']
        if not args.allow_dependency_downloads:
            npm.append('--offline')
        # npm install deliberately reconciles stale lockfiles in this disposable copy;
        # it does not hide stale-lock failures behind --locked/npm ci.
        command(root, npm, cwd=frontend, env=env, label='frontend-dependencies')
        command(root, [tools['npm'], 'run', 'build'], cwd=frontend, env=env, label='react-build')
        command(root, [tools['npm'], 'run', 'lint'], cwd=frontend, env=env, label='react-lint')
        frontend_tests = sorted((frontend / 'tests').glob('*.test.ts'))
        require(frontend_tests, 'Frontend behavioral regression suite is missing')
        test_output = frontend / '.test-output'
        command(root, [tools['node'], frontend / 'node_modules/typescript/bin/tsc',
                       '--target', 'ES2022', '--module', 'commonjs', '--moduleResolution', 'node',
                       '--lib', 'ES2022,DOM,DOM.Iterable', '--types', 'node', '--jsx', 'react-jsx',
                       '--esModuleInterop', '--strict', '--skipLibCheck',
                       '--rewriteRelativeImportExtensions', '--rootDir', frontend,
                       '--outDir', test_output, *frontend_tests],
                cwd=frontend, env=env, label='frontend-test-compile')
        save(test_output / 'package.json', {'type': 'commonjs'})
        compiled_tests = sorted((test_output / 'tests').glob('*.test.js'))
        require(len(compiled_tests) == len(frontend_tests), 'Frontend test compilation omitted a suite')
        command(root, [tools['node'], '--test', *compiled_tests],
                cwd=frontend, env=env, label='frontend-tests')
        for crate in CRATES:
            extra = [] if args.allow_dependency_downloads else ['--offline']
            command(root, [tools['cargo'], 'build', '--release', '--bins', *extra], cwd=source / crate, env=env, label=crate + '-build')
            command(root, [tools['cargo'], 'test', '--all-targets', *extra], cwd=source / crate, env=env, label=crate + '-tests')
        command(root, [python, '-m', 'unittest', 'discover', '-s', str(source / 'validation'), '-p', 'test_*.py'], cwd=source, env=dict(env, MPC_VALIDATE_BIN=str(source / 'apc_engine/target/release/mpc_validate')), label='python-tests')
        command(root, [python, '-c', 'import json,platform,importlib.metadata; print(json.dumps({"python":platform.python_version(),"numpy":importlib.metadata.version("numpy")}))'],
                env=env, label='python-versions')
        wine = tools.get(args.wine)
        bridge = source / 'opc_da_simulator/build/opc_da_bridge.exe' if wine else None
        if wine:
            command(root, [tools['make'], '-C', source / 'opc_da_simulator', 'CXX=' + tools[args.cxx]], env=env, label='real-da-build')
            command(root, [tools['wineboot'], '--init'], env=env, label='private-wine-prefix')
            dll = source / 'opc_da_simulator/build/authrity_opc_da.dll'
            dll_windows = command(root, [tools['winepath'], '-w', dll], env=env, label='private-dll-path').read_text().strip()
            require(dll_windows and '\n' not in dll_windows, 'Wine DLL path conversion was not unambiguous; inspect its captured log')
            command(root, [wine, 'regsvr32', '/s', dll_windows], env=env, label='private-com-registration')
            for key in ('CLSID\\{BCE86C29-5DCB-43D8-A6A0-4699B503A0B8}', 'AuTHRity.LocalDaSimulator.1'):
                command(root, [wine, 'reg', 'copy', 'HKCU\\Software\\Classes\\' + key,
                               'HKLM\\Software\\Classes\\' + key, '/s', '/f'], env=env, label='private-wine-hive-compatibility')
            command(root, [wine, source / 'opc_da_simulator/build/opc_da_tests.exe'], env=env, label='real-com-tests')
            command(root, [tools['wineserver'], '-k'], env=env, label='private-wine-preparation-stop', expected_codes=(0, 1))
            command(root, [tools['wineserver'], '-w'], env=env, label='private-wine-preparation-stopped')
        binary = source / 'apc_engine/target/release/mpc_campaign'
        # Generation uses numpy from the selected Python, including the explicit venv.
        command(root, [python, source / 'validation/cement_workspace/workspace.py', 'generate', '--root', root], env=env, label='generate-quarter')
        # Explicit test password; the generated local_test_mode settings reject field profiles.
        password = 'admin'
        save(root / 'test-credentials.json', {'username': 'admin', 'password': password, 'scope': 'disposable_loopback_only'})
        os.chmod(root / 'test-credentials.json', 0o600)
        save(root / 'reviewer-credentials.json', {'username': 'reviewer', 'password': secrets.token_urlsafe(24),
             'initial_password': secrets.token_urlsafe(24), 'scope': 'independent_disposable_reviewer'})
        os.chmod(root / 'reviewer-credentials.json', 0o600)
        for kind, port in (('research', args.research_port), ('runtime', args.runtime_port)):
            settings(root, source, kind, port, python, binary, wine, bridge, password, args.supervisor_port, args.opc_port,
                     args.questdb_http_port, args.questdb_ilp_port)
        config = {'root': str(root), 'python': str(python), 'binary': str(binary), 'wine': wine,
                  'bridge': str(bridge) if bridge else None, 'transport': args.transport,
                  'research_port': args.research_port, 'runtime_port': args.runtime_port,
                  'supervisor_port': args.supervisor_port, 'opc_port': args.opc_port,
                  'postgres': args.postgres, 'postgres_port': args.postgres_port,
                  'questdb_http_port': args.questdb_http_port, 'questdb_ilp_port': args.questdb_ilp_port}
        save(root / 'workspace.json', config)
        command(root, [python, source / 'validation/cement_workspace/workspace.py', 'runtime-generate', '--root', root],
                env=env, label='generate-causal-runtime')
        if args.postgres:
            columns = ['observed_at', 'time_seconds', 'quality'] + [v['name'] for role in ('cv', 'mv', 'dv')
                       for v in json.loads((root / 'data/campaign.json').read_text())['variables'][role]]
            with (root / 'research/settings.toml').open('a') as stream:
                stream.write('\n[[research.postgres_sources]]\nid = "cement-quarter"\nname = "Disposable cement quarter"\n'
                    'description = "Native synthetic raw CSV imported into owned loopback PostgreSQL"\n'
                    'dsn_env = "AUTHRITY_CEMENT_TEST_PG"\nschema = "public"\ntable = "cement_raw"\n'
                    'timestamp_column = "observed_at"\ncolumns = ' + json.dumps(columns) +
                    '\nmax_rows = 129601\nstatement_timeout_seconds = 120\n')
        if args.postgres:
            command(root, [tools['initdb'], '-D', root / 'postgres', '-U', 'cement_test', '--auth-local=trust', '--auth-host=trust'], env=env, label='private-postgres-init')
        locks = {str(p.relative_to(source)): sha(p) for p in source.rglob('*') if p.name in ('Cargo.lock', 'package-lock.json')}
        save(root / 'resolved-locks.json', locks)
        save(root / 'built-artifacts.json', {str(p.relative_to(source)): sha(p) for crate in CRATES
             for p in (source / crate / 'target/release').iterdir() if p.is_file() and os.access(p, os.X_OK)})
        report.update(status='prepared', transport_status='real_com_tests_executed' if wine else 'not_requested_unavailable_for_research',
                      runtime_acceptance='not_started', ui_acceptance='not_exercised', full_quarter_acceptance='not_exercised')
        next_command = shlex.join([str(python), '-B', str(source / 'validation/cement_workspace/workspace.py'), 'start', '--root', str(root)])
        print(f'PREPARED {root}\nRAW CSV {root / "data/cement-2026-q1-raw.csv"}\nNEXT {next_command}', flush=True)
    except Exception as error:
        report.update(status='failed', error=str(error))
        raise
    finally:
        save(root / 'preparation.json', report)


def available(port):
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', port))


def start(args):
    root = args.root
    import fcntl
    owner_lock = (root / 'launcher.lock').open('a+')
    fcntl.flock(owner_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    config = json.loads((root / 'workspace.json').read_text())
    require(json.loads((root / 'preparation.json').read_text())['status'] == 'prepared', 'Prepare did not complete')
    require(config['transport'] == 'opc-da', 'Full research requires actual COM DA; direct-only preparation is not a passing research stack')
    for key in ('research_port', 'runtime_port', 'supervisor_port', 'opc_port', 'questdb_http_port', 'questdb_ilp_port'):
        available(config[key])
    available(config['opc_port'] + 1)
    source = root / 'source'
    env = private_env(root)
    children = []
    logs = []
    receipt = root / ('launch-' + str(time.time_ns()) + '.json')
    def spawn(name, argv, cwd, child_env):
        log = (root / (name + '-' + str(time.time_ns()) + '.log')).open('xb')
        logs.append(log)
        child = subprocess.Popen(list(map(str, argv)), cwd=cwd, env=child_env, stdout=log,
                                 stderr=subprocess.STDOUT, start_new_session=True)
        children.append((name, child))
        return child
    def stop(signum, frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, stop)
    try:
        if config['postgres']:
            postgres(root, config, env)
        from historian_fixture import spawn_historian
        spawn_historian(root, env, spawn)
        from runtime_fixture import spawn_runtime, start_native
        spawn_runtime(root, source, config, env, spawn)
        for kind in ('research', 'runtime'):
            child_env = dict(env, AUTHRITY_HMI_CONFIG=str(root / kind / 'settings.toml'))
            if config['postgres']:
                child_env['AUTHRITY_CEMENT_TEST_PG'] = f'postgresql://cement_reader@127.0.0.1:{config["postgres_port"]}/cement_test'
            spawn(kind, [source / 'hmi/target/release/authrity-hmi'], root / kind, child_env)
        host_env = dict(env, HOST_API_KEY=json.loads((root / 'test-credentials.json').read_text())['password'])
        spawn('supervisor', [source / 'controller_host/target/release/controller_host', '--bind-address', '127.0.0.1',
              '--port', config['supervisor_port'], '--model-dir', root / 'runtime/models', '--engine-bin',
              source / 'apc_engine/target/release/authrity-apc-engine', '--pki-dir', root / 'runtime/engine-pki',
              '--opc-defaults-path', root / 'runtime/config/opc_client.env',
              '--runtime-dir', root / 'runtime/authority'], root / 'runtime', host_env)
        save(receipt, {'status': 'launched', 'processes': [{'name': n, 'pid': c.pid} for n, c in children],
                       'root': str(root), 'runtime_opc': 'real_causal_ideal_actuator_original_seconds_controller60_physics1'})
        deadline = time.monotonic() + 90
        for kind in ('research', 'runtime'):
            url = f'http://127.0.0.1:{config[kind + "_port"]}/'
            while True:
                require(all(c.poll() is None for _, c in children), 'Owned service exited; inspect launch logs')
                try:
                    with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(url, timeout=2) as response:
                        require(response.status == 200, 'UI root did not respond successfully')
                    break
                except (OSError, TimeoutError):
                    require(time.monotonic() < deadline, f'Startup timed out: {url}; inspect owned logs')
                    time.sleep(0.5)
            print(f'{kind.upper()} UI {url}', flush=True)
        from verify_api import Api
        credentials = json.loads((root / 'test-credentials.json').read_text())
        credentials = {key: credentials[key] for key in ('username', 'password')}
        for kind in ('research', 'runtime'):
            api = Api(f'http://127.0.0.1:{config[kind + "_port"]}', credentials)
            users = api.call('/api/admin/users')
            require(next(user for user in users if user['username'] == 'admin')['id'] == 1,
                    'Disposable profile grants require the freshly bootstrapped administrator ID1')
            reviewer = json.loads((root / 'reviewer-credentials.json').read_text())
            if not any(user['username'] == reviewer['username'] for user in users):
                api.call('/api/admin/users', {'username': reviewer['username'], 'password': reviewer['initial_password'], 'role': 'admin'})
                review_api = Api(api.url, {'username': reviewer['username'], 'password': reviewer['initial_password']})
                review_api.call('/api/auth/change-password', {'current_password': reviewer['initial_password'], 'new_password': reviewer['password']})
            users = api.call('/api/admin/users')
            require(next(user for user in users if user['username'] == reviewer['username'])['id'] == 2,
                    'Independent disposable reviewer must match explicit profile grant ID2')
        save(root / ('native-start-' + str(time.time_ns()) + '.json'), start_native(root, config, recover_monitor=args.recover_monitor))
        verify_command = shlex.join([config['python'], '-B', str(source / 'validation/cement_workspace/workspace.py'), 'verify', '--root', str(root), '--acknowledge-owned-faults'])
        print(f'TEST ACCOUNT {root / "test-credentials.json"}\nREVIEWER ACCOUNT {root / "reviewer-credentials.json"}\nRUNTIME: real command-driven ideal actuator, original physical seconds, controller Ts60 / acquisition Ts1. Genuine controller warmup is required; explicit operator Engage required.\nHISTORIAN http://127.0.0.1:{config["questdb_http_port"]}/\nKeep this foreground owner alive; Ctrl-C stops only owned processes.\nFULL API ACCEPTANCE (includes bounded owned acquisition-publication outage and engine interruption): {verify_command}\nUI VIDEO: record original React only after all required checks pass; Traditional Chinese subtitles and Mandarin narration are separate required parent-browser deliverables.', flush=True)
        while True:
            require(all(c.poll() is None for _, c in children), 'Owned service exited; inspect launch logs')
            time.sleep(1)
    finally:
        for _, child in reversed(children):
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGTERM)
        for _, child in reversed(children):
            try:
                child.wait(timeout=30)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()
        for log in logs:
            log.close()
        if config['postgres'] and (root / 'postgres/postmaster.pid').exists():
            command(root, [tool('pg_ctl'), '-D', root / 'postgres', '-m', 'fast', 'stop'], env=env, label='private-postgres-stop')
        if config['transport'] == 'opc-da':
            # Wine -k returns 1 when no server remains; -w independently confirms termination.
            command(root, [tool('wineserver'), '-k'], env=env, label='private-wine-stop', expected_codes=(0, 1))
            command(root, [tool('wineserver'), '-w'], env=env, label='private-wine-stopped')
        owner_lock.close()


def postgres(root, config, env):
    available(config['postgres_port'])
    command(root, [tool('pg_ctl'), '-D', root / 'postgres', '-l', root / 'postgres.log', '-o',
                  f'-h 127.0.0.1 -p {config["postgres_port"]} -k {root}', '-w', 'start'], env=env, label='private-postgres-start')
    marker = root / 'postgres-import.json'
    if marker.exists():
        return
    base = [tool('psql'), '-h', '127.0.0.1', '-p', config['postgres_port'], '-U', 'cement_test', '-v', 'ON_ERROR_STOP=1']
    command(root, [*base, '-d', 'postgres', '-c', 'CREATE DATABASE cement_test'], env=env, label='private-postgres-database')
    columns = ['ProductFineness', 'MillDifferentialPressure', 'FreshFeed', 'SeparatorSpeed', 'ClinkerHardness', 'FeedMoisture']
    sql = 'CREATE TABLE cement_raw (time_seconds double precision, quality integer,' + ','.join('"' + c + '" double precision' for c in columns) + ');'
    command(root, [*base, '-d', 'cement_test', '-c', sql], env=env, label='private-postgres-table')
    path = str(root / 'data/cement-2026-q1-raw.csv').replace("'", "''")
    command(root, [*base, '-d', 'cement_test', '-c', f"\\copy cement_raw FROM '{path}' WITH (FORMAT csv, HEADER true)"], env=env, label='private-postgres-import')
    command(root, [*base, '-d', 'cement_test', '-c', "ALTER TABLE cement_raw ADD COLUMN observed_at timestamptz; UPDATE cement_raw SET observed_at = TIMESTAMPTZ '2026-01-01T00:00:00Z' + time_seconds * INTERVAL '1 second';"], env=env, label='private-postgres-timestamps')
    command(root, [*base, '-d', 'cement_test', '-c',
        'CREATE ROLE cement_reader LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE; '
        'GRANT CONNECT ON DATABASE cement_test TO cement_reader; '
        'GRANT USAGE ON SCHEMA public TO cement_reader; GRANT SELECT ON cement_raw TO cement_reader; '
        'ALTER ROLE cement_reader SET default_transaction_read_only=on;'],
        env=env, label='private-postgres-readonly-import-role')
    save(marker, {'csv_sha256': sha(root / 'data/cement-2026-q1-raw.csv'), 'rows': SAMPLES + 1, 'source': 'disposable_loopback_postgresql'})


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('action', choices=('prepare', 'start', 'verify', 'generate', 'runtime-generate'))
    parser.add_argument('--root', required=True, type=lambda x: Path(x).expanduser().resolve())
    parser.add_argument('--allow-dependency-downloads', action='store_true', help='Explicitly permit Cargo/npm/pip dependency network downloads; never installs toolchains')
    parser.add_argument('--transport', choices=('opc-da', 'direct'), default='opc-da')
    parser.add_argument('--wine', default='wine')
    parser.add_argument('--cxx', default='x86_64-w64-mingw32-g++')
    parser.add_argument('--postgres', action='store_true', help='Create only a disposable loopback PostgreSQL cluster using installed tools')
    parser.add_argument('--questdb-java', help='Existing bundled QuestDB Java/module runtime executable; no installer')
    parser.add_argument('--questdb-jar', help='Existing local QuestDB jar (uses compatible Java on PATH unless --questdb-java supplied)')
    parser.add_argument('--until-selection', action='store_true', help='verify: leave server tuning job for real browser candidate selection')
    parser.add_argument('--resume-run', help='verify: verify a specific existing manually selected run')
    parser.add_argument('--deadline-hours', type=float, default=168, help='verify: wall-clock limit only, never a reduced sample count')
    parser.add_argument('--acknowledge-owned-faults', action='store_true', help='verify: permit bounded SIGUSR1/SIGUSR2 acquisition-publication outage on the verified opted-in simulator, plus owned loopback engine interruption; physical time never stops')
    parser.add_argument('--recover-monitor', action='store_true', help='start: explicitly recover the original baseline Monitor after independently checking prior operation/process state; native reachability and writer-lock guards remain required')
    for name, default in (('research', 43180), ('runtime', 43181), ('supervisor', 43182), ('opc', 43185),
                          ('postgres', 43189), ('questdb-http', 43190), ('questdb-ilp', 43191)):
        parser.add_argument('--' + name + '-port', type=int, default=default)
    args = parser.parse_args()
    ports = [getattr(args, name + '_port') for name in ('research', 'runtime', 'supervisor', 'opc', 'postgres', 'questdb_http', 'questdb_ilp')]
    require(all(1024 <= p <= 65534 for p in ports) and len(set(ports + [args.opc_port + 1])) == len(ports) + 1,
            'Use distinct unprivileged loopback ports')
    if args.action == 'prepare':
        prepare(args)
    elif args.action == 'start':
        start(args)
    elif args.action == 'generate':
        require((args.root / 'owner.json').is_file(), 'Generation requires an owned new preparation root')
        generate(args.root, args.root / 'source', args.root / 'source/apc_engine/target/release/mpc_campaign', Path(sys.executable))
    elif args.action == 'runtime-generate':
        from runtime_fixture import prepare_runtime
        config = json.loads((args.root / 'workspace.json').read_text())
        prepare_runtime(args.root, args.root / 'source', config, private_env(args.root))
    else:
        config = json.loads((args.root / 'workspace.json').read_text())
        options = ['--deadline-hours', str(args.deadline_hours)]
        if args.acknowledge_owned_faults:
            options.append('--acknowledge-owned-faults')
        if args.until_selection:
            options.append('--until-selection')
        if args.resume_run:
            options += ['--resume-run', args.resume_run]
        command(args.root, [config['python'], args.root / 'source/validation/cement_workspace/verify_api.py', '--root', args.root, *options],
                env=private_env(args.root), label='full-api-acceptance')


if __name__ == '__main__':
    try:
        main()
    except KeyboardInterrupt:
        print('Stopped owned workspace processes; artifacts preserved.', file=sys.stderr)
        sys.exit(130)
    except Exception as error:
        print(f'BLOCKED/FAILED: {error}', file=sys.stderr)
        sys.exit(2)
