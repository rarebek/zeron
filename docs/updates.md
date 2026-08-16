# Production updates

Zeron releases use two independent trust layers:

1. GitHub Actions builds platform artifacts. macOS artifacts must be Developer ID signed and notarized.
2. An offline Ed25519 release key signs the exact bytes of `manifest.json`. Clients embed only the public key and reject unsigned or altered metadata.

The stable and beta pointers live at:

- `https://github.com/rarebek/zeron/releases/download/channel-stable/manifest.json`
- `https://github.com/rarebek/zeron/releases/download/channel-beta/manifest.json`

Each signed manifest points to immutable artifacts under the matching `v<version>` GitHub release. Sync configuration and update configuration are deliberately separate.

## One-time signing setup

Generate the Ed25519 key on a trusted offline machine:

```bash
openssl genpkey -algorithm ED25519 -out update-signing.pem
openssl pkey -in update-signing.pem -pubout -outform DER \
  | tail -c 32 | base64
```

Store the PEM as the GitHub Actions secret `UPDATE_SIGNING_PRIVATE_KEY`. Store the printed base64 public key as `UPDATE_SIGNING_PUBLIC_KEY`. Keep an encrypted offline backup of the private key; never commit it or place it on a release server.

Tagged macOS releases additionally require:

- `MACOS_CERT_P12`
- `MACOS_CERT_PASSWORD`
- `AC_API_KEY_P8`
- `AC_API_KEY_ID`
- `AC_API_ISSUER_ID`

The release workflow fails closed when any production credential is absent or the public/private update keys do not match.

## Publishing

1. Bump `[workspace.package].version` in `Cargo.toml`.
2. Merge and validate the release commit.
3. Tag it `v<version>` and push the tag.

Versions containing a SemVer prerelease suffix publish to `beta`; ordinary versions publish to `stable`. The workflow builds Linux x86_64/aarch64 and macOS arm64 artifacts, signs the manifest, publishes the immutable version release, then replaces only the signed channel pointer.

## Client behavior

- Checks begin after startup and repeat every six hours; failed checks retry after 30 minutes.
- HTTPS is mandatory except for explicit localhost development.
- Manifest signatures, schema, channel, artifact name, byte length, and SHA-256 must all match.
- macOS also requires `codesign --verify --deep --strict` and Gatekeeper acceptance before staging.
- macOS downloads automatically, asks for restart, retains the previous bundle, and restores it if the new process fails its launch health window.
- Managed Linux installs stage into a versioned directory, atomically switch `current`, retain `previous`, wait for active sessions and terminals to finish, and roll back if service restart fails.
- Source checkouts are report-only and update through Git.

## Configuration

- `ZERON_UPDATE_CHANNEL=stable|beta` selects the channel.
- `ZERON_UPDATE_URL` overrides the compiled release root for self-hosting.
- `ZERON_UPDATE_PUBLIC_KEY_OVERRIDE` is a development/self-host override; production builds embed `ZERON_UPDATE_PUBLIC_KEY` at compile time.
- Managed daemons and desktop builds update automatically by default. Set `ZERON_AUTO_UPDATE=0` to disable unattended daemon application and require a manual desktop download click.

Never rotate the update key by publishing a manifest signed only by the new key. Ship a normal release embedding the next public key first, maintain an overlap window if multi-key support is added, then retire the old key.
