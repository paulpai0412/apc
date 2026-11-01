"""32-scan packaging acceptance, not the full three-month research qualification."""
import json
from pathlib import Path
import subprocess
import tempfile
import numpy as np

with tempfile.TemporaryDirectory(dir='/tmp') as folder:
    root = Path(folder)
    plant = {'sample_time': 1.0, 'baseline_cv': [0.0], 'baseline_mv': [0.0], 'baseline_dv': [0.0],
             'actuator_tau': [1.0], 'actuator_low': [-2.0], 'actuator_high': [2.0], 'actuator_rate': [0.3],
             'paths': [[{'gain': 1.0, 'tau1': 2.0, 'tau2': 1.0, 'delay_seconds': 1.0},
                        {'gain': -0.4, 'tau1': 2.0, 'tau2': 1.0, 'delay_seconds': 1.0}]],
             'nonlinear_strength': [0.0], 'nonlinear_scale': [1.0]}
    scenario = {'sample_time': 1.0, 'time_seconds': list(range(32)), 'cv': [[0.0]] * 32,
                'mv': [[0.0]] * 32, 'dv': [[0.1 if n > 15 else 0.0] for n in range(32)], 'targets': [[0.5]] * 32}
    for name, value in [('plant', plant), ('scenario', scenario)]:
        (root / (name + '.json')).write_text(json.dumps(value))
    results = {}
    for mode in ['direct', 'opc-da']:
        output = root / (mode + '.json')
        argv = ['/app/bin/mpc_campaign', 'closed-loop', '--plant', str(root / 'plant.json'),
                '--scenario', str(root / 'scenario.json'), '--controller', '/app/validation/examples/synthetic_model.json',
                '--output', str(output)]
        if mode == 'opc-da':
            argv += ['--transport', 'opc-da', '--bridge', '/usr/bin/wine', '--bridge-arg', '/app/com/opc_da_bridge.exe']
        subprocess.run(argv, check=True, capture_output=True, timeout=120)
        result = json.loads(output.read_text())
        assert result['status'] == 'valid' and result['samples'] == 32
        results[mode] = result
    differences = {}
    for key in ['time_seconds', 'cv', 'mv', 'dv', 'targets', 'command_mv']:
        a, b = np.asarray(results['direct']['trace'][key]), np.asarray(results['opc-da']['trace'][key])
        assert np.allclose(a, b, rtol=1e-6, atol=1e-6), key
        differences[key] = float(np.max(np.abs(a - b)))
    assert results['opc-da']['transport_stats']['read_calls'] > 0
    assert results['opc-da']['transport_stats']['write_calls'] > 0
    print(json.dumps({'samples': 32, 'scope': 'packaging smoke only',
                      'actual_com': True, 'direct_com_max_differences': differences,
                      'transport_stats': results['opc-da']['transport_stats'], 'real_device_control': False}))
