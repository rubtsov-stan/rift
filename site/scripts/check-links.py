#!/usr/bin/env python3
"""Checks the links of a built site with lychee.

    site/scripts/check-links.py SITE_DIR

Links to this repository's own files on github.com (blob or tree on main) are
checked against the checkout instead, so a missing file or heading fails and
github.com refusing the runner does not. Other links that answer 429 or 503
are asked again with backoff (lychee 0.24 retries 429 itself but not 503); the
ones that still do are reported as warnings. Any other failure fails.
Run from the repository; needs lychee on PATH.
"""
import json
import os
import subprocess
import sys
import tempfile
import time

OWN = r"https://github\.com/rubtsov-stan/rift/(?:blob|tree)/main/"
SOFT = {429, 503}
# Seconds before each extra round for links still answering 429 or 503.
BACKOFF = (4, 8, 16)


def lychee(args, out):
    """Runs lychee with JSON output into OUT; returns its report, or exits on a lychee failure."""
    rc = subprocess.call(["lychee", "--no-progress", "--format", "json", "--output", out, *args])
    # 2 means some links failed; anything else besides 0 is lychee itself.
    if rc not in (0, 2):
        sys.exit(rc)
    with open(out) as f:
        return json.load(f)


def failures(report):
    """(page, url, status) for every failed or timed-out link of a report."""
    for kind in ("error_map", "timeout_map"):
        for page, items in report.get(kind, {}).items():
            for e in items:
                yield page, e["url"], e.get("status") or {}


def soft(url, status):
    return status.get("code") in SOFT and not url.startswith("file:")


def main() -> int:
    site = sys.argv[1]
    root = subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True).strip()
    with tempfile.TemporaryDirectory() as tmp:
        out = os.path.join(tmp, "links.json")
        report = lychee([
            "--include-fragments", "--max-retries", "4", "--retry-wait-time", "2",
            "--remap", f"^{OWN}(.*)$ file://{root}/$1",
            "--exclude-path", os.path.join(site, "fonts"),
            "--root-dir", os.path.abspath(site), site,
        ], out)
        hard = [f for f in failures(report) if not soft(f[1], f[2])]
        pending = [f for f in failures(report) if soft(f[1], f[2])]
        for wait in BACKOFF:
            if not pending:
                break
            time.sleep(wait)
            # URLs on the command line are pages to read; a file of URLs is links to check.
            listed = os.path.join(tmp, "pending.txt")
            with open(listed, "w") as f:
                f.write("\n".join(sorted({url for _, url, _ in pending})) + "\n")
            again = lychee(["--max-retries", "0", listed], out)
            still = {url: status for _, url, status in failures(again)}
            # A link that now fails another way counts as a failure.
            hard += [(p, u, still[u]) for p, u, _ in pending if u in still and not soft(u, still[u])]
            pending = [(p, u, still[u]) for p, u, _ in pending if u in still and soft(u, still[u])]

    for page, url, status in hard:
        print(f"::error::{page}: {url} ({status.get('text', 'no status')})")
    for page, url, status in pending:
        print(f"::warning::{page}: {url} ({status.get('text', 'no status')}, after {len(BACKOFF)} more tries)")
    print(f"{report['total']} links: {len(hard)} failed, {len(pending)} still answered 429 or 503 and are warnings")
    return 1 if hard else 0


if __name__ == "__main__":
    sys.exit(main())
