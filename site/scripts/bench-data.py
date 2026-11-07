#!/usr/bin/env python3
"""Reads the committed bench report and writes site/data/bench-linux.json.

Every number the site shows from the Linux run comes from this file, which is
computed from bench/results/*/results.json. Medians are over the repetitions
of the same command and size, SQL profile.
"""
import glob, json, os, statistics, sys

root = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..')
src = sorted(glob.glob(os.path.join(root, 'bench', 'results', '*', 'results.json')))[-1]
d = json.load(open(src))
h = d['host']
MIB = 1024 * 1024
SIZES = {268435456: '256 MiB', 1073741824: '1 GiB', 2147483648: '2 GiB'}
TOOLS = [
    ('rift --stats --mem 256M', 'rift (full diff)'),
    ('rift --summary --mem 256M', 'rift --summary'),
    ('rift --stats --mem 64M', 'rift --mem 64M (full diff)'),
    ('rift --stats --mem 1G', 'rift --mem 1G (full diff)'),
    ('diff a b', 'GNU diff'),
    ('git diff --no-index a b', 'git diff --no-index'),
]

def med(xs):
    xs = [x for x in xs if x is not None]
    return statistics.median(xs) if xs else None

rows = []
for size, label in SIZES.items():
    for tool, name in TOOLS:
        runs = [r for r in d['runs'] if r['profile'] == 'sql' and r['size_bytes'] == size and r['tool'] == tool and r['group'] in ('compare', 'generated')]
        if not runs:
            continue
        ok = [r for r in runs if r['result'] == 'success']
        killed = any(r['result'] == 'oom-kill' for r in runs)
        row = {'size': label, 'size_bytes': size, 'tool': name, 'runs': len(ok)}
        if ok:
            row['wall_s'] = med([r['wall_s'] for r in ok])
            row['rss_mib'] = round(med([r['max_rss_kb'] for r in ok]) / 1024, 1)
            ph = med([r.get('rift_peak_heap_bytes') for r in ok])
            row['heap_mib'] = round(ph / MIB, 1) if ph else None
            row['output_bytes'] = med([r.get('output_bytes') for r in ok])
            # Strings for the page, so 113.0 stays 113.0 and 1.2 s reads 1.20 s.
            row['time'] = '%.2f s' % row['wall_s']
            row['rss'] = '%.1f GiB' % (row['rss_mib'] / 1024) if row['rss_mib'] >= 1024 else '%.1f MiB' % row['rss_mib']
            row['heap'] = '%.1f MiB' % row['heap_mib'] if row['heap_mib'] else ''
            row['sbar'] = round(size * 100 / 2147483648, 1)
            row['hbar'] = round(row['heap_mib'] * 100 / 256, 1) if row['heap_mib'] else 0
        if killed:
            row['note'] = 'killed at the memory cap'
        rows.append(row)

out = {
    'source': os.path.relpath(src, root),
    'report': os.path.relpath(os.path.join(os.path.dirname(src), 'report.md'), root),
    'script': 'bench/linux-safe.sh',
    'host': {
        'cpu': h['cpu_model'].replace('(R)', '').replace('(TM)', '').replace('  ', ' ').replace(' CPU @ 3.30GHz', ''),
        'cpu_full': h['cpu_model'],
        'ram_gb': round(int(h['mem_total_bytes']) / 1e9),
        'threads': h['cpu_threads'],
        'os': h['distro'],
        'kernel': h['kernel'],
        'scratch': h['scratch_fs'].split(',')[0],
        'date': h['date'][:10],
        'rift_ref': h['rift_ref'],
        'seed': 7,
    },
    'rows': rows,
}
dest = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'data', 'bench-linux.json')
json.dump(out, open(dest, 'w'), indent=1)
open(dest, 'a').write('\n')
