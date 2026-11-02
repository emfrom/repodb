# Release artifact provenance

`.github/workflows/release-provenance.yml` is a manually dispatched build-and-attest workflow. It accepts only the protected `main` branch or a `v*` tag, builds the Cargo package and Linux x86_64 release binary, writes a SHA-256 manifest, uploads a 90-day GitHub Actions artifact, and creates GitHub build-provenance attestations for each subject. Attestations use GitHub's keyless OIDC/Sigstore-backed identity. The workflow does not create a GitHub Release, publish to a registry, deploy, or sign Git tags.

After downloading the workflow artifact, verify its local file checksums and provenance before use. The provenance commands require a GitHub CLI release that includes `gh attestation verify`:

```sh
sha256sum --check SHA256SUMS.txt
gh attestation verify repodb-<version>.crate --repo emfrom/repodb
gh attestation verify repodb-linux-x86_64 --repo emfrom/repodb
```

Confirm the attestation identifies the expected repository, workflow, source commit, and protected ref. A checksum alone only detects accidental changes; it does not establish publisher identity. Do not treat an unattested file or a file from an unexpected ref as an official release.

This workflow is a provenance mechanism, not release authorization. Publishing an artifact or creating a release requires a separate explicit release decision and is not automated here.
