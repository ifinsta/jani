(function(){
'use strict';
var D=window.__D,S=window.__S,E=window.__E,$=D.$,$$=D.$$,esc=D.esc,ico=D.ico,reduce=D.reduce,store=D.store,ROLES=D.ROLES,NAVS=D.NAVS,TITLES=D.TITLES,TASKS=D.TASKS,TEAMS=D.TEAMS,MAPS=D.MAPS,SC=D.SC,ASKS=D.ASKS,PROOF=D.PROOF,BRANCH=D.BRANCH,toast=window.__toast;
var view=$('#view');
function role(){return ROLES[S.role]}
function first(n){return n.split(' ')[0]}
function stCls(w){return /Needs you|Behind|Over capacity|Stopped/.test(w)?'warn':/Done|On track|Has capacity|Connected|Verified|Running by|Allowed/.test(w)?'ok':/Running|Working/.test(w)?'info live':''}
function st(w,extra){var c=stCls(w);if(extra)c=extra;return '<span class="st '+c+'">'+esc(w)+'</span>'}
var RUNMAP={ready:'Ready',running:'Running',needs:'Needs you',paused:'Paused',stopped:'Stopped',done:'Done','ask':'Ready','needs-ask':'Needs you'};
function runOf(t){return t.script?S.runs[t.script]:null}
function tState(t){var r=runOf(t);return r?RUNMAP[r.state]:t.state}
function myTasks(){return TASKS.filter(function(t){return t.role===S.role})}
function needsCount(){var n=0;myTasks().forEach(function(t){if(tState(t)==='Needs you')n++});if(S.role==='cs'||S.role==='ops'||S.role==='region')n+=0;return n}
function goto(p){location.hash='#'+p}
function sampleTag(){return '<span class="sample">'+ico('info')+'Sample data</span>'}

/* ---------- shell ---------- */
function renderRail(){
  var r=role(),n=needsCount(),h='<div class="brand"><span class="mark">'+ico('star')+'</span>Jani</div><div class="rail-group">Your space</div>';
  r.nav.forEach(function(k){
    var a=NAVS[k],badge=(k==='tasks'||(k==='home'&&!r.nav.includes('tasks')))&&n?'<span class="n" aria-label="'+n+' need you">'+n+'</span>':'';
    h+='<a class="nav" href="#'+k+'" '+(S.page===k?'aria-current="page"':'')+'>'+ico(a[1])+'<span>'+a[0]+'</span>'+badge+'</a>';
  });
  h+='<a class="nav" href="#about" '+(S.page==='about'?'aria-current="page"':'')+' style="margin-top:8px">'+ico('info')+'<span>About</span></a>';
  h+='<div class="rail-foot">Jani picks the work. You decide.<br><span class="sample">'+ico('info')+'Prototype, sample data</span></div>';
  $('#rail').innerHTML=h;
}
function renderTop(){
  var r=role(),t=TITLES[S.page]||['',''];
  var opts=Object.keys(ROLES).map(function(k){return '<option value="'+k+'"'+(k===S.role?' selected':'')+'>'+esc(ROLES[k].title)+'</option>'}).join('');
  $('#top').innerHTML='<div class="crumb"><b>'+esc(t[0])+'</b><span>'+esc(t[1])+'</span></div>'+
   '<label class="who"><span class="avatar">'+r.ini+'</span><span class="sr">View as</span><select id="roleSel" aria-label="View as a different role">'+opts+'</select></label>'+
   '<button class="iconbtn" id="themeBtn" aria-label="Switch light and dark" title="Switch light and dark">'+ico('moon')+'</button>';
  $('#roleSel').onchange=function(e){S.role=e.target.value;store('jani.role',S.role);S.cur=null;if(!ROLES[S.role].nav.includes(S.page)&&S.page!=='about')S.page='home';render();toast('Now viewing as '+ROLES[S.role].title)};
  $('#themeBtn').onclick=toggleTheme;
}
function theme(){var t=document.documentElement.getAttribute('data-theme');if(t)return t;return window.matchMedia&&window.matchMedia('(prefers-color-scheme: dark)').matches?'dark':'light'}
function toggleTheme(){var n=theme()==='dark'?'light':'dark';document.documentElement.setAttribute('data-theme',n);store('jani.theme',n)}
window.__refreshBadges=function(){renderRail();if(S.page==='home'||S.page==='tasks')renderPage(true)};

/* ---------- home ---------- */
function nextTask(){var t=myTasks().filter(function(t){var s=tState(t);return s!=='Done'&&t.script})[0]||myTasks().filter(function(t){return tState(t)!=='Done'})[0];return t}
var NOTICED={
 officer:[['6 customers at Riverside have a phone number that does not match mobile banking.','These are the 5 to confirm and 1 more Jani found today.'],['Last week’s checker notes mention date-of-birth typos.','Jani added a check for it. You will see it in today’s run.']],
 accountant:[['A cheque for KES 25,000 cleared one day late.','Jani will ask whether to treat it as timing.'],['Bank charges rose 6% this month.','Worth a look before the controller sees the adjustments.']],
 analyst:[['Closures rose 14% this week.','Two branches drive most of it. Jani will lead the report with that.'],['The scorecard data is 2 days stale.','A refresh is lined up for Friday.']],
 cs:[['Failed mobile transfers are the biggest cause this week.','14 of 38 open complaints.'],['Kevin’s queue is over capacity.','Esther has room. Jani suggests moving 3.']],
 ops:[['Joseph Otieno has 9 approvals waiting.','More than anyone, and 7 are overdue.'],['Two people have spare capacity.','Grace and Daniel could take reassignments.']],
 region:[['Northgate has 31 open exceptions, the most in the region.','Target is 20. It has been rising for 3 weeks.'],['Riverside cut its backlog by half this month.','Its process may be worth copying.']],
 ceo:[['Customer records is ahead of plan.','Corrections are reaching the checker faster than before.'],['One approver holds 9 open corrections.','A people risk, not a data risk.']]
};
function homeNeeds(){
  var rows=myTasks().filter(function(t){return tState(t)==='Needs you'});
  if(!rows.length)return '<div class="hero-empty">Nothing is waiting on you.<br><span class="faint">Jani will tell you the moment something is.</span></div>';
  return rows.map(function(t){return '<div class="t" data-open="'+t.id+'" role="button" tabindex="0"><div style="flex:1;min-width:0"><div class="tt">'+esc(t.title)+'</div><div class="tm">'+esc(t.why.split('.')[0])+'.</div></div>'+st('Needs you')+'</div>'}).join('');
}
function homeDoer(){
  var r=role(),mine=myTasks(),nt=nextTask(),done=mine.filter(function(t){return tState(t)==='Done'}).length,pct=Math.round(done/mine.length*100);
  var h='<div class="hello"><span class="avatar lg">'+r.ini+'</span><div><h1>Good morning, '+first(r.name)+'.</h1><p class="muted" style="margin-top:6px;max-width:60ch">Jani lined up '+mine.length+' tasks for you today and put the most urgent first. You decide anything that changes something.</p></div></div>';
  h+='<div class="grid g-main"><div class="stack">';
  if(nt){h+='<section class="next" aria-label="Next task"><div class="row"><span class="st info">Next for you</span><span class="faint">'+esc(nt.mission)+'</span><span class="sp faint">'+ico('clock')+' '+esc(nt.due)+'</span></div><h2 style="font-size:20px;letter-spacing:-.01em">'+esc(nt.title)+'</h2><div class="why"><b>Why Jani chose this for you.</b> '+esc(nt.why)+'</div><div class="row"><button class="btn primary" data-start="'+nt.id+'">'+ico('play')+' '+(runOf(nt)&&runOf(nt).state!=='ready'?'Open the work':'Start with Jani')+'</button><button class="btn quiet" data-swap="'+nt.id+'">Give this to someone else</button><span class="sp faint">'+esc(nt.est)+'</span></div></section>'}
  h+='<section class="panel"><header><h2>Needs you</h2></header><div class="rows tasklist" id="needs">'+homeNeeds()+'</div></section></div>';
  h+='<div class="stack"><section class="panel"><header><h2>Your day</h2><span class="faint">'+done+' of '+mine.length+' done</span></header><div style="padding:12px 16px 4px"><div class="meter ok" role="progressbar" aria-label="Tasks done today" aria-valuenow="'+pct+'" aria-valuemin="0" aria-valuemax="100"><i style="width:'+pct+'%"></i></div></div><div class="rows tasklist">'+mine.map(function(t){return '<div class="t" data-open="'+t.id+'" role="button" tabindex="0"><div style="flex:1;min-width:0"><div class="tt">'+esc(t.title)+'</div><div class="tm">'+esc(t.est)+'</div></div>'+st(tState(t))+'</div>'}).join('')+'</div></section>';
  h+='<section class="panel"><header><h2>Jani noticed</h2></header><div class="rows">'+(NOTICED[S.role]||[]).map(function(n){return '<div><b>'+esc(n[0])+'</b><div class="muted" style="margin-top:2px">'+esc(n[1])+'</div></div>'}).join('')+'</div></section></div></div>';
  h+=askBox();return h;
}
function askBox(){
  return '<form class="askbox" id="askForm" style="margin-top:20px"><span class="avatar jani">'+ico('star')+'</span><input id="askIn" type="text" placeholder="Ask Jani anything about your work" aria-label="Ask Jani"><button class="btn primary" type="submit">'+ico('send')+' Ask</button></form><div class="ask-row" style="margin-top:10px">'+['What changed since yesterday?','Explain this in simpler words','Who else is affected?'].map(function(q){return '<button class="chip" data-q="'+esc(q)+'">'+esc(q)+'</button>'}).join('')+'</div>';
}
function statsStrip(a){return '<div class="stats">'+a.map(function(x){return '<div><div class="l">'+esc(x[0])+'</div><div class="v">'+esc(x[1])+'</div><div class="d '+(x[3]||'faint')+'">'+esc(x[2]||'')+'</div></div>'}).join('')+'</div>'}
function teamRows(key){
  return TEAMS[key].map(function(p){var pct=Math.min(100,Math.round(p[2]/p[3]*100)),cls=p[2]>p[3]?'bad':pct>=85?'warn':'ok';
    return '<tr><td><div class="row"><span class="avatar">'+p[1]+'</span><b>'+esc(p[0])+'</b></div></td><td class="num">'+p[2]+' of '+p[3]+'</td><td style="min-width:120px"><div class="meter '+cls+'" role="progressbar" aria-label="Workload" aria-valuenow="'+pct+'" aria-valuemin="0" aria-valuemax="100"><i style="width:'+pct+'%"></i></div></td><td>'+st(p[4])+'</td></tr>'}).join('');
}
function homeTeam(){
  var r=role(),n=needsCount(),key=S.role;
  var h='<div class="hello"><span class="avatar lg">'+r.ini+'</span><div><h1>Good morning, '+first(r.name)+'.</h1><p class="muted" style="margin-top:6px;max-width:60ch">'+(key==='cs'?'The contact centre has 38 open complaints. Jani sorted them and found who has room.':'Operations has 12 overdue exceptions. Most trace back to one approver.')+'</p></div></div>';
  h+=statsStrip(key==='cs'?[['Open complaints','38','6 older than 5 days','warn'],['Needs you',String(n),'Jani is waiting'],['Replies due today','24','Drafted by Jani'],['Resolved this week','61','+9 on last week','ok']]:[['Overdue exceptions','12','Down from 19','ok'],['Needs you',String(n),'Jani is waiting'],['Approvals waiting','14','9 on one person','warn'],['Missions running','4','All on plan','ok']]);
  h+='<div class="grid g-main" style="margin-top:16px"><div class="stack"><section class="panel"><header><h2>Needs you</h2></header><div class="rows tasklist" id="needs">'+homeNeeds()+'</div></section>';
  h+='<section class="panel"><header><h2>Jani suggests</h2></header><div class="rows">'+(key==='cs'?[['Move 3 calls from Kevin to Esther.','Kevin is over capacity and Esther has room.'],['Take the 6 oldest complaints yourself.','They have waited longest and need a decision-maker.']]:[['Move 7 approvals from Joseph to Mary and Ali.','Joseph has 9 open. Two others have room.'],['Start the draft mission “Loan file checks”.','Faith wrote it. It needs your sign-off, then IT.']]).map(function(s){return '<div class="row"><div style="flex:1;min-width:0"><b>'+esc(s[0])+'</b><div class="muted">'+esc(s[1])+'</div></div><button class="btn sm" data-sug="1">Review</button></div>'}).join('')+'</div></section></div>';
  h+='<section class="panel"><header><h2>'+(key==='cs'?'Your team now':'People and workload')+'</h2></header><div class="tw"><table><tbody>'+teamRows(key==='cs'?'cs':'ops')+'</tbody></table></div></section></div>';
  return h+askBox();
}
function branchTable(){
  return '<div class="tw"><table><thead><tr><th>Branch</th><th>Customers checked</th><th>Open exceptions</th><th>Days to resolve</th><th>Trend</th><th>Status</th></tr></thead><tbody>'+BRANCH.map(function(b){return '<tr><td><b>'+b[0]+'</b></td><td class="num">'+b[1]+'</td><td class="num">'+b[2]+'</td><td class="num">'+b[3]+'</td><td class="num '+(b[5]>0?'':'')+'">'+(b[5]>0?'Up '+b[5]:'Down '+Math.abs(b[5]))+' this week</td><td>'+st(b[4])+'</td></tr>'}).join('')+'</tbody></table></div>';
}
function homeRegion(){
  var r=role();
  var h='<div class="hello"><span class="avatar lg">'+r.ini+'</span><div><h1>Good morning, '+first(r.name)+'.</h1><p class="muted" style="margin-top:6px;max-width:60ch">Four of six branches are on track. Northgate needs your attention. Everything below comes from the same checks the branches use.</p></div></div>';
  h+=statsStrip([['Branches on track','4 of 6','Northgate behind','warn'],['Open exceptions','78','Down 6 this week','ok'],['Median days to resolve','1.9','Was 4.1 a quarter ago','ok'],['Needs your decision','1','Northgate cover','warn']])+'<div style="margin-top:16px" class="grid g-main"><section class="panel"><header><h2>Branches</h2>'+sampleTag()+'</header>'+branchTable()+'</section><div class="stack"><section class="panel"><header><h2>Needs you</h2></header><div class="rows tasklist" id="needs">'+homeNeeds()+'</div></section><section class="panel"><header><h2>Jani noticed</h2></header><div class="rows">'+NOTICED.region.map(function(n){return '<div><b>'+esc(n[0])+'</b><div class="muted">'+esc(n[1])+'</div></div>'}).join('')+'</div></section></div></div>';
  return h+askBox();
}
function homeCeo(){
  var r=role();
  var h='<div class="hello"><span class="avatar lg">'+r.ini+'</span><div><h1>Good morning, Dr '+r.name.split(' ')[1]+'.</h1><p class="muted" style="margin-top:6px;max-width:60ch">The four missions are running and ahead of plan overall. Two things for you: one decision, and one risk Jani wants you to know about.</p></div></div>';
  h+=statsStrip([['Records reconciled','18,420','This month','faint'],['Open exceptions','312','Down 9% on last month','ok'],['Median days to resolve','1.9','Was 4.1','ok'],['Corrections checked by a second person','100%','Every one','ok']]);
  h+='<div class="grid g-main" style="margin-top:16px"><section class="panel"><header><h2>Missions</h2>'+sampleTag()+'</header><div class="rows">'+[['Customer records','Ahead of plan','610 h saved'],['Month-end close','On plan','290 h saved'],['Weekly insight pack','Ahead of plan','240 h saved'],['Complaint handling','On plan','100 h saved']].map(function(m){return '<div class="row"><b style="flex:1">'+m[0]+'</b><span class="muted">'+m[2]+'</span>'+st(m[1].indexOf('Ahead')===0?'On track':'On track')+'</div>'}).join('')+'</div></section><div class="stack"><section class="panel"><header><h2>Needs you</h2></header><div class="rows tasklist" id="needs">'+homeNeeds()+'</div></section><section class="panel"><header><h2>Risks Jani is watching</h2></header><div class="rows">'+NOTICED.ceo.map(function(n){return '<div><b>'+esc(n[0])+'</b><div class="muted">'+esc(n[1])+'</div></div>'}).join('')+'</div></section></div></div>';
  return h+askBox();
}
function bindHome(){
  $$('[data-open]').forEach(function(el){var go=function(){openTask(el.getAttribute('data-open'))};el.onclick=go;el.onkeydown=function(e){if(e.key==='Enter'||e.key===' '){e.preventDefault();go()}}});
  $$('[data-start]').forEach(function(b){b.onclick=function(){openTask(b.getAttribute('data-start'),true)}});
  $$('[data-swap]').forEach(function(b){b.onclick=function(){toast('Jani will offer this to someone with capacity, and tell you who.')}});
  $$('[data-sug]').forEach(function(b){b.onclick=function(){toast('In the real product this opens the suggestion with exactly what would change.')}});
  var f=$('#askForm');if(f)f.onsubmit=function(e){e.preventDefault();var v=$('#askIn').value.trim();if(!v)return;S.pendingAsk=v;goto('work')};
  $$('[data-q]').forEach(function(b){b.onclick=function(){S.pendingAsk=b.getAttribute('data-q');goto('work')}});
}
function openTask(id,begin){
  var t=TASKS.filter(function(x){return x.id===id})[0];if(!t)return;
  if(t.script){S.pendingTask=t.script;S.pendingStart=!!begin;goto('work')}
  else if(t.role===S.role&&/decide|Decide|Review the draft/.test(t.title)){S.pendingAsk=S.role==='ops'?null:null;if(S.role==='ops'){S.page='missions';goto('missions')}else{goto('work')}}
  else toast('Jani will start this after the task before it.');
}

/* ---------- tasks ---------- */
function renderTasks(){
  var mine=myTasks(),groups=[['Needs you',function(s){return s==='Needs you'}],['Ready or running',function(s){return s==='Ready'||s==='Running'||s==='Paused'}],['Waiting',function(s){return /Waiting/.test(s)}],['Done',function(s){return s==='Done'||s==='Stopped'}]];
  var h='<div class="page-head"><div><h1>Your tasks</h1><p class="lead">Jani picks what you should do next and says why. You can always hand a task to someone else.</p></div><div class="act">'+sampleTag()+'</div></div><div class="stack">';
  groups.forEach(function(g){
    var rows=mine.filter(function(t){return g[1](tState(t))});if(!rows.length)return;
    h+='<section class="panel"><header><h2>'+g[0]+'</h2><span class="faint">'+rows.length+'</span></header><div class="rows">'+rows.map(function(t){
      return '<div><div class="row"><div style="flex:1;min-width:200px"><b>'+esc(t.title)+'</b><div class="faint" style="margin-top:2px">'+esc(t.mission)+' · due '+esc(t.due)+' · '+esc(t.est)+'</div></div>'+st(tState(t))+'<button class="btn sm '+(tState(t)==='Needs you'?'primary':'')+'" data-open="'+t.id+'">'+(tState(t)==='Needs you'?'Decide':(tState(t)==='Done'?'See result':'Open'))+'</button></div><details style="margin-top:8px"><summary class="faint" style="cursor:pointer">Why Jani gave you this</summary><p class="muted" style="margin-top:6px;max-width:68ch">'+esc(t.why)+'</p><div class="row" style="margin-top:8px"><button class="btn sm quiet" data-swap="'+t.id+'">Give this to someone else</button><button class="btn sm quiet" data-later="'+t.id+'">Do it later</button></div></details></div>';
    }).join('')+'</div></section>';
  });
  h+='</div>';view.innerHTML=h;bindHome();
  $$('[data-later]').forEach(function(b){b.onclick=function(){toast('Jani moved it to later today and will remind you.')}});
}

/* ---------- work: the run ---------- */
function cites(x){return esc(x).replace(/\[(\d)\]/g,'<a class="cite" href="#work" data-cite="$1" aria-label="source $1">$1</a>')}
function evInner(l,run){
  if(l.k==='say')return '<span class="avatar jani">'+ico('star')+'</span><div class="b"><div class="bubble">'+cites(l.x)+(l.done?'':'<span class="cursor"></span>')+'</div></div>';
  if(l.k==='me')return '<span class="avatar">'+role().ini+'</span><div class="b"><div class="bubble me">'+esc(l.x)+'</div></div>';
  if(l.k==='tool'){var last=(l.d||'').split(' · ').pop();return '<span class="avatar jani" style="visibility:hidden"></span><div class="b"><details class="toolrow"><summary>'+(l.done?ico('check'):'<span class="st info live" style="padding:0 6px"> </span>')+'<b>'+esc(l.t)+'</b><span class="d">'+(l.done?esc(last):'Working')+'</span></summary><pre>'+esc(l.d)+'</pre></details></div>'}
  if(l.k==='data'){var e=l.ev;return '<span class="avatar jani" style="visibility:hidden"></span><div class="b"><div class="card"><div class="h">'+ico('chart')+esc(e.title)+'</div><div class="tw"><table><thead><tr>'+e.cols.map(function(c){return '<th>'+esc(c)+'</th>'}).join('')+'</tr></thead><tbody>'+e.rows.map(function(r){return '<tr>'+r.map(function(c,i){return '<td'+(i>0&&/^[+\-\d]/.test(c)?' class="num"':'')+'>'+(/^(Behind|Watch|On track)$/.test(c)?st(c):esc(c))+'</td>'}).join('')+'</tr>'}).join('')+'</tbody></table></div></div></div>'}
  if(l.k==='ask'){var q=l.ev;
    if(l.ans)return '<span class="avatar jani" style="visibility:hidden"></span><div class="b"><div class="decided">'+ico('check')+'<span><b>'+esc(q.q)+'</b> You answered: '+esc(l.ans)+'</span></div></div>';
    return '<span class="avatar jani">'+ico('star')+'</span><div class="b"><div class="card askc" role="group" aria-label="Question from Jani"><div class="h">'+ico('info')+'Jani has a question</div><div class="bd"><div class="what">'+esc(q.q)+'</div><div class="opts">'+q.o.map(function(o,i){return '<button class="btn '+(i===0?'primary':'')+'" data-ans="'+l.idx+'" data-text="'+esc(o)+'">'+esc(o)+'</button>'}).join('')+'</div><div class="own"><input type="text" id="own'+l.idx+'" placeholder="Or answer in your own words" aria-label="Your answer"><button class="btn" data-own="'+l.idx+'">Send</button></div></div></div></div>'}
  if(l.k==='gate'){var g=l.ev;
    if(l.dec){var w={all:g.all,small:g.small,no:g.no}[l.dec.choice];return '<span class="avatar jani" style="visibility:hidden"></span><div class="b"><div class="decided">'+ico(l.dec.choice==='no'?'pause':'check')+'<span><b>'+esc(g.what)+'</b> '+esc(role().name.split(' ')[0])+' chose “'+esc(w)+'” at '+l.dec.at+'. Recorded in Proof.</span></div></div>'}
    return '<span class="avatar jani">'+ico('star')+'</span><div class="b"><div class="card gate" role="group" aria-label="Needs your decision"><div class="h">'+ico('alert')+'Needs you</div><div class="bd"><div class="what">'+esc(g.what)+'</div><div class="facts-grid">'+g.facts.map(function(f){return '<div><div class="k">'+esc(f[0])+'</div><div class="v">'+esc(f[1])+'</div></div>'}).join('')+'</div><div class="preview"><table><thead><tr>'+g.cols.map(function(c){return '<th>'+esc(c)+'</th>'}).join('')+'</tr></thead><tbody>'+g.rows.map(function(r){return '<tr>'+r.map(function(c,i){return '<td'+(i===r.length-1&&g.cols.length>2?' class="add"':'')+'>'+esc(c)+'</td>'}).join('')+'</tr>'}).join('')+'</tbody></table></div><div class="note">'+ico('lock')+' '+esc(g.conseq)+'</div><div class="acts"><button class="btn primary" data-gate="'+l.idx+'" data-c="all">'+esc(g.all)+'</button><button class="btn" data-gate="'+l.idx+'" data-c="small">'+esc(g.small)+'</button><button class="btn quiet" data-gate="'+l.idx+'" data-c="no">'+esc(g.no)+'</button></div><div class="note">Approving allows exactly this, once. Anything else asks again.</div></div></div></div>'}
  if(l.k==='result'){var rs=l.ev,stats=rs.stats(run.vars);return '<span class="avatar jani">'+ico('star')+'</span><div class="b"><div class="card done"><div class="h">'+ico('check')+'Done</div><div class="bd"><div class="what">'+esc(rs.title)+'</div><div class="facts-grid">'+stats.map(function(f){return '<div><div class="k">'+esc(f[0])+'</div><div class="v">'+esc(f[1])+'</div></div>'}).join('')+'</div><div class="note">'+esc(rs.note)+'</div><div class="acts"><button class="btn primary" data-go="proof">'+ico('shield')+' See the proof</button><button class="btn" data-go="home">Back to Home</button></div></div></div></div>'}
  return '';
}
function planHTML(run){
  if(!run.plan.length)return '';
  return '<ol class="plan" aria-label="Plan">'+run.plan.map(function(p,i){var w={done:'Done',active:'Working',waiting:'Needs you',todo:'To do'}[p.s];return '<li class="'+p.s+'"'+(p.s==='active'?' aria-current="step"':'')+'><span class="m">'+(p.s==='done'?ico('check'):'')+'</span><span>'+esc(p.t)+'</span><span class="sr">'+w+'</span></li>'}).join('')+'</ol>';
}
var STW={ready:['Ready',''],running:['Running','info live'],needs:['Needs you','warn'],'needs-ask':['Needs you','warn'],paused:['Paused',''],stopped:['Stopped','bad'],done:['Done','ok'],ask:['Ask anything','info']};
function headHTML(run){
  var sc=run.sc,w=STW[run.state],ctl='',total=sc.ev.length||1,stepN=Math.min(run.plan.filter(function(p){return p.s==='done'}).length+1,run.plan.length);
  if(run.state==='running')ctl='<button class="btn sm" data-ctl="pause">'+ico('pause')+' Pause</button><button class="btn sm danger" data-ctl="stop">'+ico('stop')+' Stop</button>';
  else if(run.state==='paused')ctl='<button class="btn sm primary" data-ctl="resume">'+ico('play')+' Resume</button><button class="btn sm danger" data-ctl="stop">'+ico('stop')+' Stop</button>';
  else if(run.state==='needs')ctl='<button class="btn sm danger" data-ctl="stop">'+ico('stop')+' Stop</button>';
  else if(run.state==='stopped'||run.state==='done')ctl='<button class="btn sm" data-ctl="again">'+ico('play')+' Run again</button>';
  return '<div class="row"><span class="st '+w[1]+'">'+w[0]+'</span><div class="sp row" style="gap:8px">'+ctl+'</div></div><h2 class="goal">'+esc(sc.goal)+'</h2><div class="facts"><span>'+esc(sc.by)+' for <b>'+esc(role().name)+'</b></span>'+(run.plan.length?'<span>Step <b>'+stepN+'</b> of '+run.plan.length+'</span>':'')+'<span>Only reads until you say so</span></div>';
}
function ctxHTML(run){
  var sc=run.sc,c=sc.ctx,prog=run.plan.length?run.plan.filter(function(p){return p.s==='done'}).length/run.plan.length:0,h='';
  h+='<section><h3>Sources</h3>'+(run.srcs||c.src.map(function(s){return s[0]})).map(function(s,i){var src=c.src[i];return '<div class="src" id="src'+(i+1)+'" data-src="'+(i+1)+'">'+ico('db')+'<span style="flex:1;min-width:0"><b>'+(i+1)+'.</b> '+esc(Array.isArray(src)?src[0]:s)+'</span><span class="faint">'+esc(Array.isArray(src)?src[1]:'Read-only')+'</span></div>'}).join('')+'</section>';
  h+='<section><h3>What Jani may do</h3>'+c.allow.map(function(a){return '<div class="perm"><span class="st '+a[0]+'">'+esc(a[1])+'</span><span>'+esc(a[2])+'</span></div>'}).join('')+'</section>';
  if(c.limits.length)h+='<section><h3>Limits</h3>'+c.limits.map(function(l,i){var used=i===0?Math.round(prog*(sc.est||10)*10)/10:Math.round(prog*l[1]),pct=Math.min(100,Math.round(used/l[1]*100));return '<div style="margin-bottom:10px"><div class="row" style="gap:6px"><span>'+esc(l[0])+'</span><span class="sp faint num">'+used.toLocaleString()+' of '+l[1].toLocaleString()+(l[2]?' '+l[2]:'')+'</span></div><div class="meter" role="progressbar" aria-label="'+esc(l[0])+'" aria-valuenow="'+pct+'" aria-valuemin="0" aria-valuemax="100"><i style="width:'+pct+'%"></i></div></div>'}).join('')+'</section>';
  var mem=(S.mem[run.key]=S.mem[run.key]||c.mem.slice());
  h+='<section><h3>Jani remembers</h3>'+(mem.length?mem.map(function(m,i){return '<div class="mem"><span>'+esc(m)+'</span><button data-forget="'+i+'" aria-label="Forget this">Forget</button></div>'}).join(''):'<div class="faint">Nothing yet.</div>')+'<p class="faint" style="margin-top:8px;font-size:12px">Notes are advisory. They never give Jani permission to do more.</p></section>';
  return h;
}
function queueHTML(cur){
  var mine=myTasks();
  return '<h3>Today</h3>'+mine.map(function(t){var on=t.script&&cur&&t.script===cur.key;return '<button class="qi" '+(on?'aria-current="true"':'')+' data-qt="'+t.id+'"><b>'+esc(t.title)+'</b>'+st(tState(t))+'</button>'}).join('');
}
function renderWork(){
  var r=role(),cur,ask=false;
  if(r.script){var sc=SC[r.script];if(S.pendingTask&&SC[S.pendingTask]&&ROLES[S.role].script===S.pendingTask){}cur=E.getRun(r.script,sc)}
  else{cur=E.askRun(S.role);ask=true}
  S.cur=cur;view.classList.add('fill');
  var h='<div class="work">'+(ask?'<aside class="queue"><h3>Ask about</h3>'+ASKS[S.role].map(function(a,i){return '<button class="qi" data-askq="'+i+'"><b>'+esc(a.q)+'</b><span class="faint">Answer with sources</span></button>'}).join('')+'</aside>':'<aside class="queue" id="queue">'+queueHTML(cur)+'</aside>')+
   '<section class="run"><div class="run-head" id="rh"></div><div class="run-body" id="rb"><div id="rstart"></div><div id="rplan"></div><div class="stream" id="rs" role="log" aria-live="off" aria-label="Activity"></div></div><form class="composer" id="comp"><div class="chips" id="cchips"></div><div class="askbox"><input id="cin" type="text" autocomplete="off" aria-label="Steer Jani"><button class="btn primary" type="submit">'+ico('send')+' <span id="csend">Send</span></button></div></form></section><aside class="ctx" id="rctx"></aside></div>';
  view.innerHTML=h;
  $('#comp').onsubmit=function(e){e.preventDefault();var v=$('#cin').value.trim();if(!v)return;$('#cin').value='';if(S.cur.key.indexOf('ask-')===0)userAsk(v);else E.steer(S.cur,v)};
  $('#rb').addEventListener('click',onRunClick);
  $('#rctx').addEventListener('click',function(e){var f=e.target.closest('[data-forget]');if(f){S.mem[S.cur.key].splice(+f.getAttribute('data-forget'),1);toast('Forgotten. Jani will not use that note again.');paintCtx()}var c=e.target.closest('[data-src]');if(c)flash(c)});
  var qa=$('.queue');if(qa)qa.addEventListener('click',function(e){var b=e.target.closest('[data-qt]');if(b){var t=TASKS.filter(function(x){return x.id===b.getAttribute('data-qt')})[0];if(t&&t.script){}else toast('Jani will start this after the task before it.')}var a=e.target.closest('[data-askq]');if(a)E.askQuestion(S.cur,ASKS[S.role][+a.getAttribute('data-askq')])});
  window.__paintRun=paintRun;S.nodes={};paintRun(cur,true);
  if(S.pendingAsk){var pa=S.pendingAsk;S.pendingAsk=null;if(ask)userAsk(pa);else{E.steer(cur,pa);}}
  if(S.pendingStart&&!ask){S.pendingStart=false;if(cur.state==='ready')E.start(cur)}
}
function userAsk(v){
  var run=S.cur,list=ASKS[S.role],m=list.filter(function(a){return a.q.toLowerCase()===v.toLowerCase()})[0];
  if(m)E.askQuestion(run,m);
  else{run.log.push({k:'me',x:v});window.__paintRun(run);setTimeout(function(){run.log.push({k:'say',x:'In this prototype I answer the suggested questions on the left. The real Jani answers any question from your data, and shows where each number came from.',done:true});window.__paintRun(run)},600)}
}
function flash(el){el.style.background='var(--accent-tint)';setTimeout(function(){el.style.background=''},900)}
function onRunClick(e){
  var run=S.cur,t;
  if(t=e.target.closest('[data-start]')){E.start(run);return}
  if(t=e.target.closest('[data-ctl]')){var c=t.getAttribute('data-ctl');if(c==='pause')E.pause(run);if(c==='stop')E.stop(run);if(c==='resume')E.resume(run);if(c==='again')E.start(run);return}
  if(t=e.target.closest('[data-ans]')){var idx=+t.getAttribute('data-ans');run.key.indexOf('ask-')===0?E.answerAskMode(run,idx,t.getAttribute('data-text')):E.answer(run,idx,t.getAttribute('data-text'));return}
  if(t=e.target.closest('[data-own]')){var i2=+t.getAttribute('data-own'),v=$('#own'+i2).value.trim();if(v)run.key.indexOf('ask-')===0?E.answerAskMode(run,i2,v):E.answer(run,i2,v);return}
  if(t=e.target.closest('[data-gate]')){E.decide(run,+t.getAttribute('data-gate'),t.getAttribute('data-c'));return}
  if(t=e.target.closest('[data-go]')){goto(t.getAttribute('data-go'));return}
  if(t=e.target.closest('[data-cite]')){var s=$('#src'+t.getAttribute('data-cite'));if(s){e.preventDefault();s.scrollIntoView({block:'nearest'});flash(s)}return}
}
function paintCtx(){var el=$('#rctx');if(el)el.innerHTML=ctxHTML(S.cur)}
function paintRun(run,full){
  if(S.cur!==run||S.page!=='work')return;
  var rb=$('#rb');if(!rb)return;
  var near=rb.scrollHeight-rb.scrollTop-rb.clientHeight<80;
  $('#rh').innerHTML=headHTML(run);
  var isAsk=run.key.indexOf('ask-')===0;
  var startEl=$('#rstart');
  if(run.state==='ready'&&!isAsk)startEl.innerHTML='<div class="start"><span class="avatar jani lg">'+ico('star')+'</span><h2 style="font-size:20px;letter-spacing:-.01em">Ready when you are.</h2><p class="muted">Jani will read the data it needs, show you what it finds, and stop to ask before it changes or sends anything. You can pause or stop at any time.</p><button class="btn primary" data-start="1">'+ico('play')+' Start with Jani</button></div>';
  else if(isAsk&&!run.log.length)startEl.innerHTML='<div class="start"><span class="avatar jani lg">'+ico('star')+'</span><h2 style="font-size:20px;letter-spacing:-.01em">Ask about '+(S.role==='ceo'?'the bank':'your region')+'.</h2><p class="muted">Pick a question on the left, or type one. Every answer shows where its numbers came from, and anything that needs a decision comes to you as a card.</p></div>';
  else startEl.innerHTML='';
  $('#rplan').innerHTML=planHTML(run);
  var rs=$('#rs');
  run.log.forEach(function(l,i){
    var id='ev'+i,node=S.nodes[id];
    var html=evInner(l,run);
    if(!node||!node.isConnected){node=document.createElement('div');node.className='ev'+(l.k==='me'?' me':'');node.innerHTML=html;node._h=html;rs.appendChild(node);S.nodes[id]=node}
    else if(node._h!==html){node.innerHTML=html;node._h=html}
  });
  // clean nodes beyond log length (after restart)
  while(rs.children.length>run.log.length)rs.removeChild(rs.lastChild);
  var chips=$('#cchips'),cin=$('#cin');
  if(isAsk){cin.placeholder='Ask a question';chips.innerHTML=''}
  else{cin.placeholder=run.state==='ready'?'Add a note for Jani before it starts':'Steer Jani: add a note or a limit';chips.innerHTML=['Skip anything older than a year','Explain it in simpler words','Why did you choose that?'].map(function(c){return '<button type="button" class="chip" data-chip="'+esc(c)+'">'+esc(c)+'</button>'}).join('')}
  $$('[data-chip]',chips).forEach(function(b){b.onclick=function(){cin.value=b.getAttribute('data-chip');cin.focus()}});
  paintCtx();var qe=$('#queue');if(qe)qe.innerHTML=queueHTML(run);
  if(near||full){rb.scrollTop=rb.scrollHeight}
  if(S.page==='work')renderRail();
}

/* ---------- team ---------- */
function renderTeam(){
  var key=S.role==='region'?'region':(S.role==='cs'?'cs':'ops');
  var h='<div class="page-head"><div><h1>'+(key==='region'?'Branches':'Your team')+'</h1><p class="lead">'+(key==='region'?'Each branch runs the same checks. Open a branch to see where it is stuck.':'See who has room and who is stretched. Jani suggests moves; you decide.')+'</p></div><div class="act">'+sampleTag()+'</div></div>';
  if(key==='region'){h+='<section class="panel"><header><h2>Branches</h2></header>'+branchTable()+'</section>'}
  else{h+='<section class="panel"><header><h2>Workload today</h2><span class="faint">Tasks against capacity</span></header><div class="tw"><table><thead><tr><th>Person</th><th>Tasks</th><th>Load</th><th>Status</th></tr></thead><tbody>'+teamRows(key)+'</tbody></table></div></section><section class="panel" style="margin-top:16px"><header><h2>Jani suggests</h2></header><div class="rows">'+(key==='cs'?[['Move 3 calls from Kevin Omondi to Esther Wairimu.','Kevin is at 9 of 8. Esther is at 3 of 8.']]:[['Move 7 approvals from Joseph Otieno to Mary Chebet and Ali Noor.','Joseph is at 9 of 6. Two others have room.']]).map(function(s){return '<div class="row"><div style="flex:1"><b>'+esc(s[0])+'</b><div class="muted">'+esc(s[1])+'</div></div><button class="btn primary sm" data-apply="1">Approve move</button><button class="btn sm" data-sug="1">Not now</button></div>'}).join('')+'</div></section>'}
  view.innerHTML=h;bindHome();$$('[data-apply]').forEach(function(b){b.onclick=function(){b.closest('.row').innerHTML='<div class="decided" style="width:100%">'+ico('check')+'<span>Moved. The people involved were told, and it is recorded in Proof.</span></div>';toast('Work moved')}});
}

/* ---------- results ---------- */
function bars(rows,max,unit,cls){return '<div class="bars">'+rows.map(function(r){var p=Math.round(r[1]/max*100);return '<div class="bar"><span>'+esc(r[0])+'</span><div class="meter '+(r[2]||cls||'')+'" role="img" aria-label="'+esc(r[0])+' '+r[1]+' '+unit+'"><i style="width:'+p+'%"></i></div><span class="num" style="text-align:right">'+r[1]+(unit==='%'?'%':'')+'</span></div>'}).join('')+'</div>'}
function renderResults(){
  var r=role(),h='<div class="page-head"><div><h1>Results</h1><p class="lead">'+(S.role==='ceo'?'The four missions in plain numbers. Every figure links back to the checks that produced it.':S.role==='region'?'How the branches compare, and where to help.':'How the missions are doing across Operations.')+'</p></div><div class="act">'+sampleTag()+'<div class="seg" role="group" aria-label="Period"><button aria-pressed="true">This month</button><button aria-pressed="false">This quarter</button></div></div></div>';
  h+=statsStrip([['Records reconciled','18,420','This month'],['Open exceptions','312','Down 9% on last month','ok'],['Median days to resolve','1.9','Was 4.1 a quarter ago','ok'],['Hours saved','1,240','This quarter, estimated']]);
  h+='<div class="grid g2" style="margin-top:16px"><section class="panel"><header><h2>Open exceptions by branch</h2></header><div style="padding:16px">'+bars(BRANCH.map(function(b){return [b[0],b[2],b[4]==='Behind'?'bad':b[4]==='Watch'?'warn':'ok']}),35,'open')+'<p class="muted" style="margin-top:12px">Northgate is the only branch behind its target of 20. It has been rising for three weeks.</p></div></section>';
  h+='<section class="panel"><header><h2>Hours saved by mission</h2></header><div style="padding:16px">'+bars([['Customer records',610],['Month-end close',290],['Weekly insight pack',240],['Complaint handling',100]],650,'h')+'<p class="muted" style="margin-top:12px">Estimated from the time people spent on the same work before Jani. It is a guide, not a promise.</p></div></section></div>';
  h+='<section class="panel" style="margin-top:16px"><header><h2>Quality, checked</h2></header><div class="rows"><div class="row"><b style="flex:1">Corrections approved by a second person</b><span class="num">100%</span>'+st('Verified')+'</div><div class="row"><b style="flex:1">Records changed by Jani directly</b><span class="num">0</span>'+st('Verified')+'</div><div class="row"><b style="flex:1">Real mismatches wrongly marked fine, in sampled checks</b><span class="num">Not yet measured</span>'+st('Watch','warn')+'</div></div></section>';
  view.innerHTML=h;
  $$('.seg button').forEach(function(b){b.onclick=function(){$$('.seg button').forEach(function(x){x.setAttribute('aria-pressed','false')});b.setAttribute('aria-pressed','true');toast('Showing '+b.textContent.toLowerCase())}});
}

/* ---------- map ---------- */
function renderMap(){
  var m=MAPS[S.mapK],h='<div class="page-head"><div><h1>How it fits</h1><p class="lead">Follow one piece of work from the data it starts with to the proof it leaves. The highlighted card is where you come in.</p></div><div class="act">'+sampleTag()+'</div></div>';
  var keys={recon:'Customer records',ledger:'Month-end close',report:'Weekly insight pack',triage:'Complaint handling'};
  h+='<div class="ask-row" role="group" aria-label="Choose a mission" style="margin-bottom:14px">'+Object.keys(keys).map(function(k){return '<button class="chip" aria-pressed="'+(k===S.mapK)+'" data-mk="'+k+'">'+keys[k]+'</button>'}).join('')+'</div>';
  h+='<div class="map" id="map"><svg class="edges" id="edges" aria-hidden="true"></svg>'+m.cols.map(function(c,ci){return '<div class="col"><h3>'+esc(c)+'</h3><div class="nodes">'+m.nodes.filter(function(n){return n.c===ci}).map(function(n){var you=n.roles&&n.roles.indexOf(S.role)>=0;return '<button class="node'+(you?' you':'')+'" data-node="'+n.id+'" aria-pressed="'+(S.mapSel===n.id)+'">'+(you?'<span class="you-tag">You are here</span>':'')+'<b>'+esc(n.t)+'</b><span class="sub">'+esc(n.s)+'</span><span>'+st(n.st)+'</span></button>'}).join('')+'</div></div>'}).join('')+'</div>';
  var sel=m.nodes.filter(function(n){return n.id===S.mapSel})[0]||m.nodes.filter(function(n){return n.roles&&n.roles.indexOf(S.role)>=0})[0]||m.nodes[0];
  h+='<div class="detail" id="detail"><div class="row"><b style="font-size:15px">'+esc(sel.t)+'</b>'+st(sel.st)+'</div><p class="muted" style="margin-top:6px;max-width:70ch">'+esc(sel.d)+'</p><p class="faint" style="margin-top:8px">'+(sel.c===0?'Jani can only read here.':sel.c===1?'Jani works here by itself, within the rules IT signed.':sel.c===2?'A person decides here. Jani waits.':sel.c===3?'This is where something real changes, and only after people approve.':'Written automatically, so it can be checked later.')+'</p></div>';
  view.innerHTML=h;
  $$('[data-mk]').forEach(function(b){b.onclick=function(){S.mapK=b.getAttribute('data-mk');S.mapSel=null;renderMap()}});
  $$('[data-node]').forEach(function(b){b.onclick=function(){S.mapSel=b.getAttribute('data-node');renderMap()}});
  drawEdges(m,sel.id);
}
function drawEdges(m,selId){
  var wrap=$('#map'),svg=$('#edges');if(!wrap||!svg||getComputedStyle(svg).display==='none')return;
  var wb=wrap.getBoundingClientRect(),p='';
  svg.setAttribute('viewBox','0 0 '+wb.width+' '+wb.height);
  m.links.forEach(function(l){
    var a=wrap.querySelector('[data-node="'+l[0]+'"]'),b=wrap.querySelector('[data-node="'+l[1]+'"]');if(!a||!b)return;
    var ra=a.getBoundingClientRect(),rb=b.getBoundingClientRect(),x1=ra.right-wb.left,y1=ra.top+ra.height/2-wb.top,x2=rb.left-wb.left,y2=rb.top+rb.height/2-wb.top,mx=(x1+x2)/2,on=l[0]===selId||l[1]===selId;
    p+='<path class="'+(on?'on flow':'')+'" d="M'+x1+' '+y1+' C'+mx+' '+y1+' '+mx+' '+y2+' '+x2+' '+y2+'"/>';
  });
  svg.innerHTML=p;
}

/* ---------- proof ---------- */
function renderProof(){
  var f=S.proofF,tech=!!S.tech;
  var h='<div class="page-head"><div><h1>Proof</h1><p class="lead">A plain-language record of who did what, and a check that nothing has been altered since.</p></div><div class="act"><button class="btn" id="exp">'+ico('file')+' Export evidence pack</button></div></div>';
  h+='<div class="panel" style="margin-bottom:16px"><div class="row" style="padding:14px 16px">'+ico('shield')+'<div style="flex:1;min-width:200px"><b>Record verified</b><div class="muted">1,284 entries checked just now. None were changed after the fact.</div></div>'+st('Verified')+'</div></div>';
  h+='<div class="row" style="margin-bottom:14px"><div class="ask-row" role="group" aria-label="Filter">'+[['all','Everything'],['dec','Decisions'],['chg','Changes'],['q','Questions'],['read','Reads']].map(function(c){return '<button class="chip" aria-pressed="'+(f===c[0])+'" data-pf="'+c[0]+'">'+c[1]+'</button>'}).join('')+'</div><label class="sp row" style="gap:8px;cursor:pointer"><input type="checkbox" id="techT" '+(tech?'checked':'')+'> Show technical details</label></div>';
  h+='<div class="tl" role="list">'+PROOF.filter(function(e){return f==='all'||e.k===f}).map(function(e){return '<div class="e '+e.k+'" role="listitem"><div class="t">'+esc(e.t)+(e.k==='dec'?' · decision':e.k==='chg'?' · change made':'')+'</div><div><b>'+esc(e.who)+'</b> '+esc(e.x)+'</div>'+(tech?'<div class="mono faint" style="margin-top:4px">'+esc(e.tech)+'</div>':'')+'</div>'}).join('')+'</div>';
  view.innerHTML=h;
  $$('[data-pf]').forEach(function(b){b.onclick=function(){S.proofF=b.getAttribute('data-pf');renderProof()}});
  $('#techT').onchange=function(e){S.tech=e.target.checked;renderProof()};
  $('#exp').onclick=function(){toast('In the real product this downloads a signed evidence pack.')};
}

/* ---------- missions + builder ---------- */
var LIB={sources:['Core banking (read-only)','Onboarding forms (read-only)','Finance ledger (read-only)','Complaints log (read-only)','Uploaded files'],checks:['Records agree across systems','Totals match the ledger','No duplicate customers','Dates and IDs well-formed','Amounts within limits'],people:['Onboarding officer','Accountant','Data analyst','Customer service supervisor','Operations manager','Branch manager']};
var WIZ=['Goal','People','Data','Rules','Checks','Review'];
function renderMissions(){
  if(S.wiz){renderWizard();return}
  var h='<div class="page-head"><div><h1>Missions</h1><p class="lead">A mission is a job Jani can take on. Each one lists who is involved, what data it may read, what it may do, and who must approve.</p></div><div class="act"><button class="btn primary" id="newM">'+ico('plus')+' Create a mission</button></div></div><div class="grid g3">'+S.missions.map(function(m){return '<div class="mcard"><span class="ic">'+ico(m.ic)+'</span><b style="font-size:15px">'+esc(m.n)+'</b><span class="muted">'+esc(m.d)+'</span><div class="row" style="margin-top:auto">'+st(m.s,m.s==='Running'?'ok':'warn')+'<span class="sp faint">'+esc(m.by)+'</span></div></div>'}).join('')+'</div>';
  h+='<section class="panel" style="margin-top:16px"><header><h2>Why missions are safe to hand out</h2></header><div class="rows"><div>Jani <b>reads</b> freely, <b>asks</b> before it changes or sends anything, and is <b>never</b> allowed to do what the mission rules forbid.</div><div>IT signs the checks. Nobody, including Jani, can change a signed check from the chat.</div><div>Every mission needs a named person to sign it off, and IT to approve its access, before it can run.</div></div></section>';
  view.innerHTML=h;$('#newM').onclick=function(){S.wiz={step:0,d:{name:'',goal:'',people:['Onboarding officer'],sources:['Core banking (read-only)'],perm:{read:'Allowed',draft:'Allowed',send:'Asks each time',change:'Never'},checks:['Records agree across systems'],approver:'Branch manager',measure:''}};renderMissions()};
}
function renderWizard(){
  var w=S.wiz,d=w.d,h='<div class="page-head"><div><h1>Create a mission</h1><p class="lead">Describe the job in your own words. Jani turns it into rules a person can review.</p></div><div class="act"><button class="btn quiet" id="wx">Cancel</button></div></div><div class="steps" aria-label="Steps">'+WIZ.map(function(s,i){return '<span class="s '+(i===w.step?'on':i<w.step?'done':'')+'">'+(i<w.step?'✓ ':'')+s+'</span>'}).join('')+'</div><section class="panel"><div style="padding:20px;max-width:760px">';
  function chk(list,key,name){return '<div class="pick">'+list.map(function(x){var on=d[key].indexOf(x)>=0;return '<label><input type="checkbox" '+(on?'checked':'')+' data-k="'+key+'" data-v="'+esc(x)+'"> <span>'+esc(x)+'</span></label>'}).join('')+'</div>'}
  if(w.step===0)h+='<div class="field"><label for="wn">What should this mission be called?</label><input id="wn" type="text" value="'+esc(d.name)+'" placeholder="For example: Loan file checks"></div><div class="field"><label for="wg">What is the goal, in one sentence?</label><textarea id="wg" placeholder="Check that every new loan file has the documents we require, and flag the ones that do not.">'+esc(d.goal)+'</textarea><span class="hint">Write it the way you would say it to a colleague.</span></div>';
  if(w.step===1)h+='<div class="field"><span class="lb">Who is involved?</span>'+chk(LIB.people,'people')+'<span class="hint">These people will get tasks from this mission.</span></div><div class="field"><label for="wa">Who signs off corrections or messages?</label><select id="wa">'+LIB.people.map(function(p){return '<option'+(p===d.approver?' selected':'')+'>'+p+'</option>'}).join('')+'</select><span class="hint">A different person from the one doing the task.</span></div>';
  if(w.step===2)h+='<div class="field"><span class="lb">What data may Jani read?</span>'+chk(LIB.sources,'sources')+'<span class="hint">Jani can only read. IT sets which branches and fields.</span></div>';
  if(w.step===3)h+='<div class="field"><span class="lb">What may Jani do?</span></div>'+[['read','Read records'],['draft','Draft messages and changes'],['send','Send messages to customers or colleagues'],['change','Change records directly']].map(function(p){return '<div class="row" style="padding:8px 0;border-top:1px solid var(--line)"><span style="flex:1;min-width:160px">'+p[1]+'</span><div class="seg" role="group" aria-label="'+p[1]+'">'+['Allowed','Asks each time','Never'].map(function(v){return '<button type="button" data-perm="'+p[0]+'" data-pv="'+v+'" aria-pressed="'+(d.perm[p[0]]===v)+'">'+v+'</button>'}).join('')+'</div></div>'}).join('')+'<p class="hint faint" style="margin-top:10px">Changing records directly stays off for every mission. Corrections go to a second person.</p>';
  if(w.step===4)h+='<div class="field"><span class="lb">Which checks should run?</span>'+chk(LIB.checks,'checks')+'<span class="hint">These are checks IT has signed. Need one that is not here? Ask IT to add it.</span></div><div class="field"><label for="wm">How will you know it worked?</label><input id="wm" type="text" value="'+esc(d.measure)+'" placeholder="For example: fewer than 2% of files missing a document"></div>';
  if(w.step===5)h+='<h2 style="font-size:18px;margin-bottom:12px">'+esc(d.name||'Untitled mission')+'</h2><p class="muted" style="margin-bottom:14px;max-width:62ch">'+esc(d.goal||'No goal written yet.')+'</p><div class="rows" style="border:1px solid var(--line);border-radius:var(--r-m)"><div><b>Who</b><br>'+esc(d.people.join(', ')||'Nobody yet')+'. Signed off by '+esc(d.approver)+'.</div><div><b>Reads</b><br>'+esc(d.sources.join(', ')||'Nothing')+'.</div><div><b>Jani may</b><br>'+['Read: '+d.perm.read,'Draft: '+d.perm.draft,'Send: '+d.perm.send,'Change records: Never'].join(' · ')+'</div><div><b>Checks</b><br>'+esc(d.checks.join(', ')||'None yet')+'.</div><div><b>Success</b><br>'+esc(d.measure||'Not set yet')+'</div></div><div class="note faint" style="margin-top:12px">'+ico('lock')+' It cannot run until IT approves its access and the named person signs it off.</div>';
  h+='<div class="row" style="margin-top:20px">'+(w.step>0?'<button class="btn" id="wb">Back</button>':'')+'<button class="btn primary sp" id="wn2">'+(w.step===5?'Send for sign-off':'Next')+'</button></div></div></section>';
  view.innerHTML=h;
  $('#wx').onclick=function(){S.wiz=null;renderMissions()};
  $$('[data-k]').forEach(function(c){c.onchange=function(){var k=c.getAttribute('data-k'),v=c.getAttribute('data-v'),a=d[k],i=a.indexOf(v);if(c.checked&&i<0)a.push(v);if(!c.checked&&i>=0)a.splice(i,1)}});
  $$('[data-perm]').forEach(function(b){b.onclick=function(){d.perm[b.getAttribute('data-perm')]=b.getAttribute('data-pv');renderWizard()}});
  var wb=$('#wb');if(wb)wb.onclick=function(){save();w.step--;renderWizard()};
  function save(){var n=$('#wn'),g=$('#wg'),a=$('#wa'),m=$('#wm');if(n)d.name=n.value;if(g)d.goal=g.value;if(a)d.approver=a.value;if(m)d.measure=m.value}
  $('#wn2').onclick=function(){save();if(w.step===0&&!d.name.trim()){toast('Give the mission a name first.');$('#wn').focus();return}
    if(w.step===5){S.missions.push({n:d.name,s:'Waiting for sign-off',d:d.goal||'Draft mission.',ic:'layers',by:'Draft by '+role().name});S.wiz=null;toast('Sent. IT and '+d.approver+' will review it.');renderMissions();return}
    w.step++;renderWizard()};
}

/* ---------- about ---------- */
function renderAbout(){
  view.innerHTML='<div class="page-head"><div><h1>About this design</h1><p class="lead">A prototype for people who are not technical. It shows how one engine can serve many jobs without anyone needing to learn how it works.</p></div></div>'+
  '<div class="grid g2"><section class="panel"><header><h2>The brief</h2></header><div class="rows"><div><b>Who it is for.</b> An onboarding officer, an accountant, a data analyst, a supervisor, a manager, a regional manager, a chief executive. Each should see only what they need, in words they use.</div><div><b>What they feel.</b> Calm and in control. Never watched, never left guessing what the software is doing.</div><div><b>What it must never do.</b> Make someone approve something they cannot judge, or claim an outcome that is not certain.</div></div></section><section class="panel"><header><h2>How it behaves</h2></header><div class="rows"><div><b>A run, not a chat.</b> Every piece of work shows its state, a plan, what happened, and a clear stop before anything changes.</div><div><b>Reads are free, actions wait.</b> Anything that writes, sends or spends stops at a card that shows exactly what will happen.</div><div><b>Jani assigns, people decide.</b> Tasks arrive with a reason, and can be handed on.</div><div><b>Everything leaves proof</b> in plain language, with technical detail one click away for auditors.</div></div></section></div>'+
  '<section class="panel" style="margin-top:16px"><header><h2>What is not real</h2></header><div class="rows"><div>All names, branches, numbers and results are made up. Nothing here comes from a bank.</div><div>No user research has been done. The scenarios are illustrative, and the design has not been tested with the people it is for.</div><div>The runs are scripted so you can feel the flow. The real product will run on your data and show where each number came from.</div></div></section>';
}

/* ---------- router ---------- */
function renderPage(soft){
  var p=S.page;if(!soft)view.classList.remove('fill');
  if(p!=='work'){S.cur=null;view.classList.remove('fill')}
  if(p==='home'){var r=role();view.innerHTML=r.home==='doer'?homeDoer():r.home==='team'?homeTeam():r.home==='region'?homeRegion():homeCeo();bindHome()}
  else if(p==='tasks')renderTasks();else if(p==='work')renderWork();else if(p==='team')renderTeam();else if(p==='results')renderResults();
  else if(p==='map')renderMap();else if(p==='proof')renderProof();else if(p==='missions')renderMissions();else if(p==='about')renderAbout();
}
function render(){renderRail();renderTop();renderPage()}
function route(){
  var h=(location.hash||'#home').slice(1);if(!TITLES[h])h='home';
  if(h!=='about'&&ROLES[S.role].nav.indexOf(h)<0)h='home';
  S.page=h;render();if(h!=='work')view.scrollTop=0;
}
window.addEventListener('hashchange',route);
window.addEventListener('resize',function(){if(S.page==='map')drawEdges(MAPS[S.mapK],S.mapSel)});
var th=store('jani.theme');if(th==='dark'||th==='light')document.documentElement.setAttribute('data-theme',th);
route();
})();
