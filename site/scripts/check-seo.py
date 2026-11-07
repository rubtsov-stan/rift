#!/usr/bin/env python3
"""Checks the built site against the page rules.

    site/scripts/check-seo.py [PUBLIC_DIR]     default site/public

Every page needs a title under 60 characters, a description under 155, one h1,
a canonical link and an alt text on every image. Unless RIFT_SITE_VSCODE=1,
nothing about VS Code may appear in a page or in the sitemap.
"""
import glob, html, os, re, sys
from html.parser import HTMLParser

public = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'public')

class Page(HTMLParser):
    def __init__(self):
        super().__init__()
        self.title = ''; self.in_title = False; self.title_done = False
        self.desc = None; self.canonical = False; self.h1 = 0; self.noalt = 0
    def handle_starttag(self, tag, a):
        a = dict(a)
        if tag == 'title' and not self.title_done: self.in_title = True
        elif tag == 'h1': self.h1 += 1
        elif tag == 'meta' and a.get('name') == 'description': self.desc = a.get('content', '')
        elif tag == 'link' and a.get('rel') == 'canonical': self.canonical = True
        elif tag == 'img' and 'alt' not in a: self.noalt += 1
    def handle_endtag(self, tag):
        if tag == 'title' and self.in_title: self.in_title = False; self.title_done = True
    def handle_data(self, d):
        if self.in_title: self.title += d

errors = []
pages = sorted(glob.glob(os.path.join(public, '**', '*.html'), recursive=True))
vscode = os.environ.get('RIFT_SITE_VSCODE') == '1'
for f in pages:
    name = os.path.relpath(f, public)
    raw = open(f, encoding='utf-8').read()
    if not vscode and re.search(r'vs ?code', raw, re.I):
        errors.append(f'{name}: mentions VS Code while the flag is off')
    if name == '404.html':
        continue
    p = Page(); p.feed(raw)
    if len(p.title) >= 60: errors.append(f'{name}: title is {len(p.title)} characters')
    if p.desc is None or not 0 < len(p.desc) < 155: errors.append(f'{name}: description is {None if p.desc is None else len(p.desc)} characters')
    if p.h1 != 1: errors.append(f'{name}: {p.h1} h1 elements')
    if not p.canonical: errors.append(f'{name}: no canonical link')
    if p.noalt: errors.append(f'{name}: {p.noalt} images without alt')
sitemap = open(os.path.join(public, 'sitemap.xml'), encoding='utf-8').read()
if not vscode and re.search(r'vs ?code', sitemap, re.I):
    errors.append('sitemap.xml mentions VS Code while the flag is off')
for e in errors: print(e, file=sys.stderr)
print(f'{len(pages)} pages checked, {len(errors)} problems')
sys.exit(1 if errors else 0)
