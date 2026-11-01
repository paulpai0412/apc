import json
from urllib.request import urlopen
with urlopen('http://127.0.0.1:3000/api/auth/me', timeout=3) as response:
    assert response.status == 200
    assert isinstance(json.load(response).get('authenticated'), bool)
