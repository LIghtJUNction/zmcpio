"""Dependency-free version planning and fail-closed public registry preflight."""
import json
import os
import pathlib
import re
import subprocess
import sys
import tomllib
import urllib.error
import urllib.request

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
    request = urllib.request.Request('https://crates.io/api/v1/' + path, headers={'User-Agent': 'zmcp-release-preflight/0.1 (https://github.com/LIghtJUNction/zmcp)'})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        if allow_missing and error.code == 404:
            return None
        raise

def manifest():
    return tomllib.loads((ROOT / 'Cargo.toml').read_text())

def registry_preflight(package, version):
    name = package['name']
    repository = os.environ.get('GITHUB_REPOSITORY', '')
    if package['repository'].rstrip('/') != 'https://github.com/' + repository:
        raise ValueError('Manifest repository does not match trusted workflow repository')
    owner = repository.split('/')[0].casefold()
    exists = get_json('crates/' + name, allow_missing=True)
    if exists:
        owners = get_json('crates/' + name + '/owners')
        logins = [u.get('login', '') for u in owners.get('users', [])]
        print('Registered crate owners:', ', '.join(logins))
        if owner not in {login.casefold() for login in logins}:
            raise ValueError('Package-name collision: current repository owner is not a crate owner')
    if get_json('crates/' + name + '/' + version, allow_missing=True):
        raise ValueError('This crate version already exists')
    for dep, settings in manifest().get('dependencies', {}).items():
        if not isinstance(settings, dict) or 'path' in settings or 'git' in settings:
            raise ValueError('Release dependencies must be exact registry versions')
        requirement = settings.get('version', '')
        if not requirement.startswith('='):
            raise ValueError('Release dependencies must be exact registry versions')
        release = get_json('crates/' + dep + '/' + requirement[1:])
        if release['version'].get('yanked'):
            raise ValueError('Release dependency is yanked')

def apply_version(version):
    path = ROOT / 'Cargo.toml'
    old = manifest()['package']['version']
    next_version(old, manual=version)
    text = path.read_text()
    needle = 'version = "' + old + '"'
    if needle not in text:
        raise ValueError('Cannot identify package version declaration')
    path.write_text(text.replace(needle, 'version = "' + version + '"', 1))
    subprocess.run(['cargo', 'generate-lockfile'], cwd=ROOT, check=True)
    if manifest()['package']['version'] != version:
        raise ValueError('Manifest version update failed')

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
    else:
        raise ValueError('Unknown release command')

if __name__ == '__main__':
    main()
