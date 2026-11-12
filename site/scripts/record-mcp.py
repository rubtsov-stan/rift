#!/usr/bin/env python3
"""Records a rift-mcp session over standard input and output.

    site/scripts/record-mcp.py [OUT_FILE]     default site/data/mcp-session.json

Needs release builds of rift-gen and rift-mcp (cargo build --release -p
rift-gen -p rift-mcp), found in target/release or on PATH. The data comes from
rift-gen with the seed and size in SESSIONS, in a temporary directory that
the script removes. Requests and answers go to the JSON file as they crossed
the pipe, with the size of each answer line in bytes. The guide page "MCP
server" is built from that file.
"""
import json, os, shutil, subprocess, sys, tempfile

root = os.path.abspath(os.path.join(os.path.dirname(__file__), '..', '..'))
out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(root, 'site', 'data', 'mcp-session.json')
bindir = os.path.join(root, 'target', 'release')


def tool(name):
    p = os.path.join(bindir, name)
    return p if os.path.exists(p) else shutil.which(name) or sys.exit(f'record-mcp: {name} not found, build it first')


# Fixed seeds and sizes: the same files every run.
SESSIONS = [
    dict(id='sql', seed=7, size='1G', gen=['--profile', 'sql'], a='a.sql', b='b.sql',
         what='1 GiB pg_dump-like SQL pair with inserts, deletes, edits and moved blocks'),
]


class Client:
    def __init__(self, cwd):
        self.p = subprocess.Popen([tool('rift-mcp')], cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL)
        self.n = 0
        self.steps = []

    def send(self, msg):
        self.p.stdin.write((json.dumps(msg, separators=(',', ':')) + '\n').encode())
        self.p.stdin.flush()

    def call(self, method, params=None, note=None):
        self.n += 1
        req = {'jsonrpc': '2.0', 'id': self.n, 'method': method}
        if params is not None:
            req['params'] = params
        self.send(req)
        while True:
            line = self.p.stdout.readline()
            if not line:
                sys.exit('record-mcp: rift-mcp closed its output')
            msg = json.loads(line)
            if msg.get('id') == self.n:
                break
        step = dict(note=note, request=req, response=msg, response_bytes=len(line.rstrip(b'\n')))
        self.steps.append(step)
        return msg

    def notify(self, method):
        self.send({'jsonrpc': '2.0', 'method': method})

    def tool(self, name, args, note):
        r = self.call('tools/call', {'name': name, 'arguments': args}, note)
        return r.get('result', {}).get('structuredContent')

    def close(self):
        self.p.stdin.close()
        self.p.wait(timeout=30)


def handshake(c):
    c.call('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {},
                          'clientInfo': {'name': 'example-client', 'version': '1.0'}},
           'The client opens the connection.')
    c.notify('notifications/initialized')
    c.call('tools/list', {}, 'The client asks which tools the server has.')


def session_sql(c, a, b):
    pair = {'a': a, 'b': b}
    handshake(c)
    s = c.tool('summary', pair, 'How different are the two dumps?')
    first = c.tool('next_difference', {**pair, 'after_line': 0}, 'Where is the first difference?')
    c.tool('hunks_at', {**pair, 'line': first['range']['a']['start']}, 'What does it change?')
    # The widest range of the list is the likeliest to need more than 20 lines.
    big = max(s['first_differences'], key=lambda r: r['a']['count'] + r['b']['count'])
    h = c.tool('hunks_at', {**pair, 'line': big['a']['start'], 'max_lines': 20},
               'The widest change in the list. Show only the start of it.')
    assert h['truncated'], 'the recorded call no longer shows a cut hunk'
    c.tool('next_difference', {**pair, 'after_line': big['a']['start']}, 'And the difference after that one?')
    c.tool('summary', {**pair, 'mem': '1G'}, 'Ask for a bigger memory budget than the server allows.')


work = tempfile.mkdtemp(prefix='record-mcp-')
try:
    data = dict(
        about='Recorded by site/scripts/record-mcp.py. Requests and answers as they crossed standard input and output of rift-mcp.',
        sessions=[])
    for spec, run in zip(SESSIONS, (session_sql,)):
        d = os.path.join(work, spec['id'])
        cmd = [tool('rift-gen'), '--seed', str(spec['seed']), '--size', spec['size'], *spec['gen'], '--out', d]
        subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL)
        os.rename(os.path.join(d, 'a'), os.path.join(d, spec['a']))
        os.rename(os.path.join(d, 'b'), os.path.join(d, spec['b']))
        c = Client(d)
        run(c, spec['a'], spec['b'])
        c.close()
        data['sessions'].append(dict(
            id=spec['id'], what=spec['what'],
            rift_gen=' '.join(['rift-gen', '--seed', str(spec['seed']), '--size', spec['size'], *spec['gen']]),
            a_bytes=os.path.getsize(os.path.join(d, spec['a'])),
            b_bytes=os.path.getsize(os.path.join(d, spec['b'])),
            server=c.steps[0]['response']['result']['serverInfo'],
            steps=c.steps))
        shutil.rmtree(d)
finally:
    shutil.rmtree(work, ignore_errors=True)

with open(out, 'w') as f:
    json.dump(data, f, indent=1)
    f.write('\n')
print(f'wrote {out}')
