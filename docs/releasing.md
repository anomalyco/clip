# Releasing Clip

One run of the **Release** workflow (`.github/workflows/release.yml`) builds,
signs, notarizes and publishes Clip to the Anomaly release service
(`anomalyco/release`, `https://anoma.ly/release`, registered in
`registry/clip.json`), the way Hex releases. Clip's in-app updater
(`src/updater.rs`) reads what it publishes.

## Names

```
https://anoma.ly/release/clip/
├─ index.json                    download page
└─ desktop/<channel>.json        what the app polls; channels stable, beta, dev
   └─ release.targets
      ├─ macos-arm64      Clip.zip   ditto zip, root Clip.app (updater)
      └─ macos-arm64-dmg  Clip.dmg   notarized DMG (download page)
```

The latest stable DMG is always at
`https://anoma.ly/release/clip/desktop/stable/macos-arm64-dmg`.

## Channels

| Channel | How | Version |
|---|---|---|
| dev | every push to `dev` | `0.0.0-dev-<run number>` |
| check | dispatch from `dev` | builds, signs and verifies; publishes nothing |
| stable | dispatch from `production` | `Cargo.toml`'s version; needs `docs/releases/<version>.md` |

A stable release:

1. On `dev`, raise `version` in `Cargo.toml` and add `docs/releases/<version>.md`.
2. `git push origin dev:production`
3. `gh workflow run release.yml -R anomalyco/clip --ref production -f ref=production -f channel=stable`

After publishing, the workflow tags `v<version>` and creates the GitHub release.
A failed run leaves no tag.

## The security split

Build jobs run the repository's code with no environment, secrets or id-token.
Sign and publish jobs run in the `release` environment, check out only
`.github/release/`, and treat the builds as data checked against the SHA256SUMS
digest each job reports.

## The release environment

| Name | Kind | What |
|---|---|---|
| `APPLE_SIGNING_CERTIFICATE` | secret | base64 `.p12` with the Developer ID Application identity (team 9G68SMNHEU) |
| `APPLE_SIGNING_CERTIFICATE_PASSWORD` | secret | optional; the `.p12`'s password |
| `APPLE_NOTARY_KEY` | secret | App Store Connect API key (`.p8`, PEM or base64) for notarytool |
| `APPLE_NOTARY_KEY_ID` | variable | the key's ID |
| `APPLE_NOTARY_ISSUER_ID` | variable | the key's issuer UUID (App Store Connect › Users and Access › Integrations) |

Without them, dev and check runs publish nothing and stable fails.

## Updates

Release builds know their channel (`CLIP_BUILD_CHANNEL`). They check 10 s after
launch and then hourly (dev builds every 10 minutes). They install whatever is
current on their channel when it isn't the running version, after checking the
download's SHA-256 and that it is `dev.vogel.clip`, notarized and signed by
team 9G68SMNHEU (`scripts/update-requirement.txt`). Local builds
(`scripts/bundle.sh`) never update. `defaults write dev.vogel.clip
DisableUpdates -bool true` turns updates off.
