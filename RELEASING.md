# Releasing

1. Merge every change planned for the release and wait for main to go green
   (CI, CodeQL, Site, supply chain). Remove every "not yet published" line
   from `editors/vscode/README.md` and `CHANGELOG.md` first: the README is
   packaged into the VSIX files and becomes the store page.

   The release commit sets the version in `Cargo.toml` (`[workspace.package]`),
   `Cargo.lock` and `fuzz/Cargo.lock` (`cargo metadata` updates both),
   `editors/vscode/package.json` and `package-lock.json`
   (`npm version X.Y.Z --no-git-tag-version`), the client version in
   `editors/vscode/src/client.ts`, and `man/rift.1` (`cargo xtask man`). Both
   CHANGELOGs get the version and the day of the tag, set just before that
   commit merges. Then check that dist releases exactly that version:

   ```sh
   dist plan --tag vX.Y.Z    # lists rift-diff and rift-mcp
   ```

   The MCP bundle and the MCP Registry entry need no edit: their version
   comes from the tag, which dist only accepts when it equals `Cargo.toml`,
   and the bundle job checks that the bundled `rift-mcp` reports the same.

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

4. Watch the Release workflow. The Debian packages, VS Code packages, Release
   provenance and MCP Registry workflows run from it once the release is
   published.
   On the release page, check the archives, the shell installer, the checksums, the `.deb` files, the VSIX
   packages and, for each archive, its `.sigstore.json` bundle and
   `.intoto.jsonl` provenance, and verify one archive against its bundle:

   ```sh
   gh attestation verify <archive> --bundle <archive>.sigstore.json --repo rubtsov-stan/rift
   ```

   The MCP Registry workflow attaches `rift-mcp-X.Y.Z.mcpb` with its
   `.sigstore.json` and `.intoto.jsonl`, then publishes
   `io.github.rubtsov-stan/rift-mcp` X.Y.Z to the MCP Registry. Check the
   bundle against its bundle file as above, and the entry:

   ```sh
   curl -s "https://registry.modelcontextprotocol.io/v0.1/servers?search=io.github.rubtsov-stan/rift-mcp"
   ```

   Its `fileSha256` must be the sha256 of the release asset. A published
   registry version can never be changed or replaced, only marked
   deprecated or deleted, so a wrong one costs a version number.

   0.3.1 is the first release on this flow: the `custom-deb`,
   `custom-vscode` and `custom-provenance` jobs have not run on a real tag
   before it. They run after the release is published, so a failure leaves
   the release without some of its files, not broken. Each can be run by
   hand against the tag:

   ```sh
   gh workflow run deb.yml -f tag=vX.Y.Z
   gh workflow run vscode.yml -f tag=vX.Y.Z
   gh workflow run provenance.yml -f tag=vX.Y.Z
   gh workflow run mcp-registry.yml -f tag=vX.Y.Z               # build the bundle only
   gh workflow run mcp-registry.yml -f tag=vX.Y.Z -f publish=true
   ```

   Run the MCP Registry workflow with `publish=true` only when the registry
   does not list that version yet.

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
