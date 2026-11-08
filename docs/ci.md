# CI and releases

The repository's `.github/workflows/ci.yml` calls Daybrite's shared `daylib` workflow.
A branch push or pull request runs against Day's main branch. A version tag or ordinary manual
run uses published Day dependencies; enable **day-main** for a manual run against main.

## What runs

| Job | Coverage |
| --- | --- |
| Host | Formatting, Clippy with warnings denied, and Cargo tests on Linux, macOS, and Windows. |
| Cross-check | Library Clippy for iOS, Android, HarmonyOS, and web. |
| Demo | Build, package, and the portable `dayscript/demo.yaml` walkthrough for the demo's declared targets. |
| Publish | On a version tag, publish the crate after host and cross-check jobs pass. Demo release checks run separately. |

Cargo tests include `tests/interception.rs`: scripted requests and replies, notifications,
disconnects, concurrent sessions, cancellation, and strict rejection of unexpected calls.
They do not need Bluetooth hardware or OS permission.

The portable walkthrough checks the demo's controls. The separate software-peripheral script
needs physical radios and is excluded from hosted CI. A passing hosted run does not verify native
radio behavior. Follow the [hardware test instructions](bluetooth.md#demo-and-software-peripheral)
when changing platform code.

## First release setup

1. Create the `daybrite/day-part-bluetooth` repository and enable GitHub Actions.
2. Make Day's external bridge source loading available on Day's main branch before running this
   package's branch CI. Release that support in both `day-build` and `day-cli` before tagging
   this package: Apple and Android implementations use external Swift and Java source files.
3. Register `day-part-bluetooth` on crates.io. Configure its GitHub trusted publisher for
   repository `daybrite/day-part-bluetooth`, caller workflow `ci.yml`, and environment `release`.
4. Configure the demo signing credentials required by the shared workflow, and GitHub Pages
   with GitHub Actions as its deployment source for the generated project site.
5. Run CI with published dependencies before creating a release tag. The tag must match the
   package version, for example `vX.Y.Z`.
6. After publication, replace the source-only installation notes in the README and website
   guide with `cargo add 'day-part-bluetooth@0.*'`. Add the verified crate and Rustdoc links to
   the website's external-crate catalog.

The demo is a private application package and is not published to crates.io. Its path dependency
uses this repository's Bluetooth code. Local `.cargo/config.toml` patches are ignored and are not
part of CI; do not rely on them to supply unpublished framework changes.

See the [shared workflow](https://github.com/daybrite/actions/blob/v1/.github/workflows/daylib.yml)
for runner, signing, and release settings.
