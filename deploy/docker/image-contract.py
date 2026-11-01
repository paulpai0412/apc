import json
import os
print(json.dumps({
    "image": "ghcr.io/paulpai0412/apc@" + os.environ["DIGEST"],
    "source_revision": os.environ["SOURCE_REVISION"],
    "source": "https://github.com/paulpai0412/apc",
    "ci_run": os.environ["CI_RUN"],
    "platform": "linux/arm64",
    "mode": "isolated_research",
    "opc_connected": False,
    "historian_connected": False,
    "real_device_control": False,
    "com_tests": "passed",
    "hmi_authenticated_smoke": "passed",
    "desktop_mobile_browser_smoke": "passed",
    "field_runtime_entrance": "disabled by existing isolated-research contract",
    "volume": "apc_apc-data:/data",
    "host_binding": "127.0.0.1:18082",
    "transport": "Windows x64 COM simulator under Hangover 11.16 / FEX",
    "registry_visibility": "unchanged; new GHCR packages default private",
    "vm_build": False,
}, indent=2))
