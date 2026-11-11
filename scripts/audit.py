import json
import pathlib
import subprocess
import tomllib

root = pathlib.Path(__file__).resolve().parents[1]
m = tomllib.loads((root / 'Cargo.toml').read_text())
assert not (root / 'build.rs').exists()
assert all(not m.get(k) for k in ['dependencies', 'dev-dependencies', 'build-dependencies', 'target'])
data = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--format-version', '1'], cwd=root))
assert len(data['packages']) == 1 and not data['packages'][0]['dependencies']
print('Complete dependency closure audited: zero third-party dependencies')
