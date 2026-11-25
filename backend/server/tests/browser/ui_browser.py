import json, os, re, subprocess, sys, tempfile, time, urllib.request
from playwright.sync_api import sync_playwright
ROOT = sys.argv[1]                      # jani checkout
JANI = os.path.join(ROOT, 'target/debug/jani')
FAKE = os.path.join(ROOT, 'cloud/e2e/fake_provider.py')
res, errs = [], []
MARKDOWN = '## Findings\n\nI read `README.md` and checked **three** things:\n\n1. The *build* is fine\n2. Tests: ~~none~~ **18 pass**\n   - unit\n   - integration\n\n| File | Lines | Status |\n|:--|--:|:-:|\n| `a.rs` | 120 | ok |\n| `b.rs` | 8 | **new** |\n\n> Note: this was read only.\n\n```rust\nfn main() {\n    // say hello\n    println!("hello {}", 42);\n}\n```\n\n```diff\n+ added line\n- removed line\n```\n\n- [x] checked\n- [ ] to do\n\nSee [the docs](https://docs.example/guide) or https://example.org/a_b. Footnote here[^1].\n\n<script>window.__xss=1</script>\n[bad](javascript:window.__xss=2) ![pic](https://img.example/x.png)\n\n[^1]: The footnote text.\n'
def check(name, ok, info=''):
    res.append(bool(ok)); print(('PASS ' if ok else 'FAIL ') + name, info, flush=True)
work = tempfile.mkdtemp()
repo = os.path.join(work, 'repo'); os.makedirs(repo)
env = dict(os.environ, JANI_API_KEY='test-key', GIT_AUTHOR_NAME='t', GIT_AUTHOR_EMAIL='t@t', GIT_COMMITTER_NAME='t', GIT_COMMITTER_EMAIL='t@t')
subprocess.run('git init -q && echo "# demo" > README.md && git add -A && git commit -qm init', shell=True, cwd=repo, env=env, check=True)
fp = subprocess.Popen([sys.executable, FAKE, '18941'])
time.sleep(1)
req = urllib.request.Request('http://127.0.0.1:18941/__script', method='POST', data=json.dumps({'turns': [[
    {'tool': 'ask', 'args': {'question': 'Which folder should I start with?', 'options': ['The backend', 'The frontend']}},
    {'tool': 'command', 'args': {'argv': ['make', 'deploy'], 'timeout_secs': 20}},
    {'tool': 'write', 'args': {'path': 'out/notes.txt', 'content': 'hi'}},
    {'final': 'Done. I asked which folder, ran the build, and wrote `out/notes.txt`.'}]]}).encode())
urllib.request.urlopen(req)
ui = subprocess.Popen([JANI, 'ui', '--port', '18942', '--api-base', 'http://127.0.0.1:18941/v1', '--model', 'scripted', '--state-dir', os.path.join(work, 'state'), '--allow-write', '--write-path', 'out', '--exec', 'smart'], cwd=repo, env=env, stdout=subprocess.PIPE, text=True)
url = re.search(r'http://\S+', ui.stdout.readline()).group(0)
try:
    with sync_playwright() as pw:
        b = pw.chromium.launch(args=['--no-sandbox'])
        for w, h, scheme in [(1366, 820, 'light'), (375, 740, 'dark')]:
            p = b.new_context(viewport={'width': w, 'height': h}, color_scheme=scheme, permissions=['clipboard-read', 'clipboard-write']).new_page()
            p.on('pageerror', lambda e: errs.append(str(e)))
            p.on('console', lambda m: errs.append(m.text) if m.type == 'error' else None)
            p.goto(url); p.wait_for_timeout(800)
            nav = p.eval_on_selector_all('#rail a.nav[href]', 'a=>a.map(x=>x.getAttribute("href").slice(1))')
            bad = []
            for n in nav:
                p.evaluate('h=>location.hash=h', n); p.wait_for_timeout(500)
                if p.eval_on_selector('#view', 'v=>v.innerText.length') < 40 or p.evaluate('document.documentElement.scrollWidth>innerWidth'): bad.append(n)
            check(f'{w}x{h}-{scheme}: every page renders without sideways scroll ({len(nav)} pages)', not bad, str(bad))
            if w == 1366:
                check('local operator sees Approvals and Organisation', 'approvals' in nav and 'org' in nav)
                html = p.content(); check('page does not embed the token', url.split('token=')[1] not in html)
                p.evaluate('location.hash="#home"'); p.wait_for_timeout(300)
                p.fill('#askIn', 'Look at the project and build it.'); p.click('#askForm button[type=submit]')
                p.wait_for_selector('[data-qa]', timeout=20000); check('Jani asks a question card', True)
                p.click('[data-qa="0"]')
                p.wait_for_selector('[data-consent]', timeout=20000); check('command consent card appears before anything runs', not os.path.exists(os.path.join(repo, 'out/notes.txt')))
                p.click('[data-consent="once"]')
                for _ in range(60):
                    if p.query_selector('.k-jani .t-txt'): break
                    p.wait_for_timeout(300)
                said = p.eval_on_selector_all('.k-jani .t-txt', 'e=>e.map(x=>x.textContent)')
                check('turn finishes with Jani’s answer', any('notes.txt' in s for s in said), str(said))
                check('the approved write landed in the worktree', os.path.exists(os.path.join(repo, 'out/notes.txt')))
                p.evaluate('location.hash="#proof"'); p.wait_for_timeout(900)
                txt = p.inner_text('#view'); check('Proof shows the record verified with changes and a decision', 'Record verified' in txt and 'changed a file' in txt.lower())
                # Proof can be taken away in four forms: a readable report, Markdown, a spreadsheet and the verification file.
                p.wait_for_selector('[data-ev]', timeout=10000); p.click('[data-ev]'); p.wait_for_timeout(200)
                names = p.eval_on_selector_all('#dlMenu .mi b', 'e=>e.map(x=>x.textContent)')
                check('Download offers a report, Markdown, a spreadsheet and a verification file', names == ['Report', 'Markdown', 'Spreadsheet', 'Verification file'], str(names))
                with p.expect_download() as dl:
                    p.click('#dlMenu [data-kind="md"]')
                md_text = open(dl.value.path(), encoding='utf-8').read()
                check('the Markdown report reads in plain words', md_text.startswith('# Record of work') and '## What happened' in md_text and '| Steps taken |' in md_text and 'Ran a command' in md_text and 'Changed a file' in md_text and dl.value.suggested_filename.endswith('-report.md'))
                p.click('[data-ev]')
                with p.expect_download() as dl:
                    p.click('#dlMenu [data-kind="csv"]')
                csv_text = open(dl.value.path(), encoding='utf-8').read()
                check('the spreadsheet has a header and one row per step', csv_text.startswith('\ufefftime_utc,time_local,step,result') and len(csv_text.strip().split('\r\n')) >= 5 and dl.value.suggested_filename.endswith('-timeline.csv'))
                p.click('[data-ev]')
                with p.expect_download() as dl:
                    p.click('#dlMenu [data-kind="json"]')
                ev = json.load(open(dl.value.path(), encoding='utf-8'))
                check('the verification file is the full evidence export', ev.get('schemaVersion', '').startswith('jani.run-evidence-export') and 'receipt' in ev and len(ev.get('events', [])) >= 5)
                p.click('[data-ev]')
                with p.context.expect_page() as pg:
                    p.click('#dlMenu [data-kind="report"]')
                rep = pg.value; rep.wait_for_load_state(); rep.wait_for_timeout(400)
                check('the report opens as a readable, styled page with a print button', rep.query_selector('h1') is not None and rep.query_selector('#print') is not None and rep.eval_on_selector('body', 'e=>getComputedStyle(e).fontSize') == '14px' and 'Record of work' in rep.inner_text('body') and 'verified' in rep.inner_text('body'))
                rep.evaluate('window.__n=0;window.print=()=>{window.__n++}'); rep.click('#print')
                check('the report’s print button opens the print dialog', rep.evaluate('window.__n') == 1)
                rep.close()
                p.evaluate('location.hash="#map"'); p.wait_for_timeout(700)
                check('How it fits shows the nodes', len(p.query_selector_all('.node[data-id]')) >= 5)
                # Markdown: a scripted answer that uses every construct, plus things that must never run.
                urllib.request.urlopen(urllib.request.Request('http://127.0.0.1:18941/__script', method='POST', data=json.dumps({'turns': [[{'final': MARKDOWN}]]}).encode()))
                p.evaluate('location.hash="#work"'); p.wait_for_timeout(400)
                p.click('#tIn'); p.keyboard.type('show me markdown'); p.keyboard.press('Enter')
                p.wait_for_selector('.k-jani .md h2', timeout=20000); p.wait_for_timeout(400)
                r = p.evaluate("""()=>{const m=[...document.querySelectorAll('.k-jani .md')].pop();return {h2:!!m.querySelector('h2'),ol:m.querySelectorAll('ol>li').length,nested:!!m.querySelector('ol ul li'),tbl:m.querySelectorAll('table tr').length,right:getComputedStyle(m.querySelector('tbody td:nth-child(2)')).textAlign,hl:!!m.querySelector('pre code .tk-k'),diff:!!m.querySelector('.td-add')&&!!m.querySelector('.td-del'),task:m.querySelectorAll('input[type=checkbox]').length,checked:m.querySelectorAll('input:checked').length,bq:!!m.querySelector('blockquote'),del:!!m.querySelector('del'),script:!!m.querySelector('script'),xss:window.__xss||0,jslink:[...m.querySelectorAll('a')].some(a=>/^javascript/i.test(a.getAttribute('href'))),imgs:m.querySelectorAll('img').length,ext:[...m.querySelectorAll('a[href^="http"]')].every(a=>a.rel.includes('noopener')&&a.target==='_blank'),links:m.querySelectorAll('a[href^="http"]').length,fn:!!m.querySelector('.mdfn li'),escaped:m.textContent.includes('<script>')}}""")
                check('markdown: headings, lists (nested), emphasis, quotes and rules render', r['h2'] and r['ol'] >= 2 and r['nested'] and r['bq'] and r['del'], str(r))
                check('markdown: tables keep their alignment', r['tbl'] == 3 and r['right'] == 'right')
                check('markdown: code is highlighted, diffs are coloured, task lists show state', r['hl'] and r['diff'] and r['task'] == 2 and r['checked'] == 1)
                check('markdown: links are safe and outside images are not loaded', r['ext'] and r['links'] == 3 and not r['jslink'] and r['imgs'] == 0)
                check('markdown: raw HTML is shown as text and never runs', r['escaped'] and not r['script'] and r['xss'] == 0)
                check('markdown: footnotes are collected at the end', r['fn'])
                p.click('.mdcopy'); p.wait_for_timeout(300)
                check('markdown: the Copy button copies the code', p.inner_text('.mdcopy') == 'Copied' and p.evaluate('navigator.clipboard.readText()').startswith('fn main()'))

                check('status bar shows the record, authority and model', all(t in p.inner_text('#status') for t in ['Record verified', 'Can edit files', 'scripted']))
                p.keyboard.press('Control+k'); p.keyboard.type('proof'); p.keyboard.press('Enter'); p.wait_for_timeout(500)
                check('the palette jumps to a page', p.evaluate('location.hash') == '#proof')
                p.keyboard.press('Control+b'); p.wait_for_timeout(200)
                collapsed = p.evaluate('document.documentElement.getAttribute("data-sidebar")') == 'collapsed' and p.eval_on_selector('#rail', 'e=>e.getBoundingClientRect().width') < 60
                p.keyboard.press('Control+b'); p.wait_for_timeout(200)
                check('Ctrl+B collapses the sidebar to an icon strip and back', collapsed and p.eval_on_selector('#rail', 'e=>e.getBoundingClientRect().width') > 150)
                p.click('#acctMenuBtn'); p.wait_for_timeout(150)
                check('account menu offers theme, density and width', all(t in p.inner_text('#acctMenu').lower() for t in ['theme', 'density', 'conversation width']))
                p.keyboard.press('Escape')
                p.evaluate('location.hash="#work"'); p.wait_for_timeout(400)
                w = p.eval_on_selector('#view', 'e=>e.getBoundingClientRect().width'); full = p.evaluate('innerWidth')
                check('the working area takes most of the screen', w / full > 0.8, f'{w:.0f} of {full}')
                check('Search sits in the sidebar', p.query_selector('#sbSearch') is not None)
                check('light mode is pure white behind the sidebar and the content', p.evaluate('[".sb",".main","#status"].every(s=>getComputedStyle(document.querySelector(s)).backgroundColor==="rgb(255, 255, 255)")'))
                icons = p.eval_on_selector_all('#rail a.nav[href] use', 'u=>u.map(x=>x.getAttribute("href"))')
                check('every sidebar page has its own icon', len(icons) == len(set(icons)), str(icons))
                check('there is no header bar, so content starts at the top', p.query_selector('header') is None and p.eval_on_selector('#view', 'e=>e.getBoundingClientRect().top') < 4)
                check('the details panel starts closed', p.eval_on_selector('#rctx', 'e=>getComputedStyle(e).display') == 'none')
                p.keyboard.press('Control+j'); p.wait_for_timeout(300)
                check('Ctrl+J opens the details panel', p.eval_on_selector('#rctx', 'e=>getComputedStyle(e).display') != 'none')
                p.evaluate('location.hash="#plugins"'); p.wait_for_timeout(700)
                check('Plugins page explains how to add one when none are set up', 'No plugins are set up' in p.inner_text('#view') and 'JANI_PLUGIN_DIR' in p.inner_text('#view'))
                p.evaluate('location.hash="#work"'); p.wait_for_timeout(500)
                pill = p.evaluate('(()=>{const e=document.querySelector("#pill");const c=getComputedStyle(e);return {r:parseFloat(c.borderTopLeftRadius),h:e.getBoundingClientRect().height,ph:document.querySelector("#tIn").placeholder,plus:!!document.querySelector("#plusBtn"),mic:!document.querySelector("#micBtn").hidden,send:document.querySelector("#sendBtn").hidden}})()')
                check('the composer is a rounded pill with a plus, a mic and the commands hint', pill['r'] >= 24 and pill['h'] >= 48 and pill['plus'] and pill['mic'] and pill['send'] and pill['ph'] == 'Type / for commands', str(pill))
                p.click('#tIn'); p.keyboard.type('/'); p.wait_for_timeout(200)
                check('typing / opens the command menu', '/help' in p.inner_text('#slash') and not p.eval_on_selector('#slash', 'e=>e.hidden'))
                p.keyboard.press('Escape'); p.fill('#tIn', 'hello'); p.wait_for_timeout(100)
                check('the mic gives way to Send once there is text', p.eval_on_selector('#micBtn', 'e=>e.hidden') and not p.eval_on_selector('#sendBtn', 'e=>e.hidden'))
                p.fill('#tIn', '')
                bad = p.evaluate('(()=>{const b=[];for(const s of document.styleSheets)for(const r of s.cssRules){const o=r.style&&r.style.outlineOffset;if(o&&o!=="0px"&&o!=="0")b.push(r.selectorText+":"+o)}return b})()')
                check('no focus outline has an offset', not bad, str(bad))
                # background refreshes must not rebuild the page or eat what someone is typing
                p.evaluate('location.hash="#home"'); p.wait_for_timeout(600)
                p.fill('#askIn', 'half-typed thought')
                p.evaluate('(()=>{document.querySelector(".stats").__mark=1;window.__mut=0;new MutationObserver(m=>window.__mut+=m.length).observe(document.querySelector("#view"),{childList:true,subtree:true})})()')
                p.wait_for_timeout(9500)
                kept = p.evaluate('({stats:!!document.querySelector(".stats").__mark,mut:window.__mut,text:document.querySelector("#askIn").value})')
                check('Today does not refresh or rebuild itself while idle, and keeps what was typed', kept['stats'] and kept['mut'] == 0 and kept['text'] == 'half-typed thought', str(kept))
        b.close()
finally:
    ui.terminate(); fp.terminate()
check('no console or page errors', not errs, str(errs[:3]))
print(f'\n{sum(res)}/{len(res)} passed'); sys.exit(0 if all(res) else 1)
