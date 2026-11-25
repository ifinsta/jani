(function(){
'use strict';
var D=window.__D,$=D.$,$$=D.$$,esc=D.esc,ico=D.ico,reduce=D.reduce,store=D.store,ROLES=D.ROLES,SC=D.SC,ASKS=D.ASKS;
var S=window.__S={role:store('jani.role')&&D.ROLES[store('jani.role')]?store('jani.role'):'officer',page:'home',runs:{},mem:{},proofF:'all',mapK:'recon',mapSel:null,missions:[
  {n:'Customer records check',s:'Running',d:'Compares customer records across systems.',ic:'db',by:'IT, signed'},
  {n:'Month-end close',s:'Running',d:'Matches the bank statement to the ledger.',ic:'file',by:'IT, signed'},
  {n:'Weekly insight pack',s:'Running',d:'Drafts the weekly Operations report.',ic:'chart',by:'IT, signed'},
  {n:'Complaint handling',s:'Running',d:'Sorts complaints and drafts holding replies.',ic:'chat',by:'IT, signed'},
  {n:'Loan file checks',s:'Waiting for sign-off',d:'Draft by Faith Njeri. Needs Peter, then IT.',ic:'layers',by:'Draft'}
 ],wiz:null,askSeq:0};
var announce=function(t){var l=$('#live');if(l){l.textContent='';setTimeout(function(){l.textContent=t},30)}};
function toast(t){var el=$('#toast');el.textContent=t;el.hidden=false;clearTimeout(toast.t);toast.t=setTimeout(function(){el.hidden=true},2600)}
window.__toast=toast;
function nowTime(){var d=new Date();return d.getHours()+':'+('0'+d.getMinutes()).slice(-2)}

/* ----- run model ----- */
function newRun(key,sc){return {key:key,sc:sc,state:'ready',i:0,plan:(sc.plan||[]).map(function(t){return{t:t,s:'todo'}}),log:[],vars:{sent:'0'},pauseReq:false,timer:null,secs:0}}
function getRun(key,sc){if(!S.runs[key])S.runs[key]=newRun(key,sc);return S.runs[key]}
function later(run,fn,ms){clearTimeout(run.timer);run.timer=setTimeout(fn,reduce?Math.min(ms,120):ms)}
function ui(run){if(S.page==='work'&&S.cur===run)window.__paintRun&&window.__paintRun(run)}
function activate(run,p){run.plan.forEach(function(x,i){if(i<p&&x.s!=='done')x.s='done';if(i===p&&x.s!=='done')x.s='active'})}
function start(run){
  if(run.state!=='ready'&&run.state!=='stopped'&&run.state!=='done')return;
  if(run.state!=='ready'){var fresh=newRun(run.key,run.sc);Object.keys(fresh).forEach(function(k){run[k]=fresh[k]})}
  run.state='running';
  run.log.push({k:'say',x:run.sc.intro,done:true,first:true});
  announce('Started');ui(run);
  later(run,function(){step(run)},700);
}
function step(run){
  if(run.state==='stopped')return;
  if(run.pauseReq){run.pauseReq=false;run.state='paused';announce('Paused');ui(run);return}
  if(run.state!=='running')return;
  var ev=run.sc.ev[run.i];
  if(!ev){finish(run);return}
  if(ev.p!=null)activate(run,ev.p);
  if(ev.k==='say'){
    var e={k:'say',x:'',done:false},words=ev.x.split(' '),n=0;run.log.push(e);
    (function tick(){
      if(run.state==='stopped')return;
      n=reduce?words.length:n+1;e.x=words.slice(0,n).join(' ');ui(run);
      if(n<words.length){run.timer=setTimeout(tick,30)}else{e.done=true;run.i++;ui(run);later(run,function(){step(run)},600)}
    })();return;
  }
  if(ev.k==='tool'){
    var t={k:'tool',t:ev.t,d:ev.d,done:false};run.log.push(t);ui(run);
    later(run,function(){t.done=true;if(ev.done!=null&&run.plan[ev.done])run.plan[ev.done].s='done';run.i++;ui(run);later(run,function(){step(run)},350)},ev.ms||800);return;
  }
  if(ev.k==='data'){run.log.push({k:'data',ev:ev});run.i++;ui(run);later(run,function(){step(run)},900);return}
  if(ev.k==='ask'){run.log.push({k:'ask',ev:ev,ans:null,idx:run.log.length});if(run.plan[ev.p])run.plan[ev.p].s='waiting';run.state='needs';announce('Jani has a question for you');ui(run);return}
  if(ev.k==='gate'){run.log.push({k:'gate',ev:ev,dec:null,idx:run.log.length});if(run.plan[ev.p])run.plan[ev.p].s='waiting';run.state='needs';announce('Needs your decision: '+ev.what);ui(run);return}
  if(ev.k==='result'){finish(run,ev);return}
}
function finish(run,ev){
  clearTimeout(run.timer);run.plan.forEach(function(x){x.s='done'});
  if(ev)run.log.push({k:'result',ev:ev});
  run.state='done';run.i=run.sc.ev.length;announce('Done');ui(run);
  if(window.__refreshBadges)window.__refreshBadges();
}
function stop(run){
  if(run.state!=='running'&&run.state!=='paused'&&run.state!=='needs')return;
  clearTimeout(run.timer);run.state='stopped';run.log.push({k:'say',x:'Stopped. Work already done stays done, and nothing else will run.',done:true});
  announce('Stopped');ui(run);if(window.__refreshBadges)window.__refreshBadges();
}
function pause(run){if(run.state==='running'){run.pauseReq=true;ui(run)}}
function resume(run){if(run.state==='paused'){run.state='running';announce('Resumed');ui(run);later(run,function(){step(run)},300)}}
function countOf(txt){var m=String(txt).match(/\d+/);return m?m[0]:'1'}
function decide(run,idx,choice){
  var g=run.log[idx];if(!g||g.dec||run.state!=='needs')return;
  var ev=g.ev;g.dec={choice:choice,at:nowTime()};
  if(choice==='all'){run.vars.sent=ev.facts[0][1].replace(/\D/g,'')||'1';run.vars.small=false}
  else if(choice==='small'){run.vars.sent=countOf(ev.small);run.vars.small=true}
  else{run.vars.sent='0'}
  run.log.push({k:'say',x:ev.after[choice],done:true});
  run.plan[ev.p]&&(run.plan[ev.p].s=choice==='no'?'done':'done');
  run.state='running';run.i++;ui(run);later(run,function(){step(run)},700);
  announce(choice==='no'?'Not sent':'Sent');
  if(window.__refreshBadges)window.__refreshBadges();
}
function answer(run,idx,text){
  var a=run.log[idx];if(!a||a.ans||run.state!=='needs'||!text)return;
  a.ans=text;var ev=a.ev;
  run.log.push({k:'say',x:ev.reply?ev.reply(text):'Thanks, noted.',done:true});
  if(run.plan[ev.p])run.plan[ev.p].s='active';
  run.state='running';run.i++;ui(run);later(run,function(){step(run)},700);
}
function steer(run,text){
  if(!text)return;
  run.log.push({k:'me',x:text});ui(run);
  setTimeout(function(){run.log.push({k:'say',x:run.state==='running'||run.state==='needs'||run.state==='paused'?'Noted. I will apply that from the next step, and I will say if it changes anything.':'Noted. I will keep that for the next run.',done:true});ui(run)},700);
}
/* ----- ask mode: questions without a plan ----- */
function askRun(role){
  var key='ask-'+role;if(S.runs[key])return S.runs[key];
  var r=newRun(key,{goal:'Ask Jani about '+(role==='ceo'?'the bank':'your region')+'. Answers come from your data, with sources.',by:'You asked',plan:[],intro:'',ctx:{src:[['Mission results','Read-only'],['Exceptions queue','Read-only'],['Audit journal','Read-only']],allow:[['ok','Allowed','Read summaries across branches'],['warn','Asks each time','Prepare a decision for you'],['bad','Never','Change any record or approve anything']],est:0,limits:[],mem:[]},ev:[]});
  r.state='ask';S.runs[key]=r;return r;
}
function askQuestion(run,item){
  run.log.push({k:'me',x:item.q});ui(run);
  setTimeout(function(){
    if(item.ask){
      run.log.push({k:'ask',ev:{p:-1,q:item.ask.q,o:item.ask.o,reply:function(a){return 'Recorded: '+a+'. I will prepare the next steps and tell the people involved. Nothing is committed until you confirm it in Tasks.'}},ans:null,idx:run.log.length});
      run.state='needs-ask';ui(run);return;
    }
    var e={k:'say',x:'',done:false,src:item.src},words=item.a.split(' '),n=0;run.log.push(e);run.srcs=item.src;
    (function tick(){n=reduce?words.length:n+1;e.x=words.slice(0,n).join(' ');ui(run);if(n<words.length)setTimeout(tick,26);else{e.done=true;if(item.data)run.log.push({k:'data',ev:item.data});ui(run)}})();
  },600);
}
function answerAskMode(run,idx,text){
  var a=run.log[idx];if(!a||a.ans||!text)return;a.ans=text;run.log.push({k:'say',x:a.ev.reply(text),done:true});run.state='ask';ui(run);toast('Decision recorded');
}
window.__E={getRun:getRun,start:start,stop:stop,pause:pause,resume:resume,decide:decide,answer:answer,steer:steer,askRun:askRun,askQuestion:askQuestion,answerAskMode:answerAskMode,nowTime:nowTime,announce:announce};
})();
