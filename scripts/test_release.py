import unittest
from release import next_version, parse_version, precedence

class VersionTests(unittest.TestCase):
    def test_bumps(self):
        self.assertEqual(next_version('1.2.3'), '1.2.4')
        self.assertEqual(next_version('1.2.3', 'minor'), '1.3.0')
        self.assertEqual(next_version('1.2.3', 'major'), '2.0.0')
    def test_manual(self):
        self.assertEqual(next_version('0.1.0', manual='0.1.0'), '0.1.0')
        self.assertEqual(next_version('0.1.0', manual='0.2.0-rc.1+build.2'), '0.2.0-rc.1+build.2')
        with self.assertRaises(ValueError): next_version('1.0.0', manual='0.9.9')
    def test_precedence(self):
        versions = ['1.0.0-alpha', '1.0.0-alpha.1', '1.0.0-alpha.beta', '1.0.0-beta', '1.0.0-beta.2', '1.0.0-beta.11', '1.0.0-rc.1', '1.0.0']
        self.assertEqual(sorted(versions, key=precedence), versions)
    def test_rejection(self):
        for version in ['', '1.0', '01.0.0', '1.0.0-01', '1.0.0;', '1.0.0\n', '1.0.0+bad space', '18446744073709551616.0.0']:
            with self.subTest(version=version), self.assertRaises(ValueError): parse_version(version)
        with self.assertRaises(ValueError): next_version('1.0.0', 'arbitrary')
        with self.assertRaises(ValueError): next_version('0.0.18446744073709551615')

import contextlib
import copy
import importlib.util
import io
import json
import os
import pathlib
import re
import runpy
import subprocess
import tempfile
import tomllib
from unittest import mock
import release

SCRIPTS = pathlib.Path(__file__).resolve().parent
REPOSITORY = SCRIPTS.parent
GIT = {
    'zcall': 'https://github.com/LIghtJUNction/zcall.git',
    'zconv': 'https://github.com/LIghtJUNction/zconv.git',
}
REGISTRY = 'registry+https://github.com/rust-lang/crates.io-index'

class PublicationPathTests(unittest.TestCase):
    def test_native_paths_match_posix_archive_names(self):
        expected = {'Cargo.toml', 'Cargo.toml.orig', 'Cargo.lock',
                    '.cargo_vcs_info.json', 'src/lib.rs', 'tests/transport/stdio.rs'}
        for path_type, separator, newline in (
            (pathlib.PurePosixPath, '/', '\n'),
            (pathlib.PureWindowsPath, '\\', '\r\n'),
        ):
            output = newline.join(name.replace('/', separator) for name in sorted(expected)) + newline
            with self.subTest(platform=path_type.__name__), \
                 mock.patch.object(release, 'pathlib', mock.Mock(wraps=pathlib, PurePath=path_type)), \
                 mock.patch.object(release.subprocess, 'run',
                                   return_value=subprocess.CompletedProcess([], 0, stdout=output)) as run:
                self.assertEqual(release.publication_paths('stage'), expected)
                run.assert_called_once_with(['cargo', 'package', '--list', '--locked'],
                                            cwd='stage', check=True, text=True, capture_output=True)

    def test_empty_or_duplicate_canonical_paths_are_rejected(self):
        for output in ('', 'src\\lib.rs\r\nsrc/lib.rs\r\n'):
            with self.subTest(output=output), \
                 mock.patch.object(release, 'pathlib',
                                   mock.Mock(wraps=pathlib, PurePath=pathlib.PureWindowsPath)), \
                 mock.patch.object(release.subprocess, 'run',
                                   return_value=subprocess.CompletedProcess([], 0, stdout=output)):
                with self.assertRaisesRegex(ValueError, 'Invalid Cargo publication file list'):
                    release.publication_paths('stage')

class GitOnlyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = pathlib.Path(self.temp.name) / 'source'
        self.root.mkdir()
        for name in ('Cargo.toml', 'Cargo.lock'):
            # Fixture identities do not follow a later release version bump.
            text = (REPOSITORY / name).read_text()
            text = re.sub(r'(?m)^version = "[^\"]+"$', 'version = "0.4.0"', text)
            (self.root / name).write_text(text)
        (self.root / 'src').mkdir()
        (self.root / 'src/lib.rs').write_text('// private source fixture\n')
        (self.root / 'scripts').mkdir()
        for name in ('audit.py', 'release.py'):
            (self.root / 'scripts' / name).write_bytes((SCRIPTS / name).read_bytes())
        self.git('init', '-q')
        self.git('config', 'user.name', 'Fixture')
        self.git('config', 'user.email', 'fixture@example.invalid')
        self.git('config', 'core.autocrlf', 'false')
        self.git('add', '.')
        self.git('commit', '-q', '-m', 'fixture source')
        self.head = self.git('rev-parse', 'HEAD').strip()
        self.patch_root = mock.patch.object(release, 'ROOT', self.root)
        self.patch_root.start()
        self.addCleanup(self.patch_root.stop)
        paths = {'Cargo.toml', 'Cargo.toml.orig', 'Cargo.lock', '.cargo_vcs_info.json', 'src/lib.rs'}
        listing = mock.patch.object(release, 'publication_paths', return_value=paths)
        listing.start()
        self.addCleanup(listing.stop)

    def git(self, *args):
        return subprocess.run(['git', '-c', 'commit.gpgsign=false', '-C', str(self.root), *args],
                              check=True, text=True, capture_output=True).stdout

    def manifest(self):
        return tomllib.loads((self.root / 'Cargo.toml').read_text())

    def lock(self):
        return tomllib.loads((self.root / 'Cargo.lock').read_text())

    def metadata(self, mode='development'):
        lock = self.lock()
        packages = []
        for item in lock['package']:
            name = item['name']
            deps = {'zmcpio': ['zcall'], 'zcall': ['zconv'], 'zconv': []}[name]
            dependencies = [{
                'name': dep, 'source': 'git+' + GIT[dep] if mode == 'development' else REGISTRY,
                'req': '*' if mode == 'development' and name == 'zmcpio' else '=0.4.0',
                'kind': None, 'rename': None, 'optional': False, 'uses_default_features': False,
                'features': [], 'target': None, 'registry': None,
            } for dep in deps]
            packages.append({'name': name, 'version': item['version'], 'source': item.get('source'),
                             'manifest_path': str(self.root / 'Cargo.toml') if name == 'zmcpio'
                                              else str(pathlib.Path(self.temp.name) / name / 'Cargo.toml'),
                             'id': name, 'dependencies': dependencies})
        return {'packages': packages, 'resolve': {'nodes': [
            {'id': p['id'], 'dependencies': [d['name'] for d in p['dependencies']],
             'features': []} for p in packages]}}

    def registry(self, path, allow_missing=False):
        if path == 'crates/zmcpio':
            return {'crate': {'id': 'zmcpio', 'repository': 'https://github.com/LIghtJUNction/zmcpio'}}
        if path == 'crates/zmcpio/owners':
            return {'users': [{'login': 'LIghtJUNction'}]}
        if path.startswith('crates/zmcpio/'):
            return None
        for name in ('zcall', 'zconv'):
            if path == 'crates/' + name:
                return {'crate': {'id': name, 'repository': GIT[name].removesuffix('.git')}}
            if path == 'crates/' + name + '/owners':
                return {'users': [{'login': 'LIghtJUNction'}]}
            if path == 'crates/' + name + '/0.4.0':
                return {'version': {'crate': name, 'num': '0.4.0', 'yanked': False,
                                    'checksum': ('a' if name == 'zcall' else 'b') * 64}}
            if path == 'crates/' + name + '/0.4.0/dependencies':
                return {'dependencies': [] if name == 'zconv' else [{
                    'crate_id': 'zconv', 'req': '=0.4.0', 'kind': 'normal',
                    'optional': False, 'default_features': False, 'features': [],
                    'target': None, 'registry': None,
                }]}
        self.fail('Unexpected registry fixture path: ' + path)

    def test_dev_audit_matches_committed_git_lock(self):
        called = []
        original = subprocess.check_output
        def metadata_only(argv, *args, **kwargs):
            if argv[0] == 'cargo':
                self.assertEqual(argv, ['cargo', 'metadata', '--locked', '--format-version', '1'])
                called.append(argv)
                return json.dumps(self.metadata()).encode()
            return original(argv, *args, **kwargs)
        with mock.patch('subprocess.check_output', side_effect=metadata_only), \
             mock.patch.object(__import__('sys'), 'argv', ['audit.py']), \
             contextlib.redirect_stdout(io.StringIO()):
            runpy.run_path(str(self.root / 'scripts/audit.py'), run_name='__main__')
        self.assertEqual(len(called), 1)

    def test_publish_plan_resolves_committed_git_versions(self):
        before = {name: (self.root / name).read_bytes() for name in ('Cargo.toml', 'Cargo.lock')}
        with mock.patch.dict(os.environ, {'GITHUB_REPOSITORY': 'LIghtJUNction/zmcpio'}), \
             mock.patch.object(release, 'get_json', side_effect=self.registry):
            release.registry_preflight(self.manifest()['package'], '0.4.1')
        self.assertEqual(before, {name: (self.root / name).read_bytes() for name in before})

    def test_apply_bumps_only_root_without_cargo(self):
        old_lock = copy.deepcopy(self.lock())
        original = subprocess.run
        calls = []
        def git_only(argv, *args, **kwargs):
            calls.append(argv)
            self.assertEqual(argv[0], 'git', 'version planning must not resolve new Cargo sources')
            return original(argv, *args, **kwargs)
        with mock.patch('subprocess.run', side_effect=git_only):
            release.apply_version('0.4.1')
        updated = self.lock()
        for package in old_lock['package']:
            if package['name'] == 'zmcpio':
                package['version'] = '0.4.1'
        self.assertEqual(updated, old_lock)
        self.assertEqual(self.manifest()['package']['version'], '0.4.1')
        self.assertNotIn('version', self.manifest()['dependencies']['zcall'])
        self.assertEqual(set(self.git('diff', '--name-only').splitlines()), {'Cargo.toml', 'Cargo.lock'})


    def audit_module(self):
        spec = importlib.util.spec_from_file_location('fixture_audit', self.root / 'scripts/audit.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def test_development_never_falls_back_to_registry(self):
        audit = self.audit_module()
        for update in ({'version': '=0.4.0'}, {'version': '*'},
                       {'branch': 'main'}, {'tag': 'v0.4.0'}, {'rev': 'a' * 40},
                       {'path': '../zcall'}, {'optional': True}, {'features': ['std']}):
            value = copy.deepcopy(self.manifest())
            value['dependencies']['zcall'].update(update)
            with self.subTest(update=update), self.assertRaises(ValueError):
                audit.validate_manifest(value)
        for section in ('dev-dependencies', 'build-dependencies', 'target'):
            value = copy.deepcopy(self.manifest())
            value[section] = {'surprise': '1'}
            with self.subTest(section=section), self.assertRaises(ValueError):
                audit.validate_manifest(value)

    def test_lock_identity_missing_ambiguity_and_source_mismatch(self):
        audit = self.audit_module()
        valid = self.lock()
        audit.validate_lock(self.manifest(), valid)
        mutations = []
        missing = copy.deepcopy(valid)
        missing['package'] = [p for p in missing['package'] if p['name'] != 'zconv']
        mutations.append(missing)
        duplicate = copy.deepcopy(valid)
        duplicate['package'].append(copy.deepcopy(duplicate['package'][0]))
        mutations.append(duplicate)
        for source in (REGISTRY, 'git+' + GIT['zcall'] + '?branch=main#' + 'a'*40,
                       'git+https://github.com/another/zcall.git#' + 'a'*40,
                       'git+' + GIT['zcall'] + '#short'):
            bad = copy.deepcopy(valid)
            next(p for p in bad['package'] if p['name'] == 'zcall')['source'] = source
            mutations.append(bad)
        for bad in mutations:
            with self.subTest(lock=bad), self.assertRaises(ValueError):
                audit.validate_lock(self.manifest(), bad)

    def test_metadata_must_match_lock_and_dependency_contract(self):
        audit = self.audit_module()
        original = self.metadata()
        # Cargo activates the root's named default feature even when default = [].
        next(n for n in original['resolve']['nodes'] if n['id'] == 'zmcpio')['features'] = ['default']
        audit.validate_metadata(original, self.manifest(), self.lock(), self.root)
        extra = copy.deepcopy(original)
        next(n for n in extra['resolve']['nodes'] if n['id'] == 'zcall')['features'] = ['default']
        with self.assertRaises(ValueError):
            audit.validate_metadata(extra, self.manifest(), self.lock(), self.root)
        for name, key, value in (
            ('zcall', 'version', '0.5.0'),
            ('zcall', 'source', 'git+' + GIT['zcall'] + '#' + 'c' * 40),
            ('zmcpio', 'source', REGISTRY),
        ):
            altered = copy.deepcopy(original)
            next(p for p in altered['packages'] if p['name'] == name)[key] = value
            with self.subTest(name=name, key=key), self.assertRaises(ValueError):
                audit.validate_metadata(altered, self.manifest(), self.lock(), self.root)
        for field, value in (('kind', 'build'), ('optional', True),
                             ('uses_default_features', True), ('features', ['std']),
                             ('target', 'cfg(unix)')):
            altered = copy.deepcopy(original)
            altered['packages'][0]['dependencies'][0][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                audit.validate_metadata(altered, self.manifest(), self.lock(), self.root)

    def test_stage_is_registry_only_and_leaves_source_untouched(self):
        before = {name: (self.root / name).read_bytes() for name in ('Cargo.toml', 'Cargo.lock', 'src/lib.rs')}
        dest = pathlib.Path(self.temp.name) / 'registry'
        with mock.patch.object(release, 'get_json', side_effect=self.registry):
            receipt = release.stage_registry(dest, expected_source=self.head)
        self.assertEqual(self.git('rev-parse', 'HEAD').strip(), self.head)
        self.assertEqual(self.git('status', '--porcelain'), '')
        self.assertEqual(before, {name: (self.root / name).read_bytes() for name in before})
        stage_manifest = tomllib.loads((dest / 'Cargo.toml').read_text())
        expected = copy.deepcopy(self.manifest())
        expected['dependencies']['zcall'].pop('git')
        expected['dependencies']['zcall']['version'] = '=0.4.0'
        self.assertEqual(stage_manifest, expected)
        lock = tomllib.loads((dest / 'Cargo.lock').read_text())
        for p in lock['package']:
            if p['name'] != 'zmcpio':
                self.assertEqual(p['source'], REGISTRY)
                self.assertEqual(p['checksum'], ('a' if p['name'] == 'zcall' else 'b')*64)
        self.assertEqual(receipt['source_commit'], self.head)
        self.assertEqual(release.stage_receipt(dest)['stage_commit'], receipt['stage_commit'])
        self.assertEqual(receipt['dependencies']['zcall']['git_source'],
                         next(p for p in self.lock()['package'] if p['name'] == 'zcall')['source'])
        self.assertFalse((dest / 'docs/registry-source.json').exists())
        self.assertEqual(set(release.git(dest, 'diff', '--name-only', self.head,
                                        receipt['stage_commit']).splitlines()),
                         {'Cargo.toml', 'Cargo.lock'})

    def test_stage_rejects_inside_source_existing_destination_and_drift(self):
        with mock.patch.object(release, 'get_json', side_effect=self.registry):
            for dest in (self.root / 'nested', self.root):
                with self.subTest(dest=dest), self.assertRaises(ValueError):
                    release.stage_registry(dest)
            dest = pathlib.Path(self.temp.name) / 'occupied'
            dest.mkdir()
            (dest / 'owned-by-someone-else').write_text('keep')
            with self.assertRaises(ValueError):
                release.stage_registry(dest)
            self.assertEqual((dest / 'owned-by-someone-else').read_text(), 'keep')
            with self.assertRaises(ValueError):
                release.stage_registry(pathlib.Path(self.temp.name) / 'wrong-head',
                                       expected_source='a'*40)
            (self.root / 'src/lib.rs').write_text('// modified\n')
            with self.assertRaises(ValueError):
                release.stage_registry(pathlib.Path(self.temp.name) / 'dirty')

    def test_registry_identity_yank_checksum_and_closure_rejected(self):
        cases = [
            ('crates/zcall/0.4.0', lambda x: x['version'].update(num='0.4.1')),
            ('crates/zcall/0.4.0', lambda x: x['version'].update(crate='different')),
            ('crates/zcall/0.4.0', lambda x: x['version'].update(yanked=True)),
            ('crates/zcall/0.4.0', lambda x: x['version'].update(checksum='invalid')),
            ('crates/zcall/owners', lambda x: x.update(users=[{'login': 'different'}])),
            ('crates/zcall/0.4.0/dependencies',
             lambda x: x['dependencies'][0].update(req='=0.5.0')),
            ('crates/zcall/0.4.0/dependencies',
             lambda x: x['dependencies'][0].update(default_features=True)),
            ('crates/zconv/0.4.0/dependencies',
             lambda x: x.update(dependencies=[{'crate_id': 'unexpected'}])),
        ]
        for i, (path, change) in enumerate(cases):
            def altered(request, allow_missing=False):
                result = copy.deepcopy(self.registry(request, allow_missing))
                if request == path:
                    change(result)
                return result
            dest = pathlib.Path(self.temp.name) / ('reject-' + str(i))
            with self.subTest(path=path), mock.patch.object(release, 'get_json', side_effect=altered):
                with self.assertRaises(ValueError):
                    release.stage_registry(dest)
            self.assertFalse(dest.exists())

    def test_stage_tampering_and_source_change_fail_closed(self):
        dest = pathlib.Path(self.temp.name) / 'registry'
        with mock.patch.object(release, 'get_json', side_effect=self.registry):
            release.stage_registry(dest)
        (dest / 'Cargo.lock').write_text((dest / 'Cargo.lock').read_text().replace('a'*64, 'c'*64))
        with self.assertRaises(ValueError):
            release.stage_receipt(dest)
        self.assertEqual(self.git('status', '--porcelain'), '')

    def test_concurrent_main_or_registry_source_cannot_be_pushed(self):
        dest = pathlib.Path(self.temp.name) / 'registry'
        with mock.patch.object(release, 'get_json', side_effect=self.registry):
            release.stage_registry(dest)
        original = release.git
        called = []
        def concurrent(root, *args):
            called.append(args)
            if args[0] == 'fetch':
                return ''
            if args == ('rev-parse', 'FETCH_HEAD'):
                return 'f'*40 + '\n'
            if args[0] == 'push':
                self.fail('concurrent source must stop before push')
            return original(root, *args)
        with mock.patch.object(release, 'git', side_effect=concurrent), self.assertRaises(ValueError):
            release.push_source(dest, self.head, 'main')
        self.assertTrue(any(args[0] == 'fetch' for args in called))
        with mock.patch.object(release, 'ROOT', dest), self.assertRaises(ValueError):
            release.push_source(dest, self.head, 'main')


    def make_archive(self, stage, changes=None, exclude=()):
        import tarfile
        value = tomllib.loads((stage / 'Cargo.toml').read_text())
        receipt = release.stage_receipt(stage)
        content = {name: (stage / name).read_bytes() for name in (
            'Cargo.toml', 'Cargo.lock', 'src/lib.rs')}
        content['Cargo.toml.orig'] = content['Cargo.toml']
        content['.cargo_vcs_info.json'] = json.dumps({
            'git': {'sha1': receipt['stage_commit'], 'dirty': False}, 'path_in_vcs': ''}).encode()
        content.update(changes or {})
        for name in exclude:
            content.pop(name)
        archive = pathlib.Path(self.temp.name) / 'fixture.crate'
        prefix = value['package']['name'] + '-' + value['package']['version'] + '/'
        with tarfile.open(archive, 'w:gz') as target:
            for name, data in content.items():
                info = tarfile.TarInfo(prefix + name)
                info.size = len(data)
                target.addfile(info, io.BytesIO(data))
        return archive

    def test_package_provenance_and_published_checksum(self):
        stage = pathlib.Path(self.temp.name) / 'registry'
        with mock.patch.object(release, 'get_json', side_effect=self.registry):
            release.stage_registry(stage)
        archive = self.make_archive(stage)
        checked = release.verify_package(stage, archive)
        self.assertEqual(checked['sha256'], release.digest(archive))
        def published(path, allow_missing=False):
            if path == 'crates/zmcpio/0.4.0':
                return {'version': {'crate': 'zmcpio', 'num': '0.4.0', 'yanked': False,
                                    'checksum': checked['sha256']}}
            return self.registry(path, allow_missing)
        with mock.patch.object(release, 'get_json', side_effect=published):
            self.assertEqual(release.verify_published(stage, archive)['sha256'], checked['sha256'])
        for field, bad in (('checksum', '0'*64), ('crate', 'different'),
                           ('num', '0.4.1'), ('yanked', True)):
            def altered(path, allow_missing=False):
                result = copy.deepcopy(published(path, allow_missing))
                if path == 'crates/zmcpio/0.4.0':
                    result['version'][field] = bad
                return result
            with self.subTest(field=field), mock.patch.object(release, 'get_json', side_effect=altered):
                with self.assertRaises(ValueError):
                    release.verify_published(stage, archive)

    def test_packaged_sources_and_registry_inputs_cannot_drift(self):
        stage = pathlib.Path(self.temp.name) / 'registry'
        with mock.patch.object(release, 'get_json', side_effect=self.registry):
            release.stage_registry(stage)
        changes = [
            {'src/lib.rs': b'// changed source\\n'},
            {'Cargo.toml.orig': b'[package]\\nname="different"\\n'},
            {'docs/registry-source.json': b'{}\\n'},
            {'.cargo_vcs_info.json': json.dumps({'git': {'sha1': self.head, 'dirty': False}}).encode()},
            {'Cargo.lock': (stage / 'Cargo.lock').read_bytes().replace(b'a'*64, b'c'*64)},
        ]
        for change in changes:
            with self.subTest(change=list(change)), self.assertRaises(ValueError):
                release.verify_package(stage, self.make_archive(stage, change))
        self.assertEqual(self.git('status', '--porcelain'), '')

    def test_archive_cannot_omit_an_included_source_file(self):
        stage = pathlib.Path(self.temp.name) / 'registry'
        with mock.patch.object(release, 'get_json', side_effect=self.registry):
            release.stage_registry(stage)
        with self.assertRaisesRegex(ValueError, 'complete Cargo publication file list'):
            release.verify_package(stage, self.make_archive(stage, exclude=('src/lib.rs',)))

    def test_legal_prerelease_bump_preserves_git_identities(self):
        old = self.lock()
        release.apply_version('0.4.1-rc.1+build.2')
        updated = self.lock()
        for package in old['package']:
            if package['name'] == 'zmcpio':
                package['version'] = '0.4.1-rc.1+build.2'
        self.assertEqual(updated, old)
        self.assertEqual(self.manifest()['package']['version'], '0.4.1-rc.1+build.2')


    def test_stage_mapping_is_checked_against_original_git_objects(self):
        stage = pathlib.Path(self.temp.name) / 'registry'
        with mock.patch.object(release, 'get_json', side_effect=self.registry):
            release.stage_registry(stage)
        receipt_path = stage / '.git/zmcpio-registry-stage.json'
        receipt = json.loads(receipt_path.read_text())
        receipt['dependencies']['zcall']['git_source'] = 'git+' + GIT['zcall'] + '#' + 'f'*40
        receipt_path.write_text(json.dumps(receipt, indent=2) + '\n')
        with self.assertRaisesRegex(ValueError, 'original Git lock'):
            release.stage_receipt(stage)

    def test_registry_stage_cannot_add_packaged_provenance_or_change_source(self):
        stage = pathlib.Path(self.temp.name) / 'registry'
        with mock.patch.object(release, 'get_json', side_effect=self.registry):
            release.stage_registry(stage)
        (stage / 'src/lib.rs').write_text('// changed after projection\n')
        release.git(stage, 'add', 'src/lib.rs')
        release.git(stage, '-c', 'commit.gpgsign=false', 'commit', '--amend', '--no-edit', '--quiet')
        receipt_path = stage / '.git/zmcpio-registry-stage.json'
        receipt = json.loads(receipt_path.read_text())
        receipt['stage_commit'] = release.git(stage, 'rev-parse', 'HEAD').strip()
        receipt['stage_tree'] = release.git(stage, 'rev-parse', 'HEAD^{tree}').strip()
        receipt_path.write_text(json.dumps(receipt, indent=2) + '\n')
        with self.assertRaisesRegex(ValueError, 'outside the manifest and lock'):
            release.stage_receipt(stage)


    def crlf_checkout(self):
        clone = pathlib.Path(self.temp.name) / 'crlf checkout'
        subprocess.run(['git', 'clone', '--quiet', '--no-hardlinks',
                        '--config', 'core.autocrlf=true', str(self.root), str(clone)],
                       check=True, capture_output=True)
        self.patch_root.stop()
        self.root = clone
        self.patch_root = mock.patch.object(release, 'ROOT', self.root)
        self.patch_root.start()
        self.addCleanup(self.patch_root.stop)
        self.assertEqual(self.git('status', '--porcelain'), '')
        before = {}
        for name in ('Cargo.toml', 'Cargo.lock', 'src/lib.rs'):
            before[name] = (self.root / name).read_bytes()
            self.assertIn(b'\r\n', before[name])
            committed = subprocess.check_output(['git', '-C', str(self.root), 'show', 'HEAD:' + name])
            self.assertNotIn(b'\r\n', committed)
        return before

    def test_development_audit_accepts_crlf_checkout_but_rejects_content_drift(self):
        self.crlf_checkout()
        self.test_dev_audit_matches_committed_git_lock()
        path = self.root / 'Cargo.toml'
        path.write_bytes(path.read_bytes() + b'\r\n# uncommitted content change\r\n')
        with self.assertRaisesRegex(ValueError, 'committed source'):
            self.test_dev_audit_matches_committed_git_lock()

    def test_registry_stage_preserves_crlf_source_and_committed_lock(self):
        import hashlib
        before = self.crlf_checkout()
        snapshot = release.source_snapshot(self.head)
        stage = pathlib.Path(self.temp.name) / 'registry stage'
        with mock.patch.object(release, 'get_json', side_effect=self.registry):
            receipt = release.stage_registry(stage, expected_source=self.head)
        self.assertEqual(before, {name: (self.root / name).read_bytes() for name in before})
        self.assertEqual(release.source_snapshot(self.head), snapshot)
        for name in ('Cargo.toml', 'Cargo.lock'):
            committed = subprocess.check_output(['git', '-C', str(self.root), 'show', 'HEAD:' + name])
            self.assertEqual(receipt['source_inputs'][name], hashlib.sha256(committed).hexdigest())
        self.assertEqual(release.stage_receipt(stage)['stage_commit'], receipt['stage_commit'])


if __name__ == '__main__':
    unittest.main()
