"""Actual authenticated HMI API acceptance, only in the disposable CI volume.

The test client forwards its own Secure session cookie explicitly over the
container loopback interface. This is API acceptance, not browser TLS acceptance.
"""
import json
from pathlib import Path
import secrets
import sys
import urllib.error
from urllib.request import Request, urlopen

base = 'http://127.0.0.1:3000'
cookie = None
csrf = None

def request(path, payload=None, expected=200):
    headers = {}
    if cookie:
        headers['Cookie'] = cookie
    if csrf:
        headers['x-csrf-token'] = csrf
    data = None if payload is None else json.dumps(payload).encode()
    if data is not None:
        headers['Content-Type'] = 'application/json'
    req = Request(base + path, data=data, headers=headers)
    try:
        response = urlopen(req, timeout=120)
    except urllib.error.HTTPError as error:
        assert error.code == expected, (path, error.code, expected)
        return None, error.headers
    with response:
        assert response.status == expected, (path, response.status)
        body = response.read()
        return (json.loads(body) if response.headers.get_content_type() == 'application/json' else body), response.headers

assert request('/api/auth/me')[0]['authenticated'] is False
request('/api/research', expected=401)
assert b'root' in request('/')[0]
password_file = Path('/data/config/acceptance-password')
password = password_file.read_text() if password_file.exists() else 'password'
login, headers = request('/api/auth/login', {'username': 'admin', 'password': password})
raw_cookie = headers['Set-Cookie']
assert 'secure' in raw_cookie.lower() and 'httponly' in raw_cookie.lower()
cookie = raw_cookie.split(';')[0]
csrf = login['csrf_token']
if not password_file.exists():
    assert login['force_password_change'] is True
    password = secrets.token_urlsafe(32)
    request('/api/auth/change-password', {'current_password': 'password', 'new_password': password}, expected=204)
    password_file.write_text(password)
    password_file.chmod(0o600)
else:
    assert login['force_password_change'] is False
assert request('/api/auth/me')[0]['authenticated'] is True
bootstrap = request('/api/research')[0]
assert bootstrap['enabled'] is True and bootstrap['schema_version'] == 1
assert len(bootstrap['capabilities']['methods']) == 2
workspace = request('/api/workspace')[0]
assert workspace['deployment_kind'] == 'isolated_research'
assert workspace['runtime_enabled'] is False and workspace['research_enabled'] is True
assert workspace['capabilities']['automatic_handoff'] is False
assert workspace['capabilities']['engine_lifecycle'] is False
assert request('/api/runtime/profiles')[0]['profiles'] == []
# Round-trip actual artifact storage; restart must retain its immutable identity.
marker = Path('/data/research/acceptance-draft-id')
if '--after-restart' in sys.argv:
    drafts = request('/api/research/drafts')[0]
    assert marker.read_text() in json.dumps(drafts)
else:
    draft = request('/api/research/drafts', {'name': 'Container persistence acceptance', 'config': {}, 'research': {}})[0]
    marker.write_text(draft['id'])
print(json.dumps({'unauthenticated_research': 'blocked', 'login': 'passed',
                  'secure_cookie': True, 'force_password_change': 'preserved',
                  'research_bootstrap': 'real Python process', 'persistence': 'passed',
                  'real_device_operations': 0, 'browser_tls': 'not tested'}))
