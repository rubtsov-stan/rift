#!/usr/bin/env python3
"""Renders man/rift.1 to the HTML fragment the reference page includes.

    site/scripts/render-man.py [MAN_FILE] [OUT_FILE]

Handles the macros clap_mangen and the hand-written head use: .TH .SH .PP .TP
.IP .RS .RE .nf .fi and the font escapes. Standard library only.
"""
import html, os, re, sys

here = os.path.dirname(os.path.abspath(__file__))
src = sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, '..', '..', 'man', 'rift.1')
dest = sys.argv[2] if len(sys.argv) > 2 else os.path.join(here, '..', 'data', 'man.html')

def inline(s):
    s = s.replace('\\\\', '\x00')
    out, font = [], 'R'
    parts = re.split(r'(\\f[BIRP])', s)
    def close():
        if font == 'B': out.append('</code>')
        elif font == 'I': out.append('</em>')
    for p in parts:
        m = re.fullmatch(r'\\f([BIRP])', p)
        if m:
            close()
            font = 'R' if m.group(1) in 'RP' else m.group(1)
            if font == 'B': out.append('<code>')
            elif font == 'I': out.append('<em>')
            continue
        t = p.replace('\\-', '-').replace('\\(aq', "'").replace('\\(bu', '•').replace('\\&', '').replace('\\e', '\\')
        t = html.escape(t, quote=False).replace('\x00', '\\')
        out.append(t)
    close()
    return ''.join(out)

lines = open(src).read().split('\n')
out = []
stack = []        # open containers: 'p', 'dl', 'dd', 'ul', 'li', 'pre'
def pop_to(*names):
    while stack and stack[-1] not in names:
        out.append('</%s>' % stack.pop())
def close_all():
    while stack:
        out.append('</%s>' % stack.pop())
def para():
    if stack and stack[-1] == 'p': return
    pop_to('dd', 'li')
    out.append('<p>'); stack.append('p')
def text(t):
    para(); out.append(inline(t) + ' ')

i = 0
nested = []       # saved depths for .RS/.RE
pending_tp = False
while i < len(lines):
    l = lines[i]; i += 1
    if l.startswith('.ie') or l.startswith('.el') or l.startswith('.TH'):
        continue
    m = re.match(r'\.(\w+)\s*(.*)', l)
    if l.startswith('.') and m:
        mac, arg = m.group(1), m.group(2)
        if mac == 'SH':
            close_all(); nested = []
            out.append('<h2 id="%s">%s</h2>' % (re.sub(r'\W+', '-', arg.lower()).strip('-'), html.escape(arg.title())))
        elif mac == 'PP':
            pop_to('dd', 'li') if nested else close_all()
            if nested: pass
        elif mac == 'TP':
            if stack and stack[-1] == 'p': out.append('</p>'); stack.pop()
            if nested and len(stack) == nested[-1]:
                out.append('<dl>'); stack.append('dl')      # a list nested in the open definition
            else:
                if stack and stack[-1] == 'dd': out.append('</dd>'); stack.pop()
                if not stack or stack[-1] != 'dl':
                    pop_to('dl') if stack else None
                    if not stack or stack[-1] != 'dl':
                        out.append('<dl>'); stack.append('dl')
            out.append('<dt>%s</dt><dd>' % inline(lines[i])); i += 1
            stack.append('dd')
        elif mac == 'IP':
            if arg.startswith('\\(bu'):
                if not stack or stack[-1] not in ('ul', 'li'):
                    close_all(); out.append('<ul>'); stack.append('ul')
                elif stack[-1] == 'li' or stack[-1] == 'p':
                    pop_to('ul')
                out.append('<li>'); stack.append('li')
            else:
                if stack and stack[-1] == 'p': out.append('</p>'); stack.pop()
                # continuation paragraph inside the current definition
        elif mac == 'RS':
            if stack and stack[-1] == 'p': out.append('</p>'); stack.pop()
            nested.append(len(stack))
        elif mac == 'RE':
            if stack and stack[-1] == 'p': out.append('</p>'); stack.pop()
            d = nested.pop() if nested else 0
            while len(stack) > d:
                out.append('</%s>' % stack.pop())
        elif mac == 'nf':
            if stack and stack[-1] == 'p': out.append('</p>'); stack.pop()
            buf = []
            while not lines[i].startswith('.fi'):
                buf.append(inline(lines[i])); i += 1
            i += 1
            out.append('<pre><code>' + '\n'.join(buf).replace('<code>', '').replace('</code>', '') + '</code></pre>')
        continue
    if l.strip() == '':
        continue
    text(l)
close_all()
open(dest, 'w').write('\n'.join(out).replace(' </p>', '</p>').replace(' </li>', '</li>').replace(' </dd>', '</dd>') + '\n')
