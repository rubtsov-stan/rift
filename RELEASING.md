# Releasing

1. Merge every change planned for the release and wait for main to go green
   (CI, CodeQL, Site, supply chain). Every issue in the release's milestone
   is closed. Remove every "not yet published" line from
   `editors/vscode/README.md` and `CHANGELOG.md` first: the README is
   packaged into the VSIX files and becomes the store page.

   The release commit sets the version in `Cargo.toml` (`[workspace.package]`
   and the three `version = "=X.Y.Z"` pins in `[workspace.dependencies]`),
   `Cargo.lock` and `fuzz/Cargo.lock` (`cargo metadata` updates both),
   `editors/vscode/package.json` and `package-lock.json`
   (`npm version X.Y.Z --no-git-tag-version`), the client version in
   `editors/vscode/src/client.ts`, and `man/rift.1` (`cargo xtask man`). Both
   CHANGELOGs get the version and the day of the tag, set just before that
   commit merges. CI's `versions` job fails when one of these files differs
   from `Cargo.toml`. Then check that dist releases exactly that version:

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
   same commit, in one atomic push:

   ```sh
   scripts/release-push.sh vX.Y.Z <commit>
   ```

   The script refuses a tag that differs from `Cargo.toml` at that commit.
   Typed into zsh by hand, `$c:refs/...` in the refspec reads as a history
   modifier and the push fails.

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

   `sha256.sum` lists only what dist builds. Each `.deb`, `.vsix` and `.mcpb`
   carries its own `<file>.sha256` in the same format (not attested, like the
   archives' checksums). Check that all seven exist, and verify one:

   ```sh
   gh release download vX.Y.Z -p 'rift-mcp-X.Y.Z.mcpb*' && sha256sum -c rift-mcp-X.Y.Z.mcpb.sha256
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

5. Publish the crates to crates.io from the tagged commit, once the release
   page checks out. A crates.io version can never be replaced, only yanked,
   so check first:

   ```sh
   git switch --detach vX.Y.Z
   cargo publish --workspace --dry-run
   cargo publish --workspace
   git switch main
   ```

   `cargo publish --workspace` uploads the crates in dependency order and
   waits for each to reach the index before the next. It needs a token from
   `cargo login` with the `publish-update` scope (`publish-new` too for the
   first release of a crate). The order, if you publish one at a time:

   ```sh
   cargo publish -p rift-diff-core
   cargo publish -p rift-diff-session
   cargo publish -p rift-diff-tui
   cargo publish -p rift-diff
   cargo publish -p rift-mcp
   ```

   `rift-gen`, `xtask` and `rift-fuzz` are not published. If an upload
   fails halfway, the crates before it are on crates.io and refuse a second
   upload of this version: publish the rest one at a time with `-p`, in the
   order above. Check the result:

   ```sh
   for c in rift-diff-core rift-diff-session rift-diff-tui rift-diff rift-mcp; do
     curl -s -A release-check "https://crates.io/api/v1/crates/$c" | jq -r '.crate | "\(.name) \(.max_version)"'
   done
   ```

6. If the `host` job still gets 403, push the release branch at the tagged
   commit and rerun the failed jobs:

   ```sh
   git push origin <commit>:refs/heads/release/vX.Y.Z
   gh run rerun <run-id> --failed
   ```

7. Delete the release branch:

   ```sh
   git push origin --delete release/vX.Y.Z
   ```
