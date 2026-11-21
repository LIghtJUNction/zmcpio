"""Git-only source releases and an isolated, locked registry projection."""
import copy
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tarfile
import tomllib
import urllib.error
import urllib.request

from audit import GIT, REGISTRY, EDGES, require, validate_lock

ROOT = pathlib.Path(__file__).resolve().parents[1]
SEMVER = re.compile(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?\Z', re.ASCII)

def parse_version(text):
    match = SEMVER.fullmatch(text)
    if not match or len(text) > 128:
        raise ValueError('Expected a legal SemVer version')
    core = tuple(int(match[i]) for i in range(1, 4))
    if any(n >= 2**64 for n in core):
        raise ValueError('Version component exceeds Cargo u64 range')
    pre = match[4]
    if pre and any(s.isdigit() and len(s) > 1 and s[0] == '0' for s in pre.split('.')):
        raise ValueError('Numeric prerelease identifiers cannot have leading zeroes')
    return core, pre

def precedence(text):
    core, pre = parse_version(text)
    parts = tuple((0, int(s)) if s.isdigit() else (1, s) for s in pre.split('.')) if pre else ()
    return core, pre is None, parts

def next_version(current, bump='patch', manual=''):
    core, _ = parse_version(current)
    if manual:
        parse_version(manual)
        if precedence(manual) < precedence(current):
            raise ValueError('Version must not decrease')
        return manual
    if bump not in ('patch', 'minor', 'major'):
        raise ValueError('Unknown bump mode')
    values = list(core)
    position = {'major': 0, 'minor': 1, 'patch': 2}[bump]
    values[position] += 1
    for i in range(position + 1, 3):
        values[i] = 0
    result = '.'.join(map(str, values))
    parse_version(result)
    return result

def get_json(path, allow_missing=False):
    request = urllib.request.Request('https://crates.io/api/v1/' + path, headers={'User-Agent': 'zmcpio-release-preflight/0.1 (https://github.com/LIghtJUNction/zmcpio)'})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        if allow_missing and error.code == 404:
            return None
        raise

def git(root, *args):
    return subprocess.run(['git', '-C', str(root), *args], check=True,
                          text=True, capture_output=True).stdout


def digest(path):
    return hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()


def manifest():
    return tomllib.loads((ROOT / 'Cargo.toml').read_text())


def source_snapshot(expected_source=None):
    head = git(ROOT, 'rev-parse', 'HEAD').strip()
    require(re.fullmatch(r'[0-9a-f]{40}', head), 'Invalid source commit')
    require(expected_source is None or head == expected_source, 'Source HEAD changed')
    require(not git(ROOT, 'status', '--porcelain', '--untracked-files=all'),
            'Release staging requires a clean committed source')
    texts = {name: git(ROOT, 'show', 'HEAD:' + name) for name in ('Cargo.toml', 'Cargo.lock')}
    for name, text in texts.items():
        require((ROOT / name).read_text() == text, 'Working source differs from committed input')
    value = tomllib.loads(texts['Cargo.toml'])
    lock = tomllib.loads(texts['Cargo.lock'])
    packages = validate_lock(value, lock)
    for item in packages.values():
        parse_version(item['version'])
    require(value['package'].get('repository') == 'https://github.com/LIghtJUNction/zmcpio',
            'Unexpected source repository')
    return {'commit': head, 'tree': git(ROOT, 'rev-parse', 'HEAD^{tree}').strip(),
            'texts': texts, 'manifest': value, 'lock': lock, 'packages': packages}


def registry_identity(name, repository, allow_missing=False):
    crate = get_json('crates/' + name, allow_missing=allow_missing)
    if crate is None:
        return False
    info = crate.get('crate', {})
    require(info.get('id') == name and info.get('repository', '').rstrip('/') == repository,
            'Registry crate repository/name does not match the expected identity')
    owners = get_json('crates/' + name + '/owners')
    owner = repository.removeprefix('https://github.com/').split('/')[0].casefold()
    require(owner in {u.get('login', '').casefold() for u in owners.get('users', [])},
            'Package-name collision: current repository owner is not a crate owner')
    return True


def dependency_registry(snapshot):
    resolved = {}
    for name in ('zcall', 'zconv'):
        item = snapshot['packages'][name]
        version = item['version']
        require(registry_identity(name, GIT[name].removesuffix('.git')), 'Dependency is not registered')
        record = get_json('crates/' + name + '/' + version).get('version', {})
        require(record.get('crate') == name and record.get('num') == version and record.get('yanked') is False,
                'Registry dependency identity/version/yank state differs from the committed lock')
        checksum = record.get('checksum', '')
        require(re.fullmatch(r'[0-9a-f]{64}', checksum), 'Registry dependency checksum is invalid')
        deps = get_json('crates/' + name + '/' + version + '/dependencies').get('dependencies')
        require(isinstance(deps, list) and len(deps) == len(EDGES[name])
                and {d.get('crate_id') for d in deps} == set(EDGES[name]),
                'Registry dependency closure differs from the three-package contract')
        for dep in deps:
            require(dep.get('req') == '=' + snapshot['packages'][dep['crate_id']]['version']
                    and dep.get('kind') == 'normal' and dep.get('optional') is False
                    and dep.get('default_features') is False and not dep.get('features')
                    and not dep.get('target') and not dep.get('registry'),
                    'Registry dependency requirement/features do not match the locked closure')
        resolved[name] = {'version': version, 'checksum': checksum, 'git_source': item['source']}
    return resolved


def registry_preflight(package, version):
    snapshot = source_snapshot()
    require(package == snapshot['manifest']['package'], 'Package changed before preflight')
    parse_version(version)
    repository = os.environ.get('GITHUB_REPOSITORY', '')
    require(package['repository'].rstrip('/') == 'https://github.com/' + repository,
            'Manifest repository does not match trusted workflow repository')
    registry_identity(package['name'], package['repository'], allow_missing=True)
    require(get_json('crates/' + package['name'] + '/' + version, allow_missing=True) is None,
            'This crate version already exists')
    return dependency_registry(snapshot)


def replace_field(text, key, old, new):
    pattern = r'(?m)^' + re.escape(key) + r'[ \t]*=[ \t]*"' + re.escape(old) + r'"[ \t]*$'
    updated, count = re.subn(pattern, key + ' = "' + new + '"', text)
    require(count == 1, 'Cannot uniquely identify ' + key + ' declaration')
    return updated


def package_version_text(text, old, new):
    pattern = r'(?ms)(^\[package\][ \t]*\n)(.*?)(?=^\[|\Z)'
    def update(match):
        return match[1] + replace_field(match[2], 'version', old, new)
    result, count = re.subn(pattern, update, text)
    require(count == 1, 'Cannot identify package table')
    return result


def map_lock(text, rewrite):
    pieces = re.split(r'(?m)^\[\[package\]\][ \t]*$', text)
    require(len(pieces) == 4, 'Expected exactly three lock package tables')
    output = pieces[0]
    for block in pieces[1:]:
        item = tomllib.loads('[[package]]' + block)['package'][0]
        output += '[[package]]' + rewrite(item, block)
    return output


def apply_version(version):
    snapshot = source_snapshot()
    old = snapshot['manifest']['package']['version']
    next_version(old, manual=version)
    manifest_text = package_version_text(snapshot['texts']['Cargo.toml'], old, version)
    lock_text = map_lock(snapshot['texts']['Cargo.lock'],
                         lambda p, t: replace_field(t, 'version', old, version) if p['name'] == 'zmcpio' else t)
    expected_manifest = copy.deepcopy(snapshot['manifest'])
    expected_manifest['package']['version'] = version
    expected_lock = copy.deepcopy(snapshot['lock'])
    next(p for p in expected_lock['package'] if p['name'] == 'zmcpio')['version'] = version
    require(tomllib.loads(manifest_text) == expected_manifest and tomllib.loads(lock_text) == expected_lock,
            'Version bump would change dependency identity')
    validate_lock(expected_manifest, expected_lock)
    (ROOT / 'Cargo.toml').write_text(manifest_text)
    (ROOT / 'Cargo.lock').write_text(lock_text)


def stage_registry(destination, expected_source=None):
    destination = pathlib.Path(destination).resolve()
    require(destination != ROOT.resolve() and ROOT.resolve() not in destination.parents,
            'Registry staging must be outside the development checkout')
    require(not destination.exists(), 'Stage destination already exists')
    snapshot = source_snapshot(expected_source)
    dependencies = dependency_registry(snapshot)
    value = copy.deepcopy(snapshot['manifest'])
    value['dependencies']['zcall'].pop('git')
    value['dependencies']['zcall']['version'] = '=' + dependencies['zcall']['version']
    pattern = r'\bgit[ \t]*=[ \t]*"' + re.escape(GIT['zcall']) + r'"'
    manifest_text, count = re.subn(pattern, 'version = "=' + dependencies['zcall']['version'] + '"',
                                  snapshot['texts']['Cargo.toml'])
    require(count == 1 and tomllib.loads(manifest_text) == value, 'Registry projection changed another manifest field')
    def rewrite_lock(item, text):
        if item['name'] == 'zmcpio':
            return text
        dep = dependencies[item['name']]
        result = replace_field(text, 'source', item['source'], REGISTRY)
        return result.replace('source = "' + REGISTRY + '"',
                              'source = "' + REGISTRY + '"\nchecksum = "' + dep['checksum'] + '"', 1)
    lock_text = map_lock(snapshot['texts']['Cargo.lock'], rewrite_lock)
    validate_lock(value, tomllib.loads(lock_text), 'registry')
    provenance = {
        'schema': 1, 'repository': snapshot['manifest']['package']['repository'],
        'source_commit': snapshot['commit'], 'source_tree': snapshot['tree'],
        'source_inputs': {name: hashlib.sha256(text.encode()).hexdigest() for name, text in snapshot['texts'].items()},
        'dependencies': dependencies,
    }
    # Nothing above creates or modifies a directory. From here on cleanup owns only this new clone.
    created = False
    try:
        destination.mkdir(parents=True, exist_ok=False)
        created = True
        git(ROOT, 'clone', '--quiet', '--no-hardlinks', '--no-checkout', str(ROOT.resolve()), str(destination))
        git(destination, 'checkout', '--quiet', '--detach', snapshot['commit'])
        require(not (destination / 'docs/registry-source.json').exists(), 'Reserved provenance path already exists')
        (destination / 'Cargo.toml').write_text(manifest_text)
        (destination / 'Cargo.lock').write_text(lock_text)
        (destination / 'docs').mkdir(exist_ok=True)
        (destination / 'docs/registry-source.json').write_text(json.dumps(provenance, indent=2) + '\n')
        git(destination, 'config', 'user.name', 'Registry staging')
        git(destination, 'config', 'user.email', 'registry-staging@example.invalid')
        git(destination, 'add', 'Cargo.toml', 'Cargo.lock', 'docs/registry-source.json')
        git(destination, '-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m',
            'Stage registry package from ' + snapshot['commit'])
        receipt = dict(provenance, source_root=str(ROOT.resolve()),
                       stage_commit=git(destination, 'rev-parse', 'HEAD').strip(),
                       stage_tree=git(destination, 'rev-parse', 'HEAD^{tree}').strip(),
                       stage_inputs={p: digest(destination / p) for p in ('Cargo.toml', 'Cargo.lock', 'docs/registry-source.json')})
        (destination / '.git/zmcpio-registry-stage.json').write_text(json.dumps(receipt, indent=2) + '\n')
        stage_receipt(destination)
        # Re-read the original checkout after the projection, including its lock and HEAD.
        require(source_snapshot(snapshot['commit']) == snapshot, 'Source changed during registry staging')
        return receipt
    except BaseException:
        if created:
            shutil.rmtree(destination)
        raise


def stage_receipt(stage):
    stage = pathlib.Path(stage).resolve()
    path = stage / '.git/zmcpio-registry-stage.json'
    require(path.is_file(), 'This is not an owned registry stage')
    receipt = json.loads(path.read_text())
    require(receipt.get('schema') == 1 and receipt.get('repository') == 'https://github.com/LIghtJUNction/zmcpio',
            'Invalid stage provenance')
    require(git(stage, 'rev-parse', 'HEAD').strip() == receipt['stage_commit']
            and git(stage, 'rev-parse', 'HEAD^{tree}').strip() == receipt['stage_tree']
            and git(stage, 'rev-parse', 'HEAD^').strip() == receipt['source_commit']
            and not git(stage, 'status', '--porcelain', '--untracked-files=all'),
            'Registry stage changed after creation')
    require(set(receipt['stage_inputs']) == {'Cargo.toml', 'Cargo.lock', 'docs/registry-source.json'},
            'Incomplete registry stage inputs')
    for name, checksum in receipt['stage_inputs'].items():
        require(digest(stage / name) == checksum, 'Registry stage input changed')
    provenance = json.loads((stage / 'docs/registry-source.json').read_text())
    require(all(receipt[k] == provenance[k] for k in provenance), 'Stage receipt/provenance mismatch')
    packages = validate_lock(tomllib.loads((stage / 'Cargo.toml').read_text()),
                             tomllib.loads((stage / 'Cargo.lock').read_text()), 'registry')
    require(set(receipt['dependencies']) == set(GIT), 'Incomplete staged dependency mapping')
    source_texts = {name: git(stage, 'show', receipt['source_commit'] + ':' + name)
                    for name in ('Cargo.toml', 'Cargo.lock')}
    require(git(stage, 'rev-parse', receipt['source_commit'] + '^{tree}').strip() == receipt['source_tree']
            and {name: hashlib.sha256(text.encode()).hexdigest() for name, text in source_texts.items()}
            == receipt['source_inputs'], 'Stage does not bind the original committed source')
    source_packages = validate_lock(tomllib.loads(source_texts['Cargo.toml']),
                                    tomllib.loads(source_texts['Cargo.lock']))
    for name, item in receipt['dependencies'].items():
        require(source_packages[name]['version'] == item['version']
                and source_packages[name]['source'] == item['git_source'],
                'Stage dependency mapping differs from the original Git lock')
        require(packages[name]['version'] == item['version'] and packages[name]['checksum'] == item['checksum'],
                'Registry stage dependency differs from verified registry identity')
    return receipt


def verify_package(stage, archive):
    stage = pathlib.Path(stage).resolve()
    receipt = stage_receipt(stage)
    value = tomllib.loads((stage / 'Cargo.toml').read_text())
    name, version = value['package']['name'], value['package']['version']
    prefix = name + '-' + version + '/'
    files = {}
    with tarfile.open(archive, 'r:gz') as package:
        for member in package:
            require(member.name.startswith(prefix) and '..' not in pathlib.PurePosixPath(member.name).parts,
                    'Unexpected archive path')
            if member.isdir():
                continue
            require(member.isfile() and member.name[len(prefix):] not in files, 'Non-file or duplicate archive member')
            files[member.name[len(prefix):]] = package.extractfile(member).read()
    required = {'Cargo.toml', 'Cargo.toml.orig', 'Cargo.lock', '.cargo_vcs_info.json', 'docs/registry-source.json'}
    require(required <= set(files), 'Package is missing registry provenance inputs')
    normalized = tomllib.loads(files['Cargo.toml'].decode())
    locked = tomllib.loads(files['Cargo.lock'].decode())
    validate_lock(normalized, locked, 'registry')
    require(normalized['package']['name'] == name and normalized['package']['version'] == version,
            'Package identity differs from registry stage')
    require(locked == tomllib.loads((stage / 'Cargo.lock').read_text()), 'Packaged lock changed')
    require(files['Cargo.toml.orig'] == (stage / 'Cargo.toml').read_bytes(), 'Original package manifest changed')
    vcs = json.loads(files['.cargo_vcs_info.json'])
    require(vcs.get('git', {}).get('sha1') == receipt['stage_commit'] and vcs['git'].get('dirty', False) is False
            and vcs.get('path_in_vcs', '') == '', 'Package VCS identity is not the clean registry stage')
    for path, contents in files.items():
        if path not in ('Cargo.toml', 'Cargo.toml.orig', 'Cargo.lock', '.cargo_vcs_info.json'):
            require((stage / path).is_file() and (stage / path).read_bytes() == contents,
                    'Packaged source differs from registry stage: ' + path)
    result = {'source_commit': receipt['source_commit'], 'stage_commit': receipt['stage_commit'],
              'package': name, 'version': version, 'archive': str(pathlib.Path(archive).resolve()),
              'bytes': pathlib.Path(archive).stat().st_size, 'sha256': digest(archive)}
    (stage / '.git/zmcpio-registry-package.json').write_text(json.dumps(result, indent=2) + '\n')
    return result


def push_source(stage, expected_base, branch):
    receipt = stage_receipt(stage)
    snapshot = source_snapshot()
    require(snapshot['commit'] == receipt['source_commit'] and snapshot['tree'] == receipt['source_tree'],
            'Only the verified Git-only source may be pushed')
    require(re.fullmatch(r'[0-9a-f]{40}', expected_base), 'Expected source commit is required')
    git(ROOT, 'check-ref-format', '--branch', branch)
    git(ROOT, 'merge-base', '--is-ancestor', expected_base, snapshot['commit'])
    require(set(git(ROOT, 'diff', '--name-only', expected_base, snapshot['commit']).splitlines())
            <= {'Cargo.toml', 'Cargo.lock'}, 'Source release commit changed more than the version inputs')
    base_manifest = tomllib.loads(git(ROOT, 'show', expected_base + ':Cargo.toml'))
    base_lock = tomllib.loads(git(ROOT, 'show', expected_base + ':Cargo.lock'))
    validate_lock(base_manifest, base_lock)
    base_manifest['package']['version'] = snapshot['manifest']['package']['version']
    next(p for p in base_lock['package'] if p['name'] == 'zmcpio')['version'] = base_manifest['package']['version']
    require(base_manifest == snapshot['manifest'] and base_lock == snapshot['lock'],
            'Source release commit changed a dependency or package field')
    git(ROOT, 'fetch', 'origin', branch)
    require(git(ROOT, 'rev-parse', 'FETCH_HEAD').strip() == expected_base,
            'Default branch changed; refusing to overwrite concurrent source')
    git(ROOT, 'push', 'origin', 'HEAD:refs/heads/' + branch)


def verify_published(stage, archive):
    package = verify_package(stage, archive)
    require(registry_identity(package['package'], 'https://github.com/LIghtJUNction/zmcpio'),
            'Published package identity mismatch')
    record = get_json('crates/' + package['package'] + '/' + package['version']).get('version', {})
    require(record.get('crate') == package['package'] and record.get('num') == package['version']
            and record.get('yanked') is False and record.get('checksum') == package['sha256'],
            'Published checksum/name/version does not match the verified package')
    return package


def archive_argument(stage):
    if len(sys.argv) > 3:
        return pathlib.Path(sys.argv[3])
    value = tomllib.loads((pathlib.Path(stage) / 'Cargo.toml').read_text())['package']
    return pathlib.Path(os.environ['REGISTRY_TARGET']) / 'package' / (value['name'] + '-' + value['version'] + '.crate')


def main():
    command = sys.argv[1]
    package = manifest()['package']
    if command == 'plan':
        version = next_version(package['version'], os.environ.get('BUMP', 'patch'), os.environ.get('MANUAL_VERSION', ''))
        if os.environ.get('PUBLISH') == 'true':
            registry_preflight(package, version)
        print('Planned version:', version)
        if os.environ.get('GITHUB_OUTPUT'):
            with open(os.environ['GITHUB_OUTPUT'], 'a', encoding='utf-8') as output:
                output.write('version=' + version + '\n')
    elif command == 'apply':
        apply_version(os.environ['VERSION'])
    elif command == 'preflight':
        registry_preflight(package, package['version'])
    elif command == 'stage':
        print(json.dumps(stage_registry(sys.argv[2], os.environ.get('SOURCE_COMMIT')), indent=2))
    elif command == 'verify-stage':
        print(json.dumps(stage_receipt(sys.argv[2]), indent=2))
    elif command == 'verify-package':
        print(json.dumps(verify_package(sys.argv[2], archive_argument(sys.argv[2])), indent=2))
    elif command == 'push-source':
        push_source(sys.argv[2], os.environ['GITHUB_SHA'], os.environ['DEFAULT_BRANCH'])
    elif command == 'verify-published':
        print(json.dumps(verify_published(sys.argv[2], archive_argument(sys.argv[2])), indent=2))
    else:
        raise ValueError('Unknown release command')


if __name__ == '__main__':
    main()
