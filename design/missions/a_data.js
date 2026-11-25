(function(){
'use strict';
var $=function(s,r){return (r||document).querySelector(s)};
var $$=function(s,r){return Array.prototype.slice.call((r||document).querySelectorAll(s))};
var esc=function(s){return String(s).replace(/[&<>"']/g,function(c){return({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'})[c]})};
var ico=function(n,c){return '<svg class="i '+(c||'')+'" aria-hidden="true"><use href="#i-'+n+'"/></svg>'};
var reduce=window.matchMedia&&window.matchMedia('(prefers-reduced-motion: reduce)').matches;
function store(k,v){try{if(v===undefined)return localStorage.getItem(k);localStorage.setItem(k,v)}catch(e){return null}}

/* ---------- who is looking ---------- */
var ROLES={
 officer:{name:'Amina Wanjiru',ini:'AW',title:'Onboarding officer',where:'Riverside branch',home:'doer',script:'recon',nav:['home','work','tasks','map','proof'],verb:'Reconcile customer records'},
 accountant:{name:'Grace Mwangi',ini:'GM',title:'Accountant',where:'Finance operations',home:'doer',script:'ledger',nav:['home','work','tasks','map','proof'],verb:'Match the bank statement'},
 analyst:{name:'Daniel Kiptoo',ini:'DK',title:'Data analyst',where:'Insights team',home:'doer',script:'report',nav:['home','work','tasks','map','proof'],verb:'Produce the weekly report'},
 cs:{name:'Faith Njeri',ini:'FN',title:'Customer service supervisor',where:'Riverside contact centre',home:'team',script:'triage',nav:['home','work','tasks','team','map','proof'],verb:'Triage complaints'},
 ops:{name:'Peter Ochieng',ini:'PO',title:'Operations manager',where:'Operations',home:'team',script:'exceptions',nav:['home','work','tasks','team','results','map','missions','proof'],verb:'Clear overdue exceptions'},
 region:{name:'Lucy Achieng',ini:'LA',title:'Regional manager',where:'Lakes region, 6 branches',home:'region',script:null,nav:['home','work','team','results','map','proof'],verb:'Ask about the region'},
 ceo:{name:'Samuel Kariuki',ini:'SK',title:'Chief executive',where:'Whole bank',home:'ceo',script:null,nav:['home','work','results','map','proof'],verb:'Ask about the bank'}
};
var NAVS={home:['Home','home'],work:['Work','chat'],tasks:['Tasks','tasks'],team:['Team','team'],results:['Results','chart'],map:['How it fits','map'],missions:['Missions','layers'],proof:['Proof','shield'],about:['About','info']};
var TITLES={home:['Home','Where things stand today'],work:['Work','Jani works, you steer and decide'],tasks:['Tasks','What Jani has lined up, and why'],team:['Team','Who is doing what, and who needs help'],results:['Results','Outcomes in plain numbers'],map:['How it fits','Follow one piece of work from data to proof'],missions:['Missions','The jobs Jani can take on'],proof:['Proof','Who did what, and that nothing was altered'],about:['About this design','Why it looks and behaves this way']};

/* ---------- tasks Jani has assigned ---------- */
var TASKS=[
 {id:'t1',role:'officer',script:'recon',title:'Reconcile customer records, Riverside branch',mission:'Customer records',state:'Ready',due:'Today, 3:00 pm',est:'About 12 minutes',why:'You own Riverside onboarding. 214 customers were added since the last check, and the audit pack is due at 3 pm. This is the most urgent thing on your list, and you have the right access.'},
 {id:'t2',role:'officer',title:'Chase 5 customers to confirm a phone number',mission:'Customer records',state:'Waiting for customers',due:'Thursday',est:'Jani drafts, you send',why:'These come from the check above. Jani will draft the messages once you have approved the corrections.'},
 {id:'t3',role:'officer',title:'Read your checker’s notes on last week’s corrections',mission:'Customer records',state:'Ready',due:'Friday',est:'5 minutes',why:'Joseph Otieno left 3 notes. Reading them first helps Jani match your style next time.'},
 {id:'t4',role:'accountant',script:'ledger',title:'Match the month-end bank statement to the ledger',mission:'Month-end close',state:'Ready',due:'Today, 5:00 pm',est:'About 10 minutes',why:'Month-end is tomorrow. The statement arrived this morning and you are the preparer for this account.'},
 {id:'t5',role:'accountant',title:'Prepare the accruals schedule',mission:'Month-end close',state:'Ready',due:'Tomorrow',est:'About 20 minutes',why:'It depends on the bank match, so Jani lined it up second.'},
 {id:'t6',role:'analyst',script:'report',title:'Weekly account-closure report for Operations',mission:'Weekly insight pack',state:'Ready',due:'Today, 4:00 pm',est:'About 8 minutes',why:'You produced this report the last three weeks. Jani knows the format and noticed closures rose this week.'},
 {id:'t7',role:'analyst',title:'Refresh the branch scorecard data',mission:'Weekly insight pack',state:'Ready',due:'Friday',est:'About 5 minutes',why:'Scheduled weekly. Jani runs it after the closure report.'},
 {id:'t8',role:'cs',script:'triage',title:'Triage 38 open complaints',mission:'Complaint handling',state:'Ready',due:'Today, 2:00 pm',est:'About 10 minutes',why:'6 complaints have waited more than 5 days. Customers have not heard from us in 3 days.'},
 {id:'t9',role:'cs',title:'Coach Kevin on card-decline calls',mission:'Complaint handling',state:'Ready',due:'Friday',est:'30 minutes, with you',why:'Kevin’s card-decline calls take twice as long as the team’s. Jani only suggests; coaching is yours.'},
 {id:'t10',role:'ops',script:'exceptions',title:'Clear the 12 overdue exceptions',mission:'Customer records',state:'Ready',due:'Today',est:'About 8 minutes',why:'12 exceptions passed their due date. Seven are waiting on one approver.'},
 {id:'t11',role:'ops',title:'Review the draft mission: Loan file checks',mission:'Missions',state:'Needs you',due:'Thursday',est:'15 minutes',why:'Faith drafted it and sent it to you for sign-off before IT reviews it.'},
 {id:'t12',role:'region',title:'Decide on temporary cover for Northgate',mission:'Customer records',state:'Needs you',due:'This week',est:'5 minutes',why:'Northgate has 31 open exceptions, the most in your region. Jani prepared two options.'},
 {id:'t13',role:'ceo',title:'Decide how much to invest in the second mission wave',mission:'All missions',state:'Needs you',due:'Next board',est:'10 minutes',why:'Jani summarised results and risks. The choice is about money and people, so it stays with you.'}
];
var TEAMS={
 cs:[['Brian Mutua','BM',6,8,'Working'],['Esther Wairimu','EW',3,8,'Has capacity'],['Hassan Ali','HA',8,8,'At capacity'],['Mercy Atieno','MA',7,8,'Working'],['Kevin Omondi','KO',9,8,'Over capacity']],
 ops:[['Amina Wanjiru','AW',3,6,'Working'],['Grace Mwangi','GM',2,6,'Has capacity'],['Daniel Kiptoo','DK',2,6,'Has capacity'],['Faith Njeri','FN',5,6,'Working'],['Joseph Otieno','JO',9,6,'Over capacity']],
 region:[['Riverside','RV',214,240,'On track'],['Northgate','NG',31,20,'Behind'],['Lakeview','LV',12,20,'On track'],['Eastgate','EG',9,20,'On track'],['Lakeside','LS',17,20,'Watch'],['Hillcrest','HC',5,20,'On track']]
};

/* ---------- the map: how pieces fit ---------- */
var MAPS={
 recon:{name:'Customer records check',cols:['Where the data lives','What Jani does','Who decides','What changes','Proof'],nodes:[
  {id:'a1',c:0,t:'Core banking',s:'Customer records · read-only',st:'Connected',d:'Jani can read customer details here. It can never write to this system.'},
  {id:'a2',c:0,t:'Onboarding forms',s:'KYC capture · read-only',st:'Connected',d:'The forms customers filled in when they joined.'},
  {id:'b1',c:1,t:'Compare records',s:'7 signed checks',st:'Runs by itself',d:'Compares each customer across systems using checks IT has reviewed and signed. Jani cannot change a check.'},
  {id:'b2',c:1,t:'Explain differences',s:'Plain-language reasons',st:'Runs by itself',d:'Jani writes why a record does not match and suggests a fix. It asks you when a match is unclear.'},
  {id:'c1',c:2,t:'Onboarding officer',s:'Reviews each suggestion',st:'You',roles:['officer'],d:'Accepts or rejects suggestions, and answers Jani’s questions. This is the first of two people who must agree.'},
  {id:'c2',c:2,t:'Checker',s:'A second person approves',st:'Joseph Otieno',roles:[],d:'Approves every correction. The officer cannot approve their own work.'},
  {id:'c3',c:2,t:'Branch and operations leads',s:'See progress, step in',st:'Watching',roles:['cs','ops','region'],d:'Supervisors see what is stuck and can reassign work.'},
  {id:'d1',c:3,t:'Bank’s own approval workflow',s:'Applies approved corrections',st:'Waits for checker',d:'Corrections are applied by the bank’s existing process, not by Jani.'},
  {id:'e1',c:4,t:'Evidence pack',s:'Checks, totals, who decided',st:'Written each run',roles:['officer'],d:'Everything needed for an audit, in plain language.'},
  {id:'e2',c:4,t:'Audit journal',s:'Cannot be altered quietly',st:'Verified',roles:['ceo','region'],d:'Every read, check and decision is recorded and protected so changes would show.'}
 ],links:[['a1','b1'],['a2','b1'],['b1','b2'],['b2','c1'],['c1','c2'],['c2','d1'],['c1','c3'],['d1','e1'],['c2','e2'],['c1','e1']]},
 ledger:{name:'Month-end close',cols:['Where the data lives','What Jani does','Who decides','What changes','Proof'],nodes:[
  {id:'a1',c:0,t:'Bank statement',s:'Uploaded each month',st:'Received',d:'The statement from the bank, read-only.'},
  {id:'a2',c:0,t:'General ledger',s:'Finance system · read-only',st:'Connected',d:'The company’s own books.'},
  {id:'b1',c:1,t:'Match line by line',s:'Amount, date, reference',st:'Runs by itself',d:'Matches exactly first, then by amount and date.'},
  {id:'b2',c:1,t:'Prepare adjustments',s:'Draft journal entries',st:'Drafts only',d:'Drafts entries for the differences. Never posts them.'},
  {id:'c1',c:2,t:'Accountant',s:'Reviews and sends',st:'You',roles:['accountant'],d:'Checks each difference and sends adjustments for approval.'},
  {id:'c2',c:2,t:'Finance controller',s:'Approves postings',st:'Second person',roles:[],d:'Approves every journal entry.'},
  {id:'d1',c:3,t:'Ledger postings',s:'Done by Finance',st:'Waits for controller',d:'Posted through the normal finance process.'},
  {id:'e1',c:4,t:'Reconciliation report',s:'Signed off for the month',st:'Written each run',roles:['accountant','ops'],d:'The month-end reconciliation, ready for audit.'}
 ],links:[['a1','b1'],['a2','b1'],['b1','b2'],['b2','c1'],['c1','c2'],['c2','d1'],['d1','e1'],['c1','e1']]},
 report:{name:'Weekly insight pack',cols:['Where the data lives','What Jani does','Who decides','What changes','Proof'],nodes:[
  {id:'a1',c:0,t:'Account data',s:'Read-only',st:'Connected',d:'Closures, openings and balances.'},
  {id:'b1',c:1,t:'Check the numbers',s:'Totals agree with the ledger',st:'Runs by itself',d:'Jani checks its own inputs against the books before analysing.'},
  {id:'b2',c:1,t:'Analyse and draft',s:'Finds what changed',st:'Drafts only',d:'Writes the finding first, with where each number came from.'},
  {id:'c1',c:2,t:'Data analyst',s:'Reviews and publishes',st:'You',roles:['analyst'],d:'Confirms the story is right before anyone else sees it.'},
  {id:'d1',c:3,t:'Weekly Ops pack',s:'41 readers',st:'Waits for you',d:'Publishing is outside the team, so it waits for your approval.'},
  {id:'e1',c:4,t:'Sources list',s:'Every number linked',st:'Written each run',roles:['analyst','ceo'],d:'Each figure links back to the query that made it.'}
 ],links:[['a1','b1'],['b1','b2'],['b2','c1'],['c1','d1'],['c1','e1']]},
 triage:{name:'Complaint handling',cols:['Where the data lives','What Jani does','Who decides','What changes','Proof'],nodes:[
  {id:'a1',c:0,t:'Complaints log',s:'38 open · read-only',st:'Connected',d:'Every open complaint, with dates.'},
  {id:'b1',c:1,t:'Group and rank',s:'By cause and urgency',st:'Runs by itself',d:'Finds the common causes and who has waited longest.'},
  {id:'b2',c:1,t:'Draft holding replies',s:'Short SMS drafts',st:'Drafts only',d:'Drafts messages. Never sends them.'},
  {id:'c1',c:2,t:'Customer service supervisor',s:'Approves sending',st:'You',roles:['cs'],d:'Decides who handles what and whether messages go out.'},
  {id:'d1',c:3,t:'Customers',s:'SMS holding replies',st:'Waits for you',d:'Sent only after you approve. Cannot be recalled.'},
  {id:'e1',c:4,t:'Handling record',s:'Who was told what',st:'Written each run',roles:['cs','ops'],d:'A record of every message sent and who approved it.'}
 ],links:[['a1','b1'],['b1','b2'],['b2','c1'],['c1','d1'],['c1','e1']]}
};

/* ---------- scripted runs ---------- */
var SC={};
SC.recon={goal:'Check that 214 customers at Riverside have matching records, and prepare corrections for any that do not.',by:'Assigned by Jani',
 plan:['Collect the records','Compare the records','Explain what does not match','Prepare corrections for approval','Write the evidence pack'],
 intro:'I will read customer records for Riverside only, and I will not change anything. Anything that would change a record comes to you first, then to Joseph Otieno as checker.',
 ctx:{src:[['Core banking, Riverside','Read-only'],['Onboarding forms','Read-only'],['Signed check set v3','Set by IT']],
  allow:[['ok','Allowed','Read Riverside customer records'],['ok','Allowed','Compare and explain differences'],['warn','Asks each time','Send corrections for approval'],['bad','Never','Change a record directly'],['bad','Never','Read other branches']],
  est:12,limits:[['Time',30,'min'],['Records read',214,'']],mem:['Amina prefers corrections grouped by customer','Phone numbers use the 07xx format']},
 ev:[
  {k:'tool',p:0,t:'Read customer records',d:'Core banking · Riverside · 214 customers · read-only · 38 s',ms:900},
  {k:'tool',p:0,t:'Read onboarding forms',d:'KYC capture · 214 forms · read-only · 21 s',ms:700,done:0},
  {k:'tool',p:1,t:'Compare records',d:'7 signed checks across 214 customers · 12.4 s',ms:1000,done:1},
  {k:'say',p:2,x:'196 customers have records that agree everywhere. 18 need attention: 11 are simple typing differences [1], 5 need the customer to confirm a phone number [2], and 2 look like the same person recorded twice [3].'},
  {k:'ask',p:2,q:'Two customers look like the same person. What should I do?',o:['Treat as one customer and ask the checker to merge','Keep both and add a note','Leave them for me to review'],reply:function(a){return 'Thanks. I will '+a.charAt(0).toLowerCase()+a.slice(1)+'. I saved your choice to this task so the checker can see it.'}},
  {k:'tool',p:2,t:'Explain each difference',d:'11 explanations written · 6.1 s',ms:800,done:2},
  {k:'gate',p:3,title:'Needs you',what:'Send 11 corrections to Joseph Otieno for approval.',facts:[['Customers','11'],['Fields changed','13'],['Biggest change','ID digits swapped'],['Goes to','Joseph Otieno']],
   cols:['Customer','Field','Now','Proposed'],rows:[['C-10482','ID number','ending 2178','ending 2187'],['C-10517','Phone','0711 220 49','0711 220 449'],['C-10533','Surname','Kamau','Kamau-Njoroge'],['C-10561','Date of birth','12 Mar 1991','21 Mar 1991']],
   conseq:'Nothing changes in the bank’s systems until Joseph approves. You can withdraw these any time before then.',small:'Send 3 first',all:'Send all 11 for approval',no:'Not now',
   after:{all:'Sent 11 corrections to Joseph Otieno. He will see them in his approvals, and I will tell you when he decides.',small:'Sent the first 3 corrections to Joseph. The other 8 are saved as drafts until you say so.',no:'Understood. I kept all 11 as drafts in this task. Nothing was sent.'}},
  {k:'tool',p:4,t:'Write the evidence pack',d:'Checks run, totals, who decided what · 2.0 s',ms:700,done:3},
  {k:'result',title:'Riverside check complete',stats:function(v){return [['Customers checked','214'],['Records agree','196'],['Corrections sent',v.sent],['Need the customer','5'],['Possible duplicates','2']]},note:'Jani explained the differences. The signed checks and two people decide, so this is verified, not guessed.',link:'proof'}
 ]};
SC.ledger={goal:'Match the month-end bank statement to the ledger, and prepare adjustments for the differences.',by:'Assigned by Jani',
 plan:['Read the statement and ledger','Match line by line','Explain the differences','Prepare adjustments for approval','Write the evidence pack'],
 intro:'I will read the statement and the ledger and I will not post anything. Adjustments go to you, then to the Finance controller.',
 ctx:{src:[['Bank statement, month end','Read-only'],['General ledger','Read-only']],allow:[['ok','Allowed','Read the statement and ledger'],['ok','Allowed','Match and explain'],['warn','Asks each time','Send adjustments for approval'],['bad','Never','Post to the ledger']],est:10,limits:[['Time',30,'min'],['Lines matched',1842,'']],mem:['Cheques usually clear within 2 working days','Bank charges post on the last working day']},
 ev:[
  {k:'tool',p:0,t:'Read bank statement and ledger',d:'1,842 statement lines · 1,851 ledger entries · read-only · 29 s',ms:900},
  {k:'tool',p:1,t:'Match line by line',d:'1,825 exact · 12 by amount and date · 5 unmatched · 8.8 s',ms:1000,done:0},
  {k:'say',p:2,x:'17 lines do not match, a difference of KES 38,400.00. Most are timing: cheques that have not cleared yet [1] and bank charges not yet posted [2].'},
  {k:'ask',p:2,q:'Cheque 004417 for KES 25,000.00 cleared one day after month end. Treat it as timing?',o:['Yes, timing, no adjustment','No, post an adjustment'],reply:function(a){return 'Understood: '+a.toLowerCase()+'. I will remember this for next month.'}},
  {k:'tool',p:3,t:'Draft journal adjustments',d:'9 adjustments drafted · total KES 13,400.00 · 3.3 s',ms:800,done:2},
  {k:'gate',p:3,title:'Needs you',what:'Send 9 journal adjustments to the Finance controller for approval.',facts:[['Adjustments','9'],['Total','KES 13,400'],['Largest','KES 5,200'],['Goes to','Finance controller']],
   cols:['Reference','Account','Debit','Credit'],rows:[['ADJ-01','Bank charges','KES 5,200','-'],['ADJ-02','Interest income','-','KES 1,150'],['ADJ-03','Bank charges','KES 760','-']],
   conseq:'Nothing is posted until the controller approves. You can withdraw these before then.',small:'Send 3 first',all:'Send all 9 for approval',no:'Not now',
   after:{all:'Sent 9 adjustments to the Finance controller.',small:'Sent the first 3. The other 6 are saved as drafts.',no:'Kept all 9 as drafts. Nothing was sent.'}},
  {k:'tool',p:4,t:'Write the reconciliation report',d:'Matched, unmatched, adjustments · 1.9 s',ms:700,done:3},
  {k:'result',title:'Month-end match complete',stats:function(v){return [['Lines matched','1,837 of 1,842'],['Timing differences','8'],['Adjustments sent',v.sent],['Still open','0']]},note:'Verified by signed matching rules and a second person.',link:'proof'}
 ]};
SC.report={goal:'Produce the weekly account-closure report for Operations.',by:'Assigned by Jani',
 plan:['Pull closure data','Check the numbers','Find what changed','Draft the report','Publish after approval'],
 intro:'I will read account data only. The report stays a draft until you approve publishing it.',
 ctx:{src:[['Account closures, last 8 weeks','Read-only'],['General ledger totals','Read-only']],allow:[['ok','Allowed','Read account data'],['ok','Allowed','Draft the report'],['warn','Asks each time','Publish to the weekly pack'],['bad','Never','Change any account']],est:8,limits:[['Time',30,'min'],['Rows read',2031,'']],mem:['Report region means the branch’s region','Lead with the change, then the cause']},
 ev:[
  {k:'ask',p:0,q:'Should “region” mean the branch’s region or the customer’s home region?',o:['Branch’s region','Customer’s home region'],reply:function(a){return 'Using '+a.toLowerCase()+'. I will remember this for next week.'}},
  {k:'tool',p:0,t:'Pull closure data',d:'2,031 closures · last 8 weeks · read-only · 11 s',ms:800},
  {k:'tool',p:1,t:'Check the numbers',d:'Totals agree with the ledger: 2,031 · 1.2 s',ms:700,done:0},
  {k:'say',p:2,x:'Closures rose 14% this week: 212, up from 186 [1]. Northgate (+11) and Lakeview (+9) account for 20 of the 26 extra closures [2].'},
  {k:'data',p:3,title:'Closures by branch, this week',cols:['Branch','This week','Last week','Change'],rows:[['Northgate','31','20','+11'],['Lakeview','29','20','+9'],['Riverside','38','37','+1'],['Eastgate','44','45','-1'],['Lakeside','40','38','+2'],['Hillcrest','30','26','+4']]},
  {k:'gate',p:4,title:'Needs you',what:'Publish this report to the weekly Operations pack.',facts:[['Readers','41'],['Goes to','Weekly Ops pack'],['Sources','2 linked'],['Draft by','Jani']],
   cols:['Section','Status'],rows:[['Headline finding','Ready'],['Closures by branch','Ready'],['Sources','2 linked']],
   conseq:'41 people will see it. You can unpublish, but they may already have read it.',small:'Send to me only',all:'Publish to 41 readers',no:'Not now',
   after:{all:'Published to the weekly pack. 41 readers can see it.',small:'Sent a copy to you only. Nothing was published.',no:'Kept it as a draft.'}},
  {k:'result',title:'Weekly report done',stats:function(v){return [['Closures this week','212'],['Change','+14%'],['Published',v.sent==='0'?'No':(v.small?'To you only':'41 readers')]]},note:'Every figure links to the query that produced it.',link:'proof'}
 ]};
SC.triage={goal:'Sort the 38 open complaints, and send holding replies to customers who have not heard from us in 3 days.',by:'Assigned by Jani',
 plan:['Read open complaints','Group by cause','Rank by urgency','Draft holding replies','Send after approval'],
 intro:'I will read the complaints log only. Messages to customers are drafts until you approve them.',
 ctx:{src:[['Complaints log, 38 open','Read-only'],['Staff rota','Read-only']],allow:[['ok','Allowed','Read complaints'],['ok','Allowed','Group, rank and draft'],['warn','Asks each time','Send SMS to customers'],['bad','Never','Close a complaint']],est:10,limits:[['Time',30,'min'],['SMS sent',24,'']],mem:['Holding replies are 2 sentences, no promises','Card declines go to Hassan first']},
 ev:[
  {k:'tool',p:0,t:'Read open complaints',d:'38 open · read-only · 4.0 s',ms:800},
  {k:'say',p:1,x:'14 complaints are about failed mobile transfers, 9 about card declines, 8 about statement errors and 7 about other things [1]. 6 have waited more than 5 days.'},
  {k:'tool',p:2,t:'Rank by urgency',d:'Waiting time, amount involved, repeat contacts · 1.1 s',ms:800,done:1},
  {k:'ask',p:2,q:'The 6 oldest complaints are the hardest. Who should take them?',o:['Me','Esther Wairimu (she has capacity)'],reply:function(a){return 'Assigned the 6 oldest to '+(a==='Me'?'you':'Esther Wairimu')+'.'}},
  {k:'tool',p:3,t:'Draft holding replies',d:'24 replies drafted · 2.4 s',ms:800,done:2},
  {k:'gate',p:4,title:'Needs you',what:'Send a holding SMS to 24 customers who have not heard from us in 3 days.',facts:[['Customers','24'],['Channel','SMS'],['Cost','About KES 120'],['Can recall','No']],
   cols:['Customer','Message'],rows:[['C-20411','We are still working on your transfer and will update you by tomorrow.'],['C-20419','Your card query is with our cards team. We will update you by tomorrow.']],
   conseq:'Text messages cannot be recalled once sent.',small:'Send 1 test to my phone',all:'Send to all 24',no:'Not now',
   after:{all:'Sent 24 holding messages.',small:'Sent 1 test message to your phone. The other 23 wait for your go-ahead.',no:'Kept the 24 messages as drafts.'}},
  {k:'result',title:'Complaints sorted',stats:function(v){return [['Open complaints','38'],['Oldest, assigned','6'],['Messages sent',v.sent==='0'?'0':(v.small?'1 test':'24')]]},note:'The handling record shows who was told what, and who approved it.',link:'proof'}
 ]};
SC.exceptions={goal:'Clear the 12 overdue exceptions across Operations.',by:'Assigned by Jani',
 plan:['Read overdue exceptions','Find the cause of each','Match each to someone with capacity','Prepare reassignments','Apply after approval'],
 intro:'I will read exceptions and the team’s workload. Reassigning work waits for your approval.',
 ctx:{src:[['Exceptions queue, 12 overdue','Read-only'],['Team workload','Read-only']],allow:[['ok','Allowed','Read the queue and workload'],['ok','Allowed','Suggest reassignments'],['warn','Asks each time','Reassign work'],['bad','Never','Approve on someone’s behalf']],est:8,limits:[['Time',30,'min']],mem:['Approvals over 3 days old go to a second approver']},
 ev:[
  {k:'tool',p:0,t:'Read overdue exceptions',d:'12 overdue · read-only · 3.2 s',ms:800},
  {k:'say',p:1,x:'7 are waiting on one checker, 3 are waiting for customers and 2 are stuck on a missing document [1]. Joseph Otieno has 9 approvals open, more than anyone [2].'},
  {k:'tool',p:2,t:'Match to people with capacity',d:'Workload and access checked · 1.4 s',ms:800,done:1},
  {k:'gate',p:3,title:'Needs you',what:'Move 7 approvals from Joseph Otieno to Mary Chebet and Ali Noor.',facts:[['Approvals moved','7'],['From','Joseph Otieno'],['To','Mary, Ali'],['Overdue after','5']],
   cols:['Approval','From','To'],rows:[['EX-3301','Joseph','Mary'],['EX-3304','Joseph','Ali'],['EX-3308','Joseph','Mary']],
   conseq:'The new approvers are told straight away. You can undo this in Team.',small:'Move 3 first',all:'Move all 7',no:'Not now',
   after:{all:'Moved 7 approvals. Mary Chebet and Ali Noor have been told.',small:'Moved 3 approvals. The other 4 stay with Joseph for now.',no:'Nothing was moved.'}},
  {k:'result',title:'Overdue exceptions reduced',stats:function(v){return [['Overdue before','12'],['Overdue after',v.sent==='0'?'12':(v.small?'9':'5')],['Approvals moved',v.sent]]},note:'The audit journal records who moved what, and who approved it.',link:'proof'}
 ]};
/* quick questions for people who ask rather than run */
var ASKS={
 region:[
  {q:'Which branches are behind this week?',a:'Northgate is behind: 31 open exceptions against a target of 20 [1]. Lakeside is close to its limit with 17. The other four are on track.',data:{title:'Open exceptions by branch',cols:['Branch','Open','Target','Status'],rows:[['Northgate','31','20','Behind'],['Lakeside','17','20','Watch'],['Lakeview','12','20','On track'],['Eastgate','9','20','On track'],['Hillcrest','5','20','On track'],['Riverside','4','20','On track']]},src:['Exceptions queue, region','Branch targets']},
  {q:'What needs my decision?',ask:{q:'Northgate needs help. What should I prepare?',o:['Temporary cover: 2 officers for 2 weeks','Get a cost estimate first','Not now']},src:['Exceptions queue, region','Staffing plan']}
 ],
 ceo:[
  {q:'How are the missions doing?',a:'Four missions are running across 6 branches. Customer records is ahead of plan. Month-end close is on plan. The weekly insight pack saved the most time [1].',data:{title:'Missions this quarter (sample)',cols:['Mission','Status','Time saved','Second-person checks'],rows:[['Customer records','Ahead','610 h','100%'],['Month-end close','On plan','290 h','100%'],['Weekly insight pack','Ahead','240 h','100%'],['Complaint handling','On plan','100 h','100%']]},src:['Mission results, quarter','Audit journal']},
  {q:'What are the biggest risks?',a:'Two risks stand out [1]. Joseph Otieno is the only approver for 9 open corrections, which slows work and concentrates risk. And Northgate has the most overdue exceptions in the bank. Neither involves a data problem; both are about people and capacity.',src:['Approvals queue','Exceptions queue']},
  {q:'What needs my decision?',ask:{q:'How much should we invest in the second wave of missions?',o:['Fund two more missions now','Wait for another quarter of results','Fund one, and review in 8 weeks']},src:['Mission results, quarter','Budget']}
 ]
};
/* proof log */
var PROOF=[
 {t:'10:42',who:'Amina Wanjiru',k:'dec',x:'sent 11 corrections to Joseph Otieno for approval.',tech:'run 7f3a · decision recorded · entry 1,284'},
 {t:'10:41',who:'Jani',k:'q',x:'asked Amina about two possible duplicate customers. She chose to keep both and add a note.',tech:'run 7f3a · question answered · entry 1,283'},
 {t:'10:38',who:'Jani',k:'read',x:'read 214 customer records and 214 onboarding forms at Riverside. Read-only.',tech:'run 7f3a · 2 reads · entries 1,281 to 1,282'},
 {t:'10:30',who:'Joseph Otieno',k:'chg',x:'approved 4 corrections from last week. The bank’s workflow applied them.',tech:'approval AP-221 · applied by workflow · entry 1,276'},
 {t:'09:55',who:'Jani',k:'read',x:'compared 7 signed checks across 198 customers at Northgate.',tech:'run 7e11 · signed check set v3 · entry 1,271'},
 {t:'09:12',who:'Faith Njeri',k:'dec',x:'approved sending 18 holding messages to customers.',tech:'run 7d92 · decision recorded · entry 1,262'},
 {t:'Yesterday',who:'Peter Ochieng',k:'dec',x:'signed off the mission “Complaint handling”.',tech:'mission signed · IT review passed · entry 1,198'},
 {t:'Yesterday',who:'Grace Mwangi',k:'chg',x:'month-end adjustments approved by the Finance controller and posted.',tech:'approval AP-219 · entry 1,181'}
];
var BRANCH=[
 ['Riverside',214,4,1.2,'On track',-2],['Northgate',198,31,3.4,'Behind',9],['Lakeview',176,12,1.6,'On track',-1],['Eastgate',231,9,1.4,'On track',-3],['Lakeside',187,17,2.3,'Watch',2],['Hillcrest',142,5,1.1,'On track',-1]
];
window.__D={$:$,$$:$$,esc:esc,ico:ico,reduce:reduce,store:store,ROLES:ROLES,NAVS:NAVS,TITLES:TITLES,TASKS:TASKS,TEAMS:TEAMS,MAPS:MAPS,SC:SC,ASKS:ASKS,PROOF:PROOF,BRANCH:BRANCH};
})();
