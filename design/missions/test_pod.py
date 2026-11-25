import sys, json
from playwright.sync_api import sync_playwright
URL = 'file:///work/artifact.html'
errs, res = [], []
def check(name, ok, info=''):
    res.append((name, bool(ok), info)); print(('PASS ' if ok else 'FAIL ') + name, info)
with sync_playwright() as pw:
    b = pw.chromium.launch(args=['--no-sandbox'])
    def mk(w, h, scheme='light'):
        c = b.new_context(viewport={'width': w, 'height': h}, color_scheme=scheme)
        p = c.new_page()
        p.on('pageerror', lambda e: errs.append('PAGEERR ' + str(e)))
        p.on('console', lambda m: errs.append('CONSOLE ' + m.text) if m.type == 'error' else None)
        p.goto(URL); p.wait_for_timeout(300); return p
    for w, h, scheme in [(1366, 820, 'light'), (1366, 820, 'dark'), (375, 740, 'light'), (375, 740, 'dark')]:
        p = mk(w, h, scheme); tag = f'{w}x{h}-{scheme}'
        roles = p.eval_on_selector_all('#roleSel option', 'o=>o.map(x=>x.value)')
        check(f'{tag} 7 roles', len(roles) == 7, str(len(roles)))
        bad = []
        for r in roles:
            p.select_option('#roleSel', r)
            navs = p.eval_on_selector_all('#rail .nav', 'a=>a.map(x=>x.getAttribute("href"))')
            for n in navs:
                p.evaluate('h=>location.hash=h', n); p.wait_for_timeout(120)
                ln = p.eval_on_selector('#view', 'v=>v.innerText.length')
                hs = p.evaluate('document.documentElement.scrollWidth>innerWidth')
                if ln < 40 or hs: bad.append((r, n, ln, hs))
        check(f'{tag} all role pages render, no horizontal scroll', not bad, str(bad))
        p.context.close()
    # functional flows on desktop
    p = mk(1366, 820)
    p.select_option('#roleSel', 'officer'); p.evaluate('location.hash="#work"'); p.wait_for_timeout(200)
    p.click('[data-start]'); asked = gated = done = False
    for _ in range(80):
        p.wait_for_timeout(400)
        if p.query_selector('[data-ans]'): asked = True; p.click('[data-ans]'); continue
        if p.query_selector('[data-gate]'): gated = True; p.click('[data-gate][data-c=all]'); continue
        if p.query_selector('.card.done'): done = True; break
    check('officer run: asks a question', asked); check('officer run: stops at gate and accepts decision', gated); check('officer run: finishes with result card', done)
    p.evaluate('location.hash="#proof"'); p.wait_for_timeout(200)
    check('proof records the decision', 'decision' in p.inner_text('#view').lower())
    for r in ['accountant', 'analyst', 'cs']:
        p.select_option('#roleSel', r); p.evaluate('location.hash="#work"'); p.wait_for_timeout(200)
        p.click('[data-start]'); d = False; ok = False
        for _ in range(100):
            p.wait_for_timeout(400)
            if p.query_selector('[data-ans]'): p.click('[data-ans]'); continue
            if p.query_selector('[data-gate]'): p.click('[data-gate][data-c=small]'); continue
            if p.query_selector('.card.done'): d = True; break
        check(f'{r} run completes', d)
    for r in ['region', 'ceo']:
        p.select_option('#roleSel', r); p.evaluate('location.hash="#work"'); p.wait_for_timeout(200)
        p.click('[data-askq]'); p.wait_for_timeout(4000)
        check(f'{r} ask mode answers with source chips', p.query_selector('.cite') is not None or p.query_selector('.card') is not None)
    p.select_option('#roleSel', 'ops'); p.evaluate('location.hash="#missions"'); p.wait_for_timeout(200)
    p.click('#newM'); p.fill('#wn', 'Loan file checks')
    for _ in range(5): p.click('#wn2'); p.wait_for_timeout(80)
    p.click('#wn2'); p.wait_for_timeout(200)
    check('wizard sends mission for sign-off', 'Loan file checks' in p.inner_text('#view') and 'Waiting for sign-off' in p.inner_text('#view'))
    p.evaluate('location.hash="#map"'); p.wait_for_timeout(300)
    check('map shows You are here', 'You are here' in p.inner_text('#view'))
    p.click('#themeBtn'); p.wait_for_timeout(100)
    check('theme toggle sets data-theme', p.evaluate('document.documentElement.getAttribute("data-theme")') in ('light', 'dark'))
    check('no html gradients', 'gradient' not in open('/work/artifact.html').read())
    b.close()
check('no console or page errors', not errs, str(errs[:5]))
fails = [r for r in res if not r[1]]
print(f'\n{len(res)-len(fails)}/{len(res)} passed'); sys.exit(1 if fails else 0)
