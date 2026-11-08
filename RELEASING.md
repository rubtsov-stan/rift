# Releasing

1. Merge every change planned for the release and wait for main to go green
   (CI, CodeQL, Site, supply chain). Remove every "not yet published" line
   from `editors/vscode/README.md` and `CHANGELOG.md` first: the README is
   packaged into the VSIX files and becomes the store page.

2. Run Miri on the exact commit you will tag and wait for it to pass:

   ```sh
   gh workflow run miri.yml --ref main
   ```

   Do not tag while it is red or still running.

3. Tag the commit and push the tag together with a release branch at the
   same commit:

   ```sh
   git tag -a vX.Y.Z -m vX.Y.Z <commit>
   git push --atomic origin <commit>:refs/heads/release/vX.Y.Z vX.Y.Z
   ```

   GitHub lets `GITHUB_TOKEN` create a release only when the target commit's
   workflow files match the tip of some branch. The release targets the
   tagged commit, so if main has moved on and changed `.github/workflows`,
   the `host` job gets HTTP 403 unless a branch points at that commit.

4. Watch the Release workflow. The Debian packages, VS Code packages and
   Release provenance workflows run from it once the release is published.
   On the release page, check the archives, the shell installer, the checksums, the `.deb` files, the VSIX
   packages and, for each archive, its `.sigstore.json` bundle and
   `.intoto.jsonl` provenance, and verify one archive against its bundle:

   ```sh
   gh attestation verify <archive> --bundle <archive>.sigstore.json --repo rubtsov-stan/rift
   ```

   0.3.1 is the first release on this flow: the `custom-deb`,
   `custom-vscode` and `custom-provenance` jobs have not run on a real tag
   before it. They run after the release is published, so a failure leaves
   the release without some of its files, not broken. Each can be run by
   hand against the tag:

   ```sh
   gh workflow run deb.yml -f tag=vX.Y.Z
   gh workflow run vscode.yml -f tag=vX.Y.Z
   gh workflow run provenance.yml -f tag=vX.Y.Z
   ```

5. If the `host` job still gets 403, push the release branch at the tagged
   commit and rerun the failed jobs:

   ```sh
   git push origin <commit>:refs/heads/release/vX.Y.Z
   gh run rerun <run-id> --failed
   ```

6. Delete the release branch:

   ```sh
   git push origin --delete release/vX.Y.Z
   ```
