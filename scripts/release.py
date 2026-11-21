"""Git-only source releases and an isolated, locked registry projection."""
import copy
import contextlib
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
from types import ModuleType
import urllib.error
import urllib.request

from audit import GIT, REGISTRY, EDGES, require, validate_lock, validate_metadata

ROOT = pathlib.Path(os.environ.get('RELEASE_SOURCE_ROOT', pathlib.Path(__file__).resolve().parents[1])).resolve()
PROJECTION_TOOL_SHA = '8df81739982dd60c760d0d47c4f7b02df0e1db74'
REGISTRY_TOOLCHAIN = '1.100.0'
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
                          text=True, capture_output=True,
                          env={**os.environ, 'GIT_NO_REPLACE_OBJECTS': '1'}).stdout


def digest(path):
    return hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()


def manifest():
    return tomllib.loads((ROOT / 'Cargo.toml').read_text())


def source_snapshot(expected_source=None):
    expected_source = expected_source or os.environ.get('SOURCE_COMMIT')
    head = git(ROOT, 'rev-parse', 'HEAD').strip()
    require(re.fullmatch(r'[0-9a-f]{40}', head), 'Invalid source commit')
    require(expected_source is None or head == expected_source, 'Source HEAD changed')
    require(not git(ROOT, 'status', '--porcelain', '--untracked-files=all'),
            'Release staging requires a clean committed source')
    blobs = {name: subprocess.run(['git', '-C', str(ROOT), 'show', head + ':' + name],
                                  check=True, capture_output=True,
                                  env={**os.environ, 'GIT_NO_REPLACE_OBJECTS': '1'}).stdout
             for name in ('Cargo.toml', 'Cargo.lock')}
    texts = {name: blob.decode().replace('\r\n', '\n') for name, blob in blobs.items()}
    for name, text in texts.items():
        require((ROOT / name).read_text() == text, 'Working source differs from committed input')
    value = tomllib.loads(texts['Cargo.toml'])
    lock = tomllib.loads(texts['Cargo.lock'])
    packages = validate_lock(value, lock)
    for item in packages.values():
        parse_version(item['version'])
    require(value['package'].get('repository') == 'https://github.com/LIghtJUNction/zmcpio',
            'Unexpected source repository')
    return {'commit': head, 'tree': git(ROOT, 'rev-parse', head + '^{tree}').strip(),
            'texts': texts, 'blobs': blobs, 'manifest': value, 'lock': lock, 'packages': packages}


def load_projection_tools():
    configured = os.environ.get('PROJECT_Z_TOOLS')
    require(configured, 'PROJECT_Z_TOOLS must identify the pinned Project Z checkout')
    tools = pathlib.Path(configured).resolve()
    require(git(tools, 'rev-parse', 'HEAD').strip() == PROJECTION_TOOL_SHA,
            'Project Z tools are not at the fixed projection revision')
    relative = 'scripts/release_projection_receipts.py'
    path = tools / relative
    expected = subprocess.run(['git', '-C', str(tools), 'show', PROJECTION_TOOL_SHA + ':' + relative],
                              check=True, capture_output=True,
                              env={**os.environ, 'GIT_NO_REPLACE_OBJECTS': '1'}).stdout
    require(path.is_file() and not path.is_symlink() and path.read_bytes() == expected,
            'Pinned projection verification source was modified')
    # Compile authenticated source bytes directly; a timestamp-matched .pyc is not trusted.
    gate = ModuleType('zmcpio_projection_verifier')
    gate.__file__ = str(path)
    gate.__package__ = ''
    exec(compile(expected, str(path), 'exec'), gate.__dict__)
    projection = gate.load_projection(tools, PROJECTION_TOOL_SHA)
    return tools, projection, gate


def registry_environment():
    require(os.environ.get('RUSTUP_TOOLCHAIN') == REGISTRY_TOOLCHAIN,
            'Registry staging and packaging require RUSTUP_TOOLCHAIN=' + REGISTRY_TOOLCHAIN)
    return {**os.environ, 'GIT_NO_REPLACE_OBJECTS': '1', 'PYTHONDONTWRITEBYTECODE': '1'}


@contextlib.contextmanager
def committed_git_bytes():
    # git archive otherwise honors the checkout's autocrlf setting. Request the
    # committed LF bytes without changing the user's index, worktree or config.
    count = int(os.environ.get('GIT_CONFIG_COUNT', '0'))
    values = {'GIT_CONFIG_COUNT': str(count + 2),
              'GIT_CONFIG_KEY_' + str(count): 'core.autocrlf',
              'GIT_CONFIG_VALUE_' + str(count): 'false',
              'GIT_CONFIG_KEY_' + str(count + 1): 'core.eol',
              'GIT_CONFIG_VALUE_' + str(count + 1): 'lf'}
    original = {key: os.environ.get(key) for key in values}
    try:
        os.environ.update(values)
        yield
    finally:
        for key, value in original.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value


def project_source(projection, commit, destination):
    with committed_git_bytes():
        return projection.stage(ROOT, commit, 'Cargo.toml', destination, projection.internal_repositories())


def require_outside_git(path):
    probe = subprocess.run(['git', '-C', str(path), 'rev-parse', '--show-toplevel'],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                           env={**os.environ, 'GIT_NO_REPLACE_OBJECTS': '1'})
    require(probe.returncode != 0, 'Registry source must be outside every Git checkout')
    require(not (path / '.git').exists() and not (path / '.cargo_vcs_info.json').exists(),
            'Registry source must not provide Git or fabricated Cargo VCS metadata')


def validate_registry_dependencies(value, lock, dependencies):
    packages = validate_lock(value, lock, 'registry')
    require(set(dependencies) == set(GIT), 'Incomplete staged dependency mapping')
    for name, dependency in dependencies.items():
        require(packages[name]['version'] == dependency['version']
                and packages[name]['checksum'] == dependency['checksum'],
                'Registry stage dependency differs from verified registry identity')
    return packages


def generate_registry_lock(stage, dependencies):
    env = registry_environment()
    subprocess.run(['cargo', 'generate-lockfile', '--manifest-path', str(stage / 'Cargo.toml')],
                   cwd=stage, check=True, env=env)
    value = tomllib.loads((stage / 'Cargo.toml').read_text())
    payload = (stage / 'Cargo.lock').read_bytes()
    lock = tomllib.loads(payload.decode())
    validate_registry_dependencies(value, lock, dependencies)
    metadata = subprocess.run(['cargo', 'metadata', '--locked', '--format-version', '1',
                               '--manifest-path', str(stage / 'Cargo.toml')],
                              cwd=stage, check=True, text=True, capture_output=True, env=env).stdout
    validate_metadata(json.loads(metadata), value, lock, stage, 'registry')
    require((stage / 'Cargo.lock').read_bytes() == payload, 'Cargo metadata changed the verified registry lock')
    return payload


def compare_projection(stage, expected):
    files = lambda root: {p.relative_to(root) for p in root.rglob('*') if not p.is_dir() or p.is_symlink()}
    actual_files, expected_files = files(stage), files(expected)
    require(actual_files == expected_files | {pathlib.Path('Cargo.lock')},
            'Registry stage has missing or additional source files')
    for relative in expected_files:
        left, right = stage / relative, expected / relative
        require(left.is_symlink() == right.is_symlink(), 'Registry stage source type changed: ' + str(relative))
        if right.is_symlink():
            equal = os.readlink(left) == os.readlink(right)
        else:
            equal = (left.is_file() and left.read_bytes() == right.read_bytes()
                     and (left.stat().st_mode & 0o111) == (right.stat().st_mode & 0o111))
        require(equal, 'Registry stage differs from the pinned source projection: ' + str(relative))


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


def projection_receipt(snapshot, dependencies, report, stage):
    return {
        'schema': 2, 'repository': snapshot['manifest']['package']['repository'],
        'source_root': str(ROOT.resolve()), 'source_commit': snapshot['commit'],
        'source_tree': snapshot['tree'],
        'source_inputs': {name: hashlib.sha256(blob).hexdigest() for name, blob in snapshot['blobs'].items()},
        'projection_tool_sha': PROJECTION_TOOL_SHA, 'cargo_toolchain': REGISTRY_TOOLCHAIN,
        'projected_manifest_sha256': report['projected_manifest_sha256'],
        'registry_lock_sha256': digest(stage / 'Cargo.lock'), 'dependencies': dependencies,
    }


def stage_registry(destination, expected_source=None):
    destination = pathlib.Path(destination).resolve()
    require(destination != ROOT.resolve() and ROOT.resolve() not in destination.parents,
            'Registry staging must be outside the development checkout')
    require(not destination.exists(), 'Stage destination already exists')
    registry_environment()
    snapshot = source_snapshot(expected_source)
    dependencies = dependency_registry(snapshot)
    _, projection, gate = load_projection_tools()
    allowlist = projection.internal_repositories()
    # The official tool archives the immutable commit and renders its complete
    # manifests. No Git repository or local staging commit is created.
    report = project_source(projection, snapshot['commit'], destination)
    try:
        require(not report['remaining_git_dependencies'], 'Registry projection still contains Git dependencies')
        stage = destination / 'source'
        require_outside_git(stage)
        payload = generate_registry_lock(stage, dependencies)
        gate.validate_registry_lock(payload, snapshot['blobs']['Cargo.lock'],
                                    snapshot['manifest']['package'], projection, allowlist)
        receipt = projection_receipt(snapshot, dependencies, report, stage)
        (destination / 'zmcpio-registry-stage.json').write_text(json.dumps(receipt, indent=2) + '\n', encoding='utf-8')
        stage_receipt(stage)
        require(source_snapshot(snapshot['commit']) == snapshot, 'Source changed during registry staging')
        return receipt
    except BaseException:
        # Only the directory just created by the official projection is owned.
        shutil.rmtree(destination)
        raise


def stage_receipt(stage):
    supplied = pathlib.Path(stage)
    require(not supplied.is_symlink(), 'Registry source must not be a symlink')
    stage = supplied.resolve()
    require(stage.name == 'source' and stage.is_dir(), 'This is not an owned registry stage')
    require(stage != ROOT.resolve() and ROOT.resolve() not in stage.parents,
            'Registry staging must be outside the development checkout')
    require_outside_git(stage)
    path = stage.parent / 'zmcpio-registry-stage.json'
    require(path.is_file() and not path.is_symlink(), 'This is not an owned registry stage')
    receipt = json.loads(path.read_text())
    snapshot = source_snapshot()
    require(receipt.get('source_commit') == snapshot['commit']
            and receipt.get('source_root') == str(ROOT.resolve()),
            'Stage does not bind the trusted original Git source')
    dependencies = dependency_registry(snapshot)
    require(receipt.get('dependencies') == dependencies,
            'Stage dependency mapping differs from the original Git lock or live registry')
    _, projection, gate = load_projection_tools()
    allowlist = projection.internal_repositories()
    lock_path = stage / 'Cargo.lock'
    require(lock_path.is_file() and not lock_path.is_symlink(), 'Registry lock must be a regular file')
    payload = lock_path.read_bytes()
    value = tomllib.loads((stage / 'Cargo.toml').read_text())
    validate_registry_dependencies(value, tomllib.loads(payload.decode()), dependencies)
    gate.validate_registry_lock(payload, snapshot['blobs']['Cargo.lock'],
                                snapshot['manifest']['package'], projection, allowlist)
    # Sidecars are evidence, not authority: reconstruct every staged source byte
    # from the trusted source and tool commits, even if a sidecar was rewritten.
    with tempfile.TemporaryDirectory(prefix='zmcpio-projection-check-') as scratch:
        fresh = pathlib.Path(scratch) / 'projection'
        report = project_source(projection, snapshot['commit'], fresh)
        require(not report['remaining_git_dependencies'], 'Registry projection still contains Git dependencies')
        report_path = stage.parent / 'projection.json'
        require(report_path.is_file() and not report_path.is_symlink()
                and json.loads(report_path.read_text()) == report, 'Projection report changed')
        compare_projection(stage, fresh / 'source')
        require(receipt == projection_receipt(snapshot, dependencies, report, stage),
                'Registry stage provenance or inputs changed')
    return receipt


def publication_paths(stage):
    output = subprocess.run(['cargo', 'package', '--list', '--locked'], cwd=stage,
                            check=True, text=True, capture_output=True).stdout
    # Cargo prints native paths, while tar member names always use POSIX separators.
    names = [pathlib.PurePath(name).as_posix() for name in output.splitlines()]
    require(names and len(names) == len(set(names)), 'Invalid Cargo publication file list')
    return set(names)


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
    required = {'Cargo.toml', 'Cargo.toml.orig', 'Cargo.lock'}
    require(required <= set(files), 'Package is missing registry provenance inputs')
    require('.cargo_vcs_info.json' not in files, 'Projected package must not contain Cargo VCS metadata')
    require(set(files) == publication_paths(stage),
            'Package files differ from the complete Cargo publication file list')
    normalized = tomllib.loads(files['Cargo.toml'].decode())
    locked = tomllib.loads(files['Cargo.lock'].decode())
    validate_lock(normalized, locked, 'registry')
    require(normalized['package']['name'] == name and normalized['package']['version'] == version,
            'Package identity differs from registry stage')
    registry_lock = (stage / 'Cargo.lock').read_bytes()
    require(files['Cargo.lock'] == registry_lock, 'Packaged lock changed')
    require(files['Cargo.toml.orig'] == (stage / 'Cargo.toml').read_bytes(), 'Original package manifest changed')
    for path, contents in files.items():
        if path not in ('Cargo.toml', 'Cargo.toml.orig', 'Cargo.lock'):
            require((stage / path).is_file() and (stage / path).read_bytes() == contents,
                    'Packaged source differs from registry stage: ' + path)
    # Compare an independent official projection and Cargo rebuild with the
    # candidate digest captured before rebuilding. Never rebuild over candidate.
    registry_environment()
    archive = pathlib.Path(archive).resolve()
    archive_sha256 = digest(archive)
    _, projection, gate = load_projection_tools()
    gate.archive_identity(archive, name, version)
    with tempfile.TemporaryDirectory(prefix='zmcpio-package-check-') as scratch:
        work = pathlib.Path(scratch)
        report = project_source(projection, receipt['source_commit'], work / 'projection')
        require(not report['remaining_git_dependencies'], 'Rebuild projection still contains Git dependencies')
        source = work / 'projection' / 'source'
        require_outside_git(source)
        (source / 'Cargo.lock').write_bytes(registry_lock)
        configured = os.environ.get('REGISTRY_REBUILD_TARGET')
        target = pathlib.Path(configured).resolve() if configured else work / 'build'
        rebuilt = target / 'package' / (name + '-' + version + '.crate')
        require(rebuilt.resolve() != archive, 'Rebuild target must not overwrite the candidate archive')
        gate.rebuild_package(source / 'Cargo.toml', target)
        require((source / 'Cargo.lock').read_bytes() == registry_lock, 'Packaging modified the verified registry lock')
        gate.archive_identity(rebuilt, name, version)
        require(digest(rebuilt) == archive_sha256, 'Official projection rebuild differs from the candidate archive')
    require(digest(archive) == archive_sha256, 'Candidate archive changed during verification')
    result = {'schema': 2, 'status': 'SOURCE_PROJECTION_REBUILT',
              'source_commit': receipt['source_commit'], 'projection_tool_sha': PROJECTION_TOOL_SHA,
              'registry_lock_sha256': receipt['registry_lock_sha256'], 'cargo_toolchain': REGISTRY_TOOLCHAIN,
              'package': name, 'version': version, 'archive': str(archive),
              'bytes': archive.stat().st_size, 'sha256': archive_sha256}
    # This build proof is not an independently trusted consumer receipt. A
    # consumer must establish its receipt pin independently after publication.
    (stage.parent / 'zmcpio-registry-package.json').write_text(json.dumps(result, indent=2) + '\n', encoding='utf-8')
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
