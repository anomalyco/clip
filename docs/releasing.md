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

Each published build is also a GitHub release carrying the same bytes as
`Clip.dmg` and `Clip.zip`: stable releases are Latest, so
`https://github.com/anomalyco/clip/releases/latest/download/Clip.dmg` (the
README's link) and `https://anoma.ly/release/clip/desktop/stable/macos-arm64-dmg`
are always the newest stable DMG. Dev builds are prereleases tagged
`v0.0.0-dev-<run>`.

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

The `release` environment holds the variables below. Its secrets come from AWS
Secrets Manager (account `anomaly-secrets`, managed from
[anomalyco/secrets](https://github.com/anomalyco/secrets)), not from GitHub:
sign-macos assumes `role/github/clip-release` with GitHub OIDC and reads only
its own folder, `clip/release/*`. Clip signs as Ryan Vogel's Developer ID
(team 9G68SMNHEU), so it cannot use the shared Anomaly Apple identity that
Hex and Deadbolt read.

| Name | Kind | What |
|---|---|---|
| `clip/release/APPLE_SIGNING_CERTIFICATE` | AWS secret | base64 `.p12`, no password, with the Developer ID Application identity (team 9G68SMNHEU) |
| `clip/release/APPLE_NOTARY_KEY` | AWS secret | App Store Connect API key (`.p8`, PEM or base64) for notarytool |
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
