# Release

Development uses the canonical default branches of the internal Git repositories.
The committed Cargo.lock records the exact zcall and zconv commits and their package
versions. The development manifest has no registry version selector. Updating the
zmcpio release version changes only its own package version in Cargo.toml and
Cargo.lock; it does not resolve or advance either dependency.

## Validation and registry staging

The closure is exactly zmcpio -> zcall -> zconv. The default development audit
checks the committed manifest and lock against actual locked Cargo metadata.
It rejects registry fallback, a different Git URL or commit, extra packages, and
development/build/optional/target dependencies or enabled dependency features.

Public Cargo packages use a separate registry directory outside the source
checkout. `scripts/release.py stage PATH` requires a clean committed source,
reads its manifest and lock from HEAD, and selects each dependency by its unique
name, canonical Git URL and full commit. Registry metadata must match the package
name, repository, owner and locked version, be unyanked, have a valid checksum,
and preserve the same disabled-feature three-package closure. Missing or
incompatible published dependencies stop staging; they are not replaced by a
different version.

Only this directory receives exact registry selectors and registry checksums.
The original source, dependency lock and branch remain unchanged. The stage
contains `docs/registry-source.json`, which records the source commit/tree,
input hashes and the Git-to-registry mapping. A private receipt binds its clean
local staging commit and file hashes. Registry audit is explicit:
`python scripts/audit.py --mode registry`; it requires this receipt and checks
actual locked metadata again. Running the default audit on a registry checkout
fails.

CI and release jobs package from the stage using a separate Cargo target
directory. Package verification checks the clean staging commit, the original
manifest, the registry lock, the provenance document and all packaged source
bytes. After publication, the registry checksum must equal the verified archive
before the workflow creates the tag and GitHub Release.

## Starting a release

The workflow supports patch/minor/major increments and explicit legal SemVer.
Default dispatch validates only. Set `publish=true` on the trusted default
branch to request publication. The plan checks package-name ownership, that the
target version is unpublished, and the exact registry dependency closure.
Network or authentication failures stop the workflow.

To publish without manual dispatch, change `.github/release-request.txt` on
`main` to contain the intended SemVer version. Only this path triggers the
release workflow automatically. Ordinary source changes run CI without
publication. Both entry points retain the same preflight and validation gates.
Do not update the request for a version already published.

The old `zmcp` name belongs to another publisher. This project first published
as `zmcpio` 0.1.0 on 2026-11-11. Every release still checks live registry
ownership and version availability.

## Source and publication boundaries

Validation retains Linux, Windows and macOS on stable and MSRV, including
default no_std, all-features, doctests, Clippy, WASM and the independent stdio
and client smoke checks. These run on the development source. The registry
stage separately passes the explicit audit, package verification and publish
dry-run on every matrix entry.

The publication job repeats its source checks and registry checks. Before
pushing, it verifies that only the root package version changed from the workflow
commit, that the dependency lock and manifest contract are unchanged, and that
the remote default branch still equals that commit. It pushes the Git-only
source with a normal fast-forward push. The registry staging commit is never
pushed to the development branch. Cargo publish runs only in the verified stage.

Only the publish step receives CARGO_REGISTRY_TOKEN. No cargo login, token
argument, token file or token printing is used. PRs and forks cannot enter the
publish job. A successful publication and matching archive checksum are followed
by a tag on the Git-only source commit and a GitHub Release in the same workflow.
Release concurrency never cancels an active publication.

If publication succeeds but a later operation fails, recover the tag/release for
that exact immutable version. Do not republish it or blindly repeat the version
bump. If publication fails, no tag or GitHub Release is created; the verified
Git-only version commit may already be on main.
