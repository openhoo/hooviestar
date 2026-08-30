# Releasing Hooviestar

Hooviestar publishes a Windows NSIS installer, Linux AppImage, and Debian package from an annotated `v<semver>` tag. Release builds are the only hosted jobs that build installers, using explicitly pinned Node, Rust, Cosign, Syft, and GitHub Action versions. Every release remains a GitHub draft until NSIS archive integrity, AppImage contents, Debian metadata, updater signatures and manifest, SPDX 2.3 SBOM, checksums, Sigstore signature, GitHub build-provenance and SBOM attestations, and Windows Authenticode pass verification. Repository release immutability is enabled: publication permanently locks the tag and assets and creates a GitHub release attestation.

## One-time signing setup

The Tauri updater private key and its password live in the repository secrets `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. The matching public key is committed in `src-tauri/tauri.conf.json`. Never replace that pair without a staged key-rotation plan: installed clients trust the committed public key.

Windows releases additionally require a code-signing certificate with the Code Signing EKU and its private key in a password-protected PFX. Store raw base64, without PEM headers or line breaks, plus the PFX password:

```bash
base64 -w0 certificate.pfx | gh secret set WINDOWS_CERTIFICATE --repo openhoo/hooviestar
read -rsp 'PFX password: ' HOOVIESTAR_PFX_PASSWORD
printf '%s' "$HOOVIESTAR_PFX_PASSWORD" | gh secret set WINDOWS_CERTIFICATE_PASSWORD --repo openhoo/hooviestar
unset HOOVIESTAR_PFX_PASSWORD
```

Production releases should leave repository variable `WINDOWS_CERTIFICATE_TRUST_MODE` unset, which defaults to `trusted`. For temporary pipeline testing, generate a self-signed certificate on Linux and opt in explicitly:

```bash
(
  set -euo pipefail
  umask 077
  signing_dir=$(mktemp -d)
  trap 'find "$signing_dir" -depth -delete' EXIT
  export HOOVIESTAR_PFX_PASSWORD=$(openssl rand -base64 48 | tr -d '\r\n')
  openssl req -newkey rsa:3072 -x509 -sha256 -days 365 \
    -subj '/CN=Hooviestar Development/O=OpenHoo' \
    -addext 'basicConstraints=critical,CA:false' \
    -addext 'keyUsage=critical,digitalSignature' \
    -addext 'extendedKeyUsage=codeSigning' \
    -keyout "$signing_dir/private-key.pem" \
    -out "$signing_dir/certificate.pem" \
    -passout env:HOOVIESTAR_PFX_PASSWORD
  openssl pkcs12 -export \
    -name 'Hooviestar Development' \
    -inkey "$signing_dir/private-key.pem" \
    -in "$signing_dir/certificate.pem" \
    -out "$signing_dir/certificate.pfx" \
    -passin env:HOOVIESTAR_PFX_PASSWORD \
    -passout env:HOOVIESTAR_PFX_PASSWORD
  base64 -w0 "$signing_dir/certificate.pfx" \
    | gh secret set WINDOWS_CERTIFICATE --repo openhoo/hooviestar
  printf '%s' "$HOOVIESTAR_PFX_PASSWORD" \
    | gh secret set WINDOWS_CERTIFICATE_PASSWORD --repo openhoo/hooviestar
  gh variable set WINDOWS_CERTIFICATE_TRUST_MODE \
    --repo openhoo/hooviestar --body self-signed
)
```

Self-signed mode trusts that exact certificate only inside the ephemeral Windows build runner. It proves that Authenticode signing and timestamping worked, but Windows users still see an unknown or untrusted publisher unless they separately install the certificate. Do not distribute a self-signed certificate as production publisher trust. Replace both certificate secrets with a publicly trusted signing service and delete `WINDOWS_CERTIFICATE_TRUST_MODE` before a production release.

The release preflight fails before building anything if any signing secret is absent, the trust mode is invalid, or the updater private key cannot sign a probe that verifies against the committed public key. The Windows job imports the PFX into its ephemeral user certificate store, rejects missing private keys, wrong EKUs, certificates outside their validity period or expiring within seven days, and certificate/trust-mode mismatches. In self-signed mode only, it temporarily trusts the public certificate in the runner. The job injects the thumbprint into a build-only Tauri config, then verifies the finished NSIS installer, exact signing certificate, and timestamp with `Get-AuthenticodeSignature`.

## Prepare and publish

Start from a clean `main` synchronized with `origin/main`. Select the next semantic version, then update every manifest and lockfile together:

```bash
npm run release:prepare -- 0.2.0
npm test
npm run build
npm run release:test
npm audit --audit-level=high
cargo audit
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm run release:check
git diff --check
```

`release:prepare` is idempotent for a target version. It accepts strict SemVer without build metadata, updates every project version together, refreshes only the two workspace entries in `Cargo.lock`, validates the result, and restores all touched manifests if validation fails.

Review and commit the version change. Push the commit and wait for CI. Create and push an annotated tag only after CI succeeds:

```bash
git tag -a v0.2.0 -m 'Hooviestar v0.2.0'
git push origin v0.2.0
```

`.github/workflows/release.yml` verifies that the exact tagged commit belongs to `main` and that each stable version is newer than the currently published stable release. It builds both supported platforms, creates a draft, and publishes it only after the final verification job succeeds. The final job verifies each updater signature against the committed public key, then creates `latest.json` once from the complete uploaded installer/signature set, avoiding concurrent matrix updates to shared metadata. A failure deliberately leaves a draft rather than exposing a partial release.

If a job fails while the release is still a draft, fix the workflow or credentials and rerun the failed workflow. Never recreate or move the tag. After publication, assets and the tag are immutable. Any defect requires a new patch version and release; do not delete and try to reuse the old tag name.

## Automatic updates

Packaged builds check `https://github.com/openhoo/hooviestar/releases/latest/download/latest.json` on startup with a 30-second metadata timeout. When a newer signed version exists, Hooviestar downloads it with a bounded 30-minute timeout, verifies the Tauri signature, installs it, and restarts. Development builds never check for updates.

Automatic update assets are the Windows NSIS installer, Linux AppImage, and Debian package. Tauri detects the package type embedded by the bundler, verifies the matching signature, and uses the matching `windows-x86_64-nsis`, `linux-x86_64-appimage`, or `linux-x86_64-deb` manifest entry. Debian upgrades request system authorization before running `dpkg -i`; users on desktops without a supported privilege prompt can still download and install the signed `.deb` manually.

## Independent verification

Download the release and verify checksums, Sigstore signature, GitHub provenance, and installer SBOM attestations:

```bash
gh release download v0.2.0 --repo openhoo/hooviestar --dir hooviestar-v0.2.0
cd hooviestar-v0.2.0
sha256sum --check SHA256SUMS
cosign verify-blob \
  --bundle SHA256SUMS.sigstore.json \
  --certificate-identity 'https://github.com/openhoo/hooviestar/.github/workflows/release.yml@refs/tags/v0.2.0' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-github-workflow-repository openhoo/hooviestar \
  --certificate-github-workflow-ref refs/tags/v0.2.0 \
  SHA256SUMS
verification_dir=$(mktemp -d)
gh api 'repos/openhoo/hooviestar/contents/src-tauri/tauri.conf.json?ref=v0.2.0' --jq .content \
  | base64 --decode \
  | jq -r '.plugins.updater.pubkey' \
  | base64 --decode > "$verification_dir/updater.pub"
for installer in *.exe *.AppImage *.deb; do
  base64 --decode "$installer.sig" > "$verification_dir/updater.minisig"
  minisign -Vm "$installer" \
    -p "$verification_dir/updater.pub" \
    -x "$verification_dir/updater.minisig"
done
find "$verification_dir" -depth -delete
for asset in *; do
  gh attestation verify "$asset" \
    --repo openhoo/hooviestar \
    --signer-workflow openhoo/hooviestar/.github/workflows/release.yml \
    --source-ref refs/tags/v0.2.0 \
    --deny-self-hosted-runners
done
for installer in *.exe *.AppImage *.deb; do
  gh attestation verify "$installer" --repo openhoo/hooviestar \
    --signer-workflow openhoo/hooviestar/.github/workflows/release.yml \
    --source-ref refs/tags/v0.2.0 \
    --deny-self-hosted-runners \
    --predicate-type https://spdx.dev/Document/v2.3
done
gh release verify v0.2.0 --repo openhoo/hooviestar
for asset in *; do
  gh release verify-asset v0.2.0 "$asset" --repo openhoo/hooviestar
done
```

On Windows, independently confirm the installer certificate:

```powershell
Get-AuthenticodeSignature .\Hooviestar_*_x64-setup.exe | Format-List Status,StatusMessage,SignerCertificate
```

Finally inspect `latest.json`: its version must match the tag and it must contain signed installer-specific entries pointing to the published `.exe`, `.AppImage`, and `.deb` assets.
