# Release runbook

Checklist for signed + notarized releases and the Homebrew cask. The actual
signing work is a roadmap item (#1/#9) — this is the procedure to follow when
doing it.

## Prereqs

- [ ] Apple **Developer ID Application** certificate in the login keychain
- [ ] App Store Connect API key (or app-specific password) for notarization
- [ ] GitHub token with `repo` scope (`GH_TOKEN`)
- [ ] A `zwaneldmz/homebrew-tap` repo exists

## One-time setup

- [ ] Set `build.mac.identity` in `package.json` to the Developer ID cert
      name (or use the `CSC_NAME` env var instead of committing it)
- [ ] Add `@electron/notarize` and configure `build.afterSign` with
      credentials via env (`appleId`/`appleIdPassword`, or `teamId` +
      `appleApiKey`/`appleApiIssuer`)
- [ ] `license` / `repository` / `homepage` / `bugs` in `package.json` —
      **done** (this slice)
- [ ] Create the `zwaneldmz/homebrew-tap` repo

## Per release

- [ ] Bump `version` in `package.json`
- [ ] `npm test && npm run build` — both green
- [ ] Build + publish:
      `GH_TOKEN=… npx electron-builder --mac --universal --publish onGitHub`
      (publishes to the `zwaneldmz/tome` mirror)
- [ ] Verify notarization staple: `stapler validate dist/mac-universal/Tome.app`
- [ ] Tag the release commit; write GitHub Release notes
- [ ] Generate the cask against the release artifact:
      `brew create-cask <url-to-release-dmg-or-zip>` (edit name, version, sha256)
- [ ] PR the cask to `zwaneldmz/homebrew-tap`; merge
- [ ] Smoke test: `brew install --cask zwaneldmz/tap/tome`, launch, confirm
      Gatekeeper opens it without the quarantine workaround

## CI artifacts (roadmap #9 — later)

Extend `.github/workflows/build.yml` with a `package` job that runs on tags
(`macos-latest`) and performs the build/notarize/publish steps above. Secrets
to create on the mirror repo first:

- `CSC_LINK` — base64-encoded Developer ID Application `.p12`
- `CSC_KEY_PASSWORD` — password for that `.p12`
- `APPLE_API_KEY` — base64-encoded App Store Connect API `.p8`
- `APPLE_API_KEY_ID` — the key's ID
- `APPLE_API_ISSUER` — the issuer UUID
- `GH_TOKEN` is provided by Actions (`GITHUB_TOKEN`) if publishing from the
  same repo; a PAT only if publishing across repos
