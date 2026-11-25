// Unit tests for the markdown renderer inside backend/server/src/ui/index.html. Run: node backend/server/tests/browser/markdown_unit.js
const fs=require('fs');const path=require('path');
const page=fs.readFileSync(path.join(__dirname,'../../src/ui/index.html'),'utf8');
const a=page.indexOf('/* ---------- markdown:'),z=page.indexOf('/* ---------- helpers for pages');
if(a<0||z<a){console.error('markdown block not found in index.html');process.exit(2)}
const esc=s=>String(s==null?'':s).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'})[c]);
eval(page.slice(a,z)+';global.md=md;global.mdHl=mdHl');
let fail=0;function t(name,src,...has){const h=md(src);for(const x of has){const ok=x instanceof RegExp?x.test(h):h.includes(x);if(!ok){fail++;console.log('FAIL',name,'missing',String(x),'\n   got:',h.slice(0,400))}}}
function no(name,src,...bad){const h=md(src);for(const x of bad){if(x instanceof RegExp?x.test(h):h.includes(x)){fail++;console.log('FAIL',name,'must not contain',String(x),'\n   got:',h.slice(0,400))}}}
t('headings','# One\n## Two\n### Three','<h1>One</h1>','<h2>Two</h2>','<h3>Three</h3>');
t('setext','Title\n=====\n\nSub\n---','<h1>Title</h1>','<h2>Sub</h2>');
t('emphasis','*a* **b** ***c*** ~~d~~ _e_ __f__','<em>a</em>','<strong>b</strong>','<strong><em>c</em></strong>','<del>d</del>','<em>e</em>','<strong>f</strong>');
t('snake_case not em','use snake_case_name here','snake_case_name');
no('snake_case not em','use snake_case_name here','<em>');
t('inline code','`a<b>` and ``x ` y``','<code>a&lt;b&gt;</code>','<code>x ` y</code>');
t('links','[x](https://a.example/p?q=1 "T") and <https://b.example> and https://c.example/x.','href="https://a.example/p?q=1"','title="T"','href="https://b.example"','href="https://c.example/x"');
t('autolink trailing punct','see https://c.example/x, ok','href="https://c.example/x"');
t('ref links','[a][r] and [r]\n\n[r]: https://r.example "Ref"','href="https://r.example"');
t('code fence','```rust\nfn main() { println!("hi"); }\n```','mdcode','tk-k','>rust<','mdcopy');
t('tilde fence + nested backticks','~~~\n```\ninner\n```\n~~~','inner','mdcode');
t('unterminated fence','```py\nprint(1)','mdcode','print');
t('indented code','    a\n    b','mdcode');
t('quote','> a\n> b\n>\n> c','<blockquote>');
t('nested quote','> a\n>> b','<blockquote>','<blockquote><p>a');
t('ul','- a\n- b\n  - c\n- d','<ul>','<li>a','<ul><li>c');
t('ol start','3. a\n4. b','<ol start="3">');
t('ol paren','1) a\n2) b','<ol>');
t('task list','- [x] done\n- [ ] todo','class="task"','checked');
t('loose list','- a\n\n- b','<p>a</p>');
t('tight list no p','- a\n- b','<li>a</li>');
t('list with code','- item\n\n  ```js\n  let x = 1\n  ```\n- next','mdcode','let');
t('table','| A | B |\n|:--|--:|\n| 1 | 2 |\n| 3 \\| x | 4 |','<table>','text-align:left','text-align:right','3 | x');
t('table no outer pipes','A | B\n--|--\n1 | 2','<table>');
t('hr','a\n\n---\n\nb','<hr>');
t('hard break','a  \nb\\\nc','<br>');
t('footnote','Text[^1] more.\n\n[^1]: The note.','mdfnref','class="mdfn"','The note.');
t('escapes','\\*not em\\* and \\# nothing','*not em*');
no('escapes','\\*not em\\*','<em>');
t('data image','![x](data:image/png;base64,AAAA)','<img','data:image/png');
t('remote image placeholder','![logo](https://e.example/a.png)','mdimgph','Image:');
no('remote image never loads','![logo](https://e.example/a.png)','<img');
// XSS
no('script tag','<script>alert(1)</script>','<script');
t('script tag text','<script>alert(1)</script>','&lt;script&gt;');
no('js link','[x](javascript:alert(1))','href="javascript','href=javascript');
no('js link caps','[x](JaVaScRiPt:alert(1))','href="JaVa','href="java');
no('data link','[x](data:text/html;base64,PHNjcmlwdD4=)','href="data');
no('data svg image','![x](data:image/svg+xml;base64,AAAA)','<img');
no('onerror','![x" onerror="alert(1)](https://e.example/a.png)','onerror="alert');
no('attr breakout','[x](https://a.example/"onmouseover="alert(1))','onmouseover="alert');
no('html entity breakout','<img src=x onerror=alert(1)>','<img');
no('autolink js','<javascript:alert(1)>','href="javascript');
no('hash link no','[toc](#top)','href="#');
no('link text html','[<b>x</b>](https://a.example)','<b>');
no('title breakout','[x](https://a.example "a\\" onclick=\\"alert(1)")','onclick="alert');
t('rel noopener','[x](https://a.example)','rel="noopener noreferrer nofollow"','target="_blank"');
t('mailto','[m](mailto:a@b.example)','href="mailto:a@b.example"');
t('large input safe',('x'.repeat(100)+'\n').repeat(500),'<p>');
t('huge input plain','y'.repeat(90000),'mdplain');
t('emoji + unicode','Héllo 🌍 **ünï**','<strong>ünï</strong>');
t('mixed real answer','## Summary\n\nI read `README.md` and found:\n\n1. **Two** modules\n2. A _small_ test suite\n\n| File | Lines |\n|---|---|\n| a.rs | 10 |\n\n> Note: read only.\n\n```bash\nif true; then echo hi; fi\n```','<h2>Summary</h2>','<code>README.md</code>','<ol>','<table>','<blockquote>','tk-k');
t('diff','```diff\n+ added\n- removed\n@@ -1 +1 @@\n```','td-add','td-del','td-hunk');
t('json','```json\n{"a": 1, "b": true}\n```','tk-t','tk-n','tk-l');
t('html highlight','```html\n<div class="x">hi</div>\n```','tk-k','tk-s');
const slow=Date.now();md(('*a '.repeat(2000))+'\n'+('[x'.repeat(2000)));if(Date.now()-slow>1500){fail++;console.log('FAIL slow pathological input',Date.now()-slow)}
console.log(fail?fail+' FAILED':'all markdown tests passed');process.exit(fail?1:0);
