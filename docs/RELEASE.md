# Release Guide

## Version Policy

BaoClaw uses one monorepo version. The root `package.json` is the source of
truth, and every workspace package manifest uses the same version. The root
`package-lock.json` is the only Node lockfile (single npm workspace root);
per-package lockfiles no longer exist.

Use Semantic Versioning:

- Major: breaking core-to-gateway protocol changes
- Minor: backwards-compatible features
- Patch: backwards-compatible fixes

## Release Flow

1. Update the version in the root and all workspace manifests with `npm version
X.Y.Z --no-git-tag-version --workspaces`.
2. Update the matching `CHANGELOG.md` section and verify the root
   `package-lock.json` agrees.
3. Run `npm run verify-all` and the gateway test commands.
4. Commit the version and changelog changes.
5. Create an annotated tag: `git tag -a vX.Y.Z -m "BaoClaw vX.Y.Z"`.
6. Push the commit and tag together: `git push origin HEAD vX.Y.Z`.

Do not create a tag before the release commit is reviewed. Existing release
tags are immutable; publish a new patch version when a release needs correction.

## Supported Platforms and Recovery

- Linux and macOS are supported for the Rust daemon and Unix-socket IPC.
- Windows installation scripts exist, but daemon IPC currently requires a Unix
  transport; Windows is not a supported daemon platform until named-pipe IPC is
  implemented.
- Before upgrading, preserve `~/.baoclaw/config.json`, `cron.json`, session
  files, and the current commit hash. The daemon migrates compatible state on
  startup; malformed files are reported and are not overwritten automatically.
- To roll back, stop the daemon, restore the previous binary and configuration
  backup, then restart. Keep the newer state backup until the old version has
  resumed successfully.
- For support diagnostics, collect `baoclaw --version`, daemon status output,
  the platform-context artifact, and redacted logs. Do not attach prompts,
  credentials, session files, or raw provider responses.

## Dry Run

To rehearse the flow, use a temporary branch and a patch version:

```bash
git switch -c release-dry-run
npm version 2.1.1 --no-git-tag-version
git diff -- package.json */package.json package-lock.json
git switch -
git branch -D release-dry-run
```
