"""Audit the committed three-package closure, with explicit development/stage modes."""
import argparse
import json
import pathlib
import re
import subprocess
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[1]
GIT = {
    'zcall': 'https://github.com/LIghtJUNction/zcall.git',
    'zconv': 'https://github.com/LIghtJUNction/zconv.git',
}
REGISTRY = 'registry+https://github.com/rust-lang/crates.io-index'
EDGES = {'zmcpio': ['zcall'], 'zcall': ['zconv'], 'zconv': []}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def validate_manifest(value, mode='development'):
    require(mode in ('development', 'registry'), 'Unknown audit mode')
    require(value.get('package', {}).get('name') == 'zmcpio', 'Unexpected root package')
    require(all(not value.get(k) for k in ('dev-dependencies', 'build-dependencies', 'target', 'patch', 'replace')),
            'Extra dependency tables are not allowed')
    require(not value.get('workspace', {}).get('dependencies'), 'Workspace dependency fallback is not allowed')
    require(set(value.get('dependencies', {})) == {'zcall'}, 'Expected only the zcall dependency')
    dep = value['dependencies']['zcall']
    require(isinstance(dep, dict), 'Expected a dependency table')
    source_key = 'git' if mode == 'development' else 'version'
    require(set(dep) <= {source_key, 'default-features', 'optional', 'features'},
            'Dependency source selectors cannot be mixed')
    require(dep.get('default-features') is False and not dep.get('optional') and not dep.get('features'),
            'No default, optional or extra dependency features are permitted')
    if mode == 'development':
        require(dep.get('git') == GIT['zcall'], 'Development must use the canonical Git default branch without version')
    else:
        require(isinstance(dep.get('version'), str) and re.fullmatch(r'=[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.+-]+)?', dep['version']),
                'Registry stage must use an exact locked version')
    return dep


def validate_lock(value, lock, mode='development'):
    dep = validate_manifest(value, mode)
    entries = lock.get('package', [])
    require(len(entries) == 3 and {p.get('name') for p in entries} == set(EDGES),
            'Lock must contain exactly one zmcpio, zcall and zconv')
    packages = {p['name']: p for p in entries}
    for name, item in packages.items():
        require(isinstance(item.get('version'), str) and item['version'], 'Lock version is missing')
        require(item.get('dependencies', []) == EDGES[name], 'Unexpected locked dependency closure')
        if name == 'zmcpio':
            require(item['version'] == value['package']['version'] and not item.get('source') and not item.get('checksum'),
                    'Root package identity differs from the manifest')
        elif mode == 'development':
            require(re.fullmatch(re.escape('git+' + GIT[name]) + r'#[0-9a-f]{40}', item.get('source', '')) is not None,
                    'Lock must bind each canonical Git URL to a full commit')
            require('checksum' not in item, 'Git packages must not masquerade as registry packages')
        else:
            require(item.get('source') == REGISTRY and re.fullmatch(r'[0-9a-f]{64}', item.get('checksum', '')) is not None,
                    'Registry package source/checksum is invalid')
    if mode == 'registry':
        require(dep['version'] == '=' + packages['zcall']['version'], 'Stage manifest does not match registry lock')
    return packages


def validate_metadata(data, value, lock, root, mode='development'):
    packages = validate_lock(value, lock, mode)
    items = data.get('packages', [])
    require(len(items) == 3 and {p.get('name') for p in items} == set(EDGES),
            'Resolved graph must contain exactly the three locked packages')
    ids = {}
    for item in items:
        name = item['name']
        require(item.get('version') == packages[name]['version'] and item.get('source') == packages[name].get('source'),
                'Resolved package identity/version does not match the committed lock')
        require(item.get('id') not in ids, 'Resolved package IDs must be unique')
        ids[item.get('id')] = name
        if name == 'zmcpio':
            require(pathlib.Path(item['manifest_path']).resolve() == (pathlib.Path(root) / 'Cargo.toml').resolve(),
                    'Resolved root is not this source directory')
        deps = item.get('dependencies', [])
        require(len(deps) == len(EDGES[name]) and {d.get('name') for d in deps} == set(EDGES[name]),
                'Resolved declarations changed the three-package closure')
        for dependency in deps:
            target = dependency['name']
            require(dependency.get('kind') is None and not dependency.get('optional')
                    and dependency.get('uses_default_features') is False and not dependency.get('features')
                    and not dependency.get('target') and not dependency.get('rename'),
                    'Extra dev/build/optional/target/feature dependency is not permitted')
            expected = 'git+' + GIT[target] if mode == 'development' else REGISTRY
            require(dependency.get('source') == expected, 'Resolved declaration uses a different source')
    nodes = data.get('resolve', {}).get('nodes', [])
    require(len(nodes) == 3 and {n.get('id') for n in nodes} == set(ids), 'Resolved nodes differ from locked packages')
    for node in nodes:
        require([ids.get(d) for d in node.get('dependencies', [])] == EDGES[ids[node['id']]],
                'Resolved node edges differ from locked packages')
        allowed = {'default'} if ids[node['id']] == 'zmcpio' else set()
        require(set(node.get('features', [])) <= allowed, 'Resolved dependency features must remain disabled')
    return packages


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--mode', choices=('development', 'registry'), default='development')
    mode = parser.parse_args().mode
    require(not (ROOT / 'build.rs').exists(), 'Root build scripts are not permitted')
    value = tomllib.loads((ROOT / 'Cargo.toml').read_text())
    lock = tomllib.loads((ROOT / 'Cargo.lock').read_text())
    validate_lock(value, lock, mode)
    if mode == 'development':
        for name in ('Cargo.toml', 'Cargo.lock'):
            committed = subprocess.check_output(['git', '-C', str(ROOT), 'rev-parse', 'HEAD:' + name])
            # Compare Git content identities, including normal CRLF checkout conversion.
            working = subprocess.check_output(
                ['git', '-C', str(ROOT), 'hash-object', '--path=' + name, str(ROOT / name)])
            require(committed.strip() == working.strip(), 'Audit requires the committed source manifest and lock')
    else:
        from release import stage_receipt
        stage_receipt(ROOT)
    data = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--format-version', '1'], cwd=ROOT))
    validate_metadata(data, value, lock, ROOT, mode)
    print(mode + ': zmcpio -> zcall -> zconv; exact committed identities, no extra dependency features')


if __name__ == '__main__':
    main()
