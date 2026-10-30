"""Existing local QuestDB runtime only; no download or installer.

Foreground module entrypoint follows https://questdb.com/docs/deployment/systemd.md
Listener keys follow https://questdb.com/docs/configuration/overview/ .
A supplied local Java/runtime must be compatible with the supplied QuestDB build;
incompatibility is a failed startup with logs, never disabled historian acceptance.
"""
import json
from pathlib import Path
import time
import urllib.request

from workspace import command, require, save, sha, tool


def prepare_historian(root, args, env):
    require(args.questdb_java or args.questdb_jar,
            'Runtime R5 requires an existing local QuestDB runtime. Pass --questdb-java /existing/questdb/bin/java '
            '(bundled module runtime) or --questdb-jar /existing/questdb.jar with compatible Java on PATH. No installer/download was run.')
    java = tool(args.questdb_java or 'java')
    jar = Path(args.questdb_jar).expanduser().resolve() if args.questdb_jar else None
    if jar:
        require(jar.is_file(), 'Configured local QuestDB jar does not exist: ' + str(jar))
    base = root / 'questdb'
    for name in ('conf', 'db', 'tmp'):
        (base / name).mkdir(parents=True, exist_ok=False)
    (base / 'conf/server.conf').write_text(
        f'http.enabled=true\nhttp.bind.to=127.0.0.1:{args.questdb_http_port}\n'
        f'http.net.bind.to=127.0.0.1:{args.questdb_http_port}\nhttp.min.enabled=false\n'
        f'line.tcp.enabled=true\nline.tcp.net.bind.to=127.0.0.1:{args.questdb_ilp_port}\n'
        'line.udp.enabled=false\npg.enabled=false\ntelemetry.enabled=false\n'
        'line.tcp.commit.interval.default=1000\n')
    argv = [java, '-ea', '-Djava.io.tmpdir=' + str(base / 'tmp'),
            '--enable-native-access=io.questdb',
            '--add-opens=java.base/java.lang=io.questdb', '--add-opens=java.base/java.lang.reflect=io.questdb',
            '--add-opens=java.base/java.nio=io.questdb', '--add-opens=java.base/java.time.zone=io.questdb',
            '--add-exports=java.base/jdk.internal.misc=io.questdb', '--add-exports=java.base/jdk.internal.vm=io.questdb']
    if jar:
        argv += ['--module-path', str(jar)]
    argv += ['-m', 'io.questdb/io.questdb.ServerMain', '-d', str(base)]
    command(root, [java, '-version'], env=env, label='historian-java-version')
    save(base / 'launch.json', {'argv': argv, 'java': java, 'jar': str(jar) if jar else None,
         'jar_sha256': sha(jar) if jar else None, 'http_port': args.questdb_http_port,
         'ilp_port': args.questdb_ilp_port, 'root': str(base), 'scope': 'owned_loopback_only',
         'reference': 'https://questdb.com/docs/deployment/systemd.md'})


def spawn_historian(root, env, spawn):
    config = json.loads((root / 'questdb/launch.json').read_text())
    if config['jar']:
        require(sha(config['jar']) == config['jar_sha256'], 'Configured QuestDB jar changed after preparation; use a fresh workspace')
    # QDB_* overrides could otherwise redirect sockets/data into a pre-existing
    # database. No inherited QuestDB configuration is accepted by this fixture.
    isolated = {key: value for key, value in env.items()
                if not key.startswith('QDB_') and key not in ('JAVA_TOOL_OPTIONS', 'JDK_JAVA_OPTIONS', '_JAVA_OPTIONS')}
    child = spawn('questdb', config['argv'], root / 'questdb', isolated)
    deadline = time.monotonic() + 90
    url = f'http://127.0.0.1:{config["http_port"]}/exec?query=select%201'
    while True:
        require(child.poll() is None, 'Owned QuestDB exited; R5 is unavailable, inspect questdb log')
        try:
            with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(url, timeout=2) as response:
                result = json.load(response)
            require(result.get('dataset') == [[1]], 'Owned QuestDB did not execute its readiness query')
            return
        except (OSError, TimeoutError):
            require(time.monotonic() < deadline, 'Owned QuestDB did not become ready; R5 remains unaccepted')
            time.sleep(0.5)
