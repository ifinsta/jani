import json, os, re, subprocess, sys, tempfile, time, urllib.request
from playwright.sync_api import sync_playwright
ROOT = sys.argv[1]                      # jani checkout
JANI = os.path.join(ROOT, 'target/debug/jani')
FAKE = os.path.join(ROOT, 'cloud/e2e/fake_provider.py')
res, errs = [], []
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
            p = b.new_context(viewport={'width': w, 'height': h}, color_scheme=scheme).new_page()
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
                p.evaluate('location.hash="#map"'); p.wait_for_timeout(700)
                check('How it fits shows the nodes', len(p.query_selector_all('.node[data-id]')) >= 5)
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
                p.keyboard.press('Control+j'); p.wait_for_timeout(300)
                check('Ctrl+J hides the details panel', p.eval_on_selector('#rctx', 'e=>getComputedStyle(e).display') == 'none')
        b.close()
finally:
    ui.terminate(); fp.terminate()
check('no console or page errors', not errs, str(errs[:3]))
print(f'\n{sum(res)}/{len(res)} passed'); sys.exit(0 if all(res) else 1)
