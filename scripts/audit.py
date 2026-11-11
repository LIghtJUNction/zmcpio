import json
import pathlib
import subprocess
import tomllib

root = pathlib.Path(__file__).resolve().parents[1]
m = tomllib.loads((root / 'Cargo.toml').read_text())
assert not (root / 'build.rs').exists()
assert all(not m.get(k) for k in ['dev-dependencies', 'build-dependencies', 'target'])
assert set(m['dependencies']) == {'zcall'}
dep = m['dependencies']['zcall']
assert dep['version'].startswith('=') and dep['default-features'] is False
assert not any(dep.get(k) for k in ['optional', 'features', 'path', 'git'])
data = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--format-version', '1'], cwd=root))
packages = {p['name']: p for p in data['packages']}
assert len(data['packages']) == 3 and set(packages) == {'zmcpio', 'zcall', 'zconv'}
assert [d['name'] for d in packages['zmcpio']['dependencies']] == ['zcall']
assert [d['name'] for d in packages['zcall']['dependencies']] == ['zconv']
assert not packages['zconv']['dependencies']
assert all(d['kind'] is None and not d['optional'] and not d['uses_default_features']
           for p in packages.values() for d in p['dependencies'])
print('Dependency closure audited: zmcpio -> zcall -> zconv; no other dependencies')
