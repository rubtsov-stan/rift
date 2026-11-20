#!/usr/bin/env python3
"""Replays the VS Code extension's request sequence against `rift serve`.

    bench/serve-replay.py BASE HEAD DIR [RUNS]

BASE and HEAD are `rift` binaries, DIR holds `a` and `b` (for example from
`rift-gen --seed 7 --change-ratio 0.001 --out DIR`). The sequence is the
extension's: initialize, open, summary, the first page of segments, the
visible span of that page, then one `hunk` (context 3) per segment with a
`set_visible` on its A range after each, as `show` does in extension.ts.

Each run starts a new server, waits for the scan and for the refinement the
visible span asked for, and times only the hunk requests. Runs of the two
binaries alternate, BASE first, after one untimed run of each. The answers of
every run must match its side's first one, and HEAD's must match BASE's.
Prints the median and range of the hunk phase per side, the range of the
peak heap each server printed on exit (`rift serve --stats`), and the 1-minute
load average before and after each run.
"""
import hashlib
import json
import os
import re
import statistics
import subprocess
import sys
import time

PAGE = 100


class Server:
    def __init__(self, binary):
        self.p = subprocess.Popen([binary, "serve", "--stats"], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.id = 0
        self.notes = []

    def _read(self):
        n = None
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise SystemExit("server closed")
            line = line.strip()
            if not line:
                break
            if line.lower().startswith(b"content-length:"):
                n = int(line.split(b":")[1])
        return json.loads(self.p.stdout.read(n))

    def call(self, method, params):
        self.id += 1
        body = json.dumps({"jsonrpc": "2.0", "id": self.id, "method": method,
                           "params": params}).encode()
        self.p.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
        self.p.stdin.flush()
        while True:
            m = self._read()
            if m.get("id") == self.id:
                if "error" in m:
                    raise SystemExit(f"{method}: {m['error']}")
                return m["result"]
            self.notes.append(m)

    def wait_note(self, pred):
        while True:
            for m in self.notes:
                if pred(m):
                    return
            self.notes.append(self._read())

    def close(self):
        """Ends the server; returns the peak heap it printed, in bytes."""
        self.p.stdin.close()
        err = self.p.stderr.read().decode()
        self.p.wait()
        m = re.search(r"peak heap (\d+) bytes", err)
        if not m:
            raise SystemExit(f"no peak heap from the server: {err!r}")
        return int(m.group(1))


def run(binary, a, b):
    s = Server(binary)
    s.call("initialize", {"protocol": 1, "client": {"name": "replay", "version": "0"}})
    r = s.call("open", {"a": a, "b": b, "mem": "256M"})
    sid = r["session"]
    s.wait_note(lambda m: m.get("method") == "session_state"
                and m["params"].get("state") in ("ready", "failed"))
    if any(m.get("method") == "session_state" and m["params"].get("state") == "failed"
           for m in s.notes):
        raise SystemExit("scan failed")
    s.call("summary", {"session": sid})
    page = s.call("segments", {"session": sid, "offset": 0, "limit": PAGE})["segments"]
    span = {"start": min(x["a"]["start"] for x in page),
            "count": max(x["a"]["start"] + x["a"]["count"] for x in page)
            - min(x["a"]["start"] for x in page)}
    s.call("set_visible", {"session": sid, "a": span})
    for _ in range(600):
        p = s.call("summary", {"session": sid}).get("progress")
        if p and p["phase"] == "refine" and p["done"] == p["total"]:
            break
        time.sleep(0.05)
    else:
        raise SystemExit("refinement did not finish in 30 s")
    h = hashlib.sha256()
    t0 = time.perf_counter()
    for x in page:
        hk = s.call("hunk", {"session": sid, "segment": x["index"], "context": 3})
        h.update(json.dumps(hk, sort_keys=True).encode())
        s.call("set_visible", {"session": sid, "a": hk["range"]["a"]})
    dt = time.perf_counter() - t0
    s.call("close", {"session": sid})
    return dt, h.hexdigest(), s.close()


def main():
    base, head, d = sys.argv[1:4]
    runs = int(sys.argv[4]) if len(sys.argv) > 4 else 10
    a, b = os.path.join(d, "a"), os.path.join(d, "b")
    side = {"base": base, "head": head}
    ref = {}
    for k, v in side.items():
        ref[k] = run(v, a, b)[1]
    if ref["base"] != ref["head"]:
        raise SystemExit("answers differ: head against base")
    times = {"base": [], "head": []}
    heaps = {"base": [], "head": []}
    loads = []
    for _ in range(runs):
        for k, v in side.items():
            l0 = os.getloadavg()[0]
            dt, dig, heap = run(v, a, b)
            loads += [l0, os.getloadavg()[0]]
            if dig != ref[k]:
                raise SystemExit(f"answers of a {k} run changed")
            times[k].append(dt)
            heaps[k].append(heap)
    for k, t in times.items():
        print(f"{k}: median {statistics.median(t) * 1000:.1f} ms, "
              f"min-max {min(t) * 1000:.1f}-{max(t) * 1000:.1f} ms, runs {len(t)}, "
              f"peak heap {min(heaps[k])}-{max(heaps[k])} bytes")
    mb, mh = statistics.median(times["base"]), statistics.median(times["head"])
    spread = (max(times["base"]) - min(times["base"]) + max(times["head"]) - min(times["head"])) / 2
    print(f"head vs base: {100 * (mh - mb) / mb:+.1f}%, threshold "
          f"{100 * max(0.03 * mb, spread) / mb:.1f}%, load {min(loads):.2f}-{max(loads):.2f}")


main()
