# Release

The workflow supports patch/minor/major increments and explicit legal SemVer.
Default dispatch validates only. Set publish=true to request registry publication
from the trusted default branch. The plan checks package-name ownership and that
the target version does not already exist. All declared dependencies must be
published, exact and not yanked. Network/authentication failures stop the workflow.

The old `zmcp` package name has a pre-existing release owned by another account.
The package and repository now consistently use `zmcpio`. Its absence from the
public crates.io index was checked on 2026-11-11, but only a successful registry
upload reserves the name; the workflow repeats a live collision check.

To publish the first release, use the **Release** workflow on `main` with
`version=0.1.0` and `publish=true`, because a default patch bump from the
unreleased manifest version would select `0.1.1`. To publish without a manual
workflow dispatch, add or update `.github/release-request.txt` on `main` with
the intended SemVer version as its only content. Only pushes changing that
specific file trigger the release workflow automatically; ordinary code pushes
run CI without publishing. Both paths perform the same registry preflight and
cross-platform validation before publishing. Do not modify the request file
again for a version already on crates.io.

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
