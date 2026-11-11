# Release

The workflow supports patch/minor/major increments and explicit legal SemVer.
Default dispatch validates only. Set publish=true to request registry publication
from the trusted default branch. The plan checks package-name ownership and that
the target version does not already exist. All declared dependencies must be
published, exact and not yanked. Network/authentication failures stop the workflow.

The 2026-11-11 public registry lookup found zmcp 0.0.1 owned by `evolutionengine`.
It is not owned by the currently connected LIghtJUNction account. Publishing this
repository as zmcp is therefore blocked. Keep the repository name; select a new
verified registry package name or obtain ownership before a first publication.
Do not install the existing 0.0.1 expecting this code.

Release validation runs on Linux, Windows and macOS with stable and MSRV. Each
job updates Cargo.toml/Cargo.lock and checks the actual candidate. Publication
repeats local checks, packages and dry-runs, then pushes the verified version
without a force push. A concurrent branch change stops before cargo publish.

Only the publish step receives CARGO_REGISTRY_TOKEN. No cargo login, token command
argument, token file or token printing is used. PRs and forks cannot enter the
publish job. A successful publication is followed by a tag and GitHub Release in
the same workflow; it does not rely on a tag created with GITHUB_TOKEN triggering
another workflow. Release concurrency never cancels an active publication.

If registry publication succeeds but a later GitHub operation fails, recover the
tag/release for the already-published exact version; do not attempt to replace
that immutable crate or repeat a version bump blindly. If publication fails, no
tag or GitHub Release is created; the validated version commit may already exist.
