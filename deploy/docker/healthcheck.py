"""Readiness is real COM acceptance plus the actual HMI, not a static index."""
import json
from pathlib import Path
from urllib.request import urlopen

assert Path('/tmp/apc-com-ready').read_text().strip() == 'real-com-tests-passed'
with urlopen('http://127.0.0.1:3000/api/auth/me', timeout=3) as response:
    data = json.load(response)
    assert response.status == 200 and isinstance(data.get('authenticated'), bool)
