const {chromium}=require('playwright');
(async()=>{
const b=await chromium.launch();const errs=[];
async function mk(w,h,scheme){const c=await b.newContext({viewport:{width:w,height:h},colorScheme:scheme||'light'});const p=await c.newPage();
 p.on('pageerror',e=>errs.push('PAGEERR '+e.message));p.on('console',m=>{if(m.type()==='error')errs.push('CONSOLE '+m.text())});
 await p.goto('file://'+process.cwd()+'/artifact.html');await p.waitForTimeout(300);return p}
const p=await mk(1366,820);
const roles=await p.$$eval('#roleSel option',o=>o.map(x=>x.value));
console.log('roles',roles);
for(const r of roles){
  await p.selectOption('#roleSel',r);
  const navs=await p.$$eval('#rail .nav',a=>a.map(x=>x.getAttribute('href')));
  for(const n of navs){await p.evaluate(h=>location.hash=h,n);await p.waitForTimeout(150);
    const len=await p.$eval('#view',v=>v.innerText.length);
    const hs=await p.evaluate(()=>document.documentElement.scrollWidth>innerWidth);
    if(len<40||hs)console.log('PROBLEM',r,n,len,hs);}
  console.log('ok',r,navs.join(','));
}
// officer run flow
await p.selectOption('#roleSel','officer');await p.evaluate(()=>location.hash='#work');await p.waitForTimeout(200);
await p.screenshot({path:'s_work0.png'});
await p.click('[data-start]');
for(let i=0;i<60;i++){await p.waitForTimeout(500);
  if(await p.$('[data-ans]')){const t=await p.$eval('[data-ans]',e=>e.dataset.text);console.log('ask',t);await p.click('[data-ans]');continue}
  if(await p.$('[data-gate]')){console.log('gate');await p.screenshot({path:'s_gate.png'});await p.click('[data-gate][data-c=all]');continue}
  if(await p.$('.card.done')){console.log('done');break}}
await p.waitForTimeout(500);await p.screenshot({path:'s_done.png'});
await p.evaluate(()=>location.hash='#proof');await p.waitForTimeout(200);await p.screenshot({path:'s_proof.png'});
await p.evaluate(()=>location.hash='#map');await p.waitForTimeout(300);await p.screenshot({path:'s_map.png'});
await p.selectOption('#roleSel','ceo');await p.evaluate(()=>location.hash='#home');await p.waitForTimeout(200);await p.screenshot({path:'s_ceo.png'});
await p.selectOption('#roleSel','region');await p.evaluate(()=>location.hash='#work');await p.waitForTimeout(200);
await p.click('[data-askq]');await p.waitForTimeout(3500);await p.screenshot({path:'s_ask.png'});
await p.selectOption('#roleSel','officer');await p.evaluate(()=>location.hash='#home');await p.waitForTimeout(200);await p.screenshot({path:'s_home.png'});await p.selectOption('#roleSel','ops');
await p.evaluate(()=>location.hash='#missions');await p.waitForTimeout(200);await p.click('#newM');await p.screenshot({path:'s_wiz.png'});
// phone dark
const m=await mk(375,740,'dark');
await m.screenshot({path:'m_home.png'});
await m.evaluate(()=>location.hash='#work');await m.waitForTimeout(300);await m.screenshot({path:'m_work.png'});
console.log('mobile hscroll',await m.evaluate(()=>document.documentElement.scrollWidth>innerWidth));
console.log('errors',errs);
await b.close()})();
