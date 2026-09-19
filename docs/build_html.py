# -*- coding: utf-8 -*-
"""Render ai-panner-tuning*.md into standalone styled HTML reading copies.

Deliberately a converter, not a copy: re-run it after editing the
Markdown so the two never drift apart.

The generated panner-*.html are gitignored - they are build output,
not source. Regenerate them on any machine with:

    python docs/build_html.py

No dependencies beyond the Python standard library.
"""
import io
import os
import re
import html

# This script lives in docs/, alongside both its inputs and its outputs.
DOCS_DIR = os.path.dirname(os.path.abspath(__file__))
OUT = DOCS_DIR

DOCS = [
    ('ai-panner-tuning.nl.md', 'panner-nl.html', 'nl'),
    ('ai-panner-tuning.md', 'panner-en.html', 'en'),
]


def slug(text):
    return re.sub(r'[^a-z0-9 -]', '', text.lower()).replace(' ', '-')


def inline(t):
    """Inline markdown -> html. Code spans are protected from other rules."""
    spans = []

    def stash(m):
        spans.append(m.group(1))
        return '\x00%d\x00' % (len(spans) - 1)

    t = re.sub(r'`([^`]+)`', stash, t)
    t = html.escape(t, quote=False)
    t = re.sub(r'\[([^\]]+)\]\(([^)]+)\)', r'<a href="\2">\1</a>', t)
    t = re.sub(r'\*\*([^*]+)\*\*', r'<strong>\1</strong>', t)
    t = re.sub(r'(?<![*\w])\*([^*]+)\*(?![*\w])', r'<em>\1</em>', t)
    return re.sub(r'\x00(\d+)\x00',
                  lambda m: '<code>%s</code>' % html.escape(spans[int(m.group(1))]),
                  t)


def cells(line):
    return [c.strip() for c in line.strip().strip('|').split('|')]


def convert(md):
    lines = md.split('\n')
    out, i, toc = [], 0, []
    while i < len(lines):
        ln = lines[i]

        if ln.startswith('```'):
            block = []
            i += 1
            while i < len(lines) and not lines[i].startswith('```'):
                block.append(html.escape(lines[i]))
                i += 1
            out.append('<pre>%s</pre>' % '\n'.join(block))
            i += 1
            continue

        if ln.strip() == '<details>':
            summ = ''
            i += 1
            if i < len(lines):
                m = re.match(r'<summary>(.*)</summary>', lines[i].strip())
                if m:
                    summ = inline(m.group(1))
                    i += 1
            body = []
            while i < len(lines) and lines[i].strip() != '</details>':
                body.append(lines[i])
                i += 1
            i += 1
            inner = convert('\n'.join(body))[0]
            out.append('<details><summary>%s</summary>'
                       '<div class="dbody">%s</div></details>' % (summ, inner))
            continue

        m = re.match(r'^(#{1,3}) (.+)$', ln)
        if m:
            lvl, txt = len(m.group(1)), m.group(2)
            s = slug(txt)
            if lvl == 2:
                toc.append((s, txt))
            out.append('<h%d id="%s">%s</h%d>' % (lvl, s, inline(txt), lvl))
            i += 1
            continue

        if (ln.startswith('|') and i + 1 < len(lines)
                and re.match(r'^\|[\s:|-]+\|$', lines[i + 1])):
            head = cells(ln)
            i += 2
            rows = []
            while i < len(lines) and lines[i].startswith('|'):
                rows.append(cells(lines[i]))
                i += 1
            t = ['<div class="tw"><table><thead><tr>']
            t += ['<th>%s</th>' % inline(c) for c in head]
            t.append('</tr></thead><tbody>')
            for r in rows:
                t.append('<tr>' + ''.join('<td>%s</td>' % inline(c) for c in r) + '</tr>')
            t.append('</tbody></table></div>')
            out.append(''.join(t))
            continue

        if ln.startswith('> '):
            q = []
            while i < len(lines) and lines[i].startswith('>'):
                q.append(lines[i].lstrip('>').strip())
                i += 1
            out.append('<blockquote>%s</blockquote>' % inline(' '.join(q)))
            continue

        if re.match(r'^\s*[-*] ', ln) or re.match(r'^\s*\d+\. ', ln):
            ordered = bool(re.match(r'^\s*\d+\. ', ln))
            items, cur = [], None
            while i < len(lines):
                l2 = lines[i]
                m2 = re.match(r'^\s*(?:[-*]|\d+\.) (.*)$', l2)
                if m2:
                    if cur is not None:
                        items.append(cur)
                    cur = m2.group(1)
                elif (l2.strip() and (l2.startswith('  ') or l2.startswith('\t'))
                        and cur is not None):
                    cur += ' ' + l2.strip()
                else:
                    break
                i += 1
            if cur is not None:
                items.append(cur)
            tag = 'ol' if ordered else 'ul'
            out.append('<%s>%s</%s>'
                       % (tag, ''.join('<li>%s</li>' % inline(x) for x in items), tag))
            continue

        if ln.strip() in ('---', '***'):
            out.append('<hr>')
            i += 1
            continue

        if not ln.strip():
            i += 1
            continue

        para = []
        stop = r'^(#{1,3} |\||> |```|---|\s*[-*] |\s*\d+\. |<details>)'
        while (i < len(lines) and lines[i].strip()
                and not re.match(stop, lines[i])):
            para.append(lines[i])
            i += 1
        if para:
            out.append('<p>%s</p>' % inline(' '.join(x.strip() for x in para)))
    return '\n'.join(out), toc


CSS = """
:root{--bg:#faf9f7;--panel:#fff;--ink:#1b1b1d;--dim:#5d5d66;--line:#e3e1dd;
--accent:#1f6f4a;--accent-soft:#e8f2ec;--warn:#8a5a00;--warn-soft:#fdf5e3;--code:#f2f1ee}
@media(prefers-color-scheme:dark){:root:not([data-theme=light]){
--bg:#16161a;--panel:#1e1e24;--ink:#e9e8e6;--dim:#a0a0ab;--line:#31313a;
--accent:#69c096;--accent-soft:#1d2f27;--warn:#e0b25f;--warn-soft:#2e2718;--code:#26262d}}
:root[data-theme=dark]{--bg:#16161a;--panel:#1e1e24;--ink:#e9e8e6;--dim:#a0a0ab;
--line:#31313a;--accent:#69c096;--accent-soft:#1d2f27;--warn:#e0b25f;
--warn-soft:#2e2718;--code:#26262d}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--ink);
font:16px/1.65 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;
-webkit-font-smoothing:antialiased}
.wrap{max-width:1180px;margin:0 auto;padding:0 16px;display:flex;gap:40px;
align-items:flex-start}
nav{position:sticky;top:0;width:230px;flex:0 0 230px;padding:40px 0;
max-height:100vh;overflow-y:auto}
nav p{font-size:11px;letter-spacing:.09em;text-transform:uppercase;color:var(--dim);
margin:0 0 12px}
nav a{display:block;padding:6px 10px;color:var(--dim);text-decoration:none;
font-size:13.5px;border-radius:6px;border-left:2px solid transparent;line-height:1.35}
nav a:hover{color:var(--ink);background:var(--panel)}
nav a.on{color:var(--accent);border-left-color:var(--accent);
background:var(--accent-soft);font-weight:600}
main{flex:1;min-width:0;padding:40px 0 100px;max-width:790px}
h1{font-size:30px;line-height:1.25;margin:0 0 6px;letter-spacing:-.02em}
h2{font-size:21px;margin:52px 0 14px;padding-top:14px;border-top:1px solid var(--line);
letter-spacing:-.01em;scroll-margin-top:20px}
h2:first-of-type{border-top:none;margin-top:30px}
h3{font-size:16.5px;margin:26px 0 8px}
p{margin:0 0 14px}
a{color:var(--accent)}
code{background:var(--code);padding:.13em .38em;border-radius:4px;
font:13px ui-monospace,"Cascadia Code",Consolas,monospace;word-break:break-word}
pre{background:var(--code);padding:14px 16px;border-radius:9px;overflow-x:auto;
font:12.5px/1.6 ui-monospace,Consolas,monospace;border:1px solid var(--line)}
pre code{background:none;padding:0}
.tw{overflow-x:auto;margin:0 0 18px;border:1px solid var(--line);border-radius:9px;
background:var(--panel)}
table{border-collapse:collapse;width:100%;font-size:14px}
th{text-align:left;padding:10px 13px;background:var(--code);font-size:11.5px;
letter-spacing:.05em;text-transform:uppercase;color:var(--dim);font-weight:600;
white-space:nowrap}
td{padding:10px 13px;border-top:1px solid var(--line);vertical-align:top}
td:first-child{font-weight:500}
tbody tr:hover{background:var(--accent-soft)}
blockquote{margin:0 0 18px;padding:13px 16px;background:var(--warn-soft);
border-left:3px solid var(--warn);border-radius:0 8px 8px 0;font-size:14.5px}
details{margin:0 0 12px;border:1px solid var(--line);border-radius:9px;
background:var(--panel);overflow:hidden}
summary{cursor:pointer;padding:12px 16px;font-weight:600;font-size:14.5px;
list-style:none;display:flex;align-items:center;gap:9px;user-select:none}
summary::-webkit-details-marker{display:none}
summary::before{content:"";width:0;height:0;border-left:5px solid var(--dim);
border-top:4px solid transparent;border-bottom:4px solid transparent;
transition:transform .15s;flex:0 0 auto}
details[open] summary::before{transform:rotate(90deg)}
summary:hover{background:var(--code)}
.dbody{padding:2px 16px 14px;border-top:1px solid var(--line);font-size:14.5px}
.dbody>*:first-child{margin-top:12px}
ul,ol{margin:0 0 15px;padding-left:22px}
li{margin:0 0 7px}
hr{border:none;height:1px;background:var(--line);margin:34px 0}
.sub{color:var(--dim);font-size:14px;margin-bottom:26px}
.bar{display:flex;gap:8px;align-items:center;margin:0 0 4px;flex-wrap:wrap}
button{font:inherit;font-size:12.5px;padding:5px 11px;border-radius:7px;
cursor:pointer;border:1px solid var(--line);background:var(--panel);color:var(--dim)}
button:hover{color:var(--ink)}
@media(max-width:940px){nav{display:none}.wrap{padding:0 16px}main{padding:26px 0 70px}}
@media print{nav,.bar{display:none}main{max-width:none;padding:0}
details{break-inside:avoid}.tw{break-inside:avoid}}
"""

JS = """
var links=[].slice.call(document.querySelectorAll('nav a'));
var heads=links.map(function(a){return document.getElementById(a.hash.slice(1))});
function mark(){var y=window.scrollY+90,idx=0;
heads.forEach(function(h,i){if(h&&h.offsetTop<=y)idx=i});
links.forEach(function(a,i){a.classList.toggle('on',i===idx)})}
document.addEventListener('scroll',mark,{passive:true});mark();
document.getElementById('exp').addEventListener('click',function(){
var d=document.querySelectorAll('details'),open=this.dataset.s!=='1';
d.forEach(function(x){x.open=open});this.dataset.s=open?'1':'0';
this.textContent=open?T.collapse:T.expand});
document.getElementById('thm').addEventListener('click',function(){
var d=document.documentElement,now=d.getAttribute('data-theme');
if(!now){now=matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light'}
d.setAttribute('data-theme',now==='dark'?'light':'dark')});
"""

T = {
    'nl': {'expand': 'Alles uitklappen', 'collapse': 'Alles inklappen',
           'theme': 'Licht / donker', 'toc': 'Inhoud',
           'sub': 'Werkversie van docs/ai-panner-tuning.nl.md - gegenereerd, '
                  'bewerk het Markdown-bestand en draai build.py opnieuw'},
    'en': {'expand': 'Expand all', 'collapse': 'Collapse all',
           'theme': 'Light / dark', 'toc': 'Contents',
           'sub': 'Working copy of docs/ai-panner-tuning.md - generated, '
                  'edit the Markdown and re-run build.py'},
}

PAGE = """<!DOCTYPE html><html lang="%s"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>%s</title><style>%s</style></head><body><div class="wrap">
<nav><p>%s</p>%s</nav><main>
<h1>%s</h1><p class="sub">%s</p>
<div class="bar"><button id="exp" data-s="0">%s</button><button id="thm">%s</button>
<a href="%s" style="font-size:12.5px;margin-left:auto">%s</a></div>
%s</main></div><script>var T=%s;%s</script></body></html>"""


def main():
    for src, dst, lang in DOCS:
        path = os.path.join(DOCS_DIR, src)
        if not os.path.exists(path):
            print('overgeslagen (niet gevonden):', path)
            continue
        md = io.open(path, encoding='utf-8').read()
        title = re.search(r'^# (.+)$', md, re.M).group(1)
        md = re.sub(r'^# .+\n', '', md, count=1)
        body, toc = convert(md)
        t = T[lang]
        nav = ''.join('<a href="#%s">%s</a>' % (s, html.escape(x)) for s, x in toc)
        other = 'panner-en.html' if lang == 'nl' else 'panner-nl.html'
        page = PAGE % (
            lang, html.escape(title), CSS, t['toc'], nav, html.escape(title),
            t['sub'], t['expand'], t['theme'], other,
            'English' if lang == 'nl' else 'Nederlands',
            body, repr(t).replace("'", '"'), JS)
        io.open(os.path.join(OUT, dst), 'w', encoding='utf-8').write(page)
        print('geschreven: %s (%d secties)' % (dst, len(toc)))


if __name__ == '__main__':
    main()
