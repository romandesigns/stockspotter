const fs=require('node:fs');const path=require('node:path');const http=require('node:http');const assert=require('node:assert/strict');
const {chromium}=require(process.env.PLAYWRIGHT_MODULE || '../chart-recovery/node_modules/playwright-core');
const root=path.resolve(__dirname,'../..');
const css=fs.readdirSync(path.join(root,'apps/client/dist/assets')).find(x=>x.endsWith('.css'));
const server=http.createServer((req,res)=>{
  if(req.url==='/fixture.js'){res.setHeader('Content-Type','application/javascript');res.end(fs.readFileSync(path.join(__dirname,'dist/browser-fixture.js')));}
  else if(req.url==='/style.css'){res.setHeader('Content-Type','text/css');res.end(fs.readFileSync(path.join(root,'apps/client/dist/assets',css)));}
  else {res.setHeader('Content-Type','text/html');res.end('<div id="root"></div><link rel="stylesheet" href="/style.css"><script type="module" src="/fixture.js"></script>');}
});
function event(overrides={}) {const now=Date.now();return {type:'experimental_strategy_update',contractVersion:'iac-extreme-minute-v1',eventId:'run:TEST:1',lifecycleId:'run:TEST:0:0',supersedes:null,
  symbol:'TEST',strategy:'iac',kind:'candidate',marketDay:'2026-08-26',eventTimeMs:now-1000,receivedAtMs:now,decisionAtMs:now,
  anchorAvailableAtMs:now,anchorEventTimeMs:now-1000,anchorPrice:30,observedPrice:30,reason:'research_thresholds_met',
  evidence:{baselineBars:20,baselineRange:1,baselineVolume:100,compressionRatio:0.5,expansionRatio:2,participationRatio:3,moveFraction:0.03,pathEfficiency:null,floatEvidence:'unavailable'},...overrides};}
(async()=>{
  await new Promise(r=>server.listen(0,'127.0.0.1',r));const url=`http://127.0.0.1:${server.address().port}`;
  const browser=await chromium.launch({channel:'chromium-headless-shell',headless:true});const errors=[];
  try {for(const width of [1400,375]) {
    const page=await browser.newPage({viewport:{width,height:900}});page.on('pageerror',e=>errors.push(String(e)));
    await page.route('**/*',route=>route.request().url().startsWith(url)?route.continue():route.abort());
    await page.goto(url);await page.waitForFunction(()=>window.sent?.length===2);
    assert.equal(await page.evaluate(()=>window.sent[0].experimentalOnly),undefined);
    assert.equal(await page.evaluate(()=>window.sent[1].experimentalOnly),true);
    const push=v=>page.evaluate(value=>window.push(value),v);
    await push({type:'welcome',protocolVersion:1,serverTime:new Date().toISOString()});
    await push({type:'experimental_strategy_status',iacEnabled:false,extremeMomentumEnabled:false,healthy:true,sourceAtMs:null,laggedEvents:0,evictions:0,processingWarnings:0,activeSymbols:0,reason:'disabled',coverage:'observed_subscribed_symbols'});
    await page.waitForFunction(()=>!document.querySelector('.experimental-strategies'));
    await push({type:'experimental_strategy_status',iacEnabled:true,extremeMomentumEnabled:true,healthy:true,sourceAtMs:Date.now(),laggedEvents:0,evictions:0,processingWarnings:0,activeSymbols:1,reason:'observing',coverage:'observed_subscribed_symbols'});
    await page.getByText('No candidates in the observed feed yet.',{exact:false}).first().waitFor();
    const candidate=event();await push(candidate);await page.getByRole('button',{name:'TEST',exact:true}).waitFor();
    await page.getByRole('button',{name:'TEST',exact:true}).focus();await page.keyboard.press('Enter');
    await page.getByText('Selected TEST',{exact:true}).waitFor();
    await push({...candidate,eventId:'run:TEST:2',supersedes:candidate.eventId,kind:'held',decisionAtMs:candidate.decisionAtMs+1000});
    await page.getByText('Observed hold · $30.00',{exact:true}).waitFor();await push(candidate);
    assert.equal(await page.getByRole('button',{name:'TEST',exact:true}).count(),1);
    assert.equal(await page.locator('#legacy').textContent(),'Legacy events 0');
    await page.evaluate(()=>window.disconnectResearch());
    await page.evaluate(()=>window.pushLegacy({type:'ignition_event',symbol:'OLD',timestamp:new Date().toISOString(),price:5,kind:'follow_through_confirmed'}));
    await page.getByText('Legacy events 1',{exact:true}).waitFor();
    await push(event({symbol:'EXT',strategy:'extreme_momentum',eventId:'run:EXT:1',lifecycleId:'run:EXT:1:0',evidence:{...candidate.evidence,compressionRatio:null,pathEfficiency:1}}));
    await page.getByRole('button',{name:'EXT',exact:true}).waitFor();
    await push({type:'experimental_strategy_gap'});await page.getByText('feed incomplete or stale',{exact:false}).waitFor();
    await page.getByText('Why this candidate',{exact:true}).first().click();
    await page.getByText('These observations do not predict a profitable trade.',{exact:false}).first().waitFor();
    assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1));
    const output=process.env.STRATEGY_QA_OUTPUT;if(output){fs.mkdirSync(output,{recursive:true});await page.screenshot({path:path.join(output,`strategies-${width}.png`),fullPage:true});}
    console.log(`PASS real hook/panel: disabled, independent strategies >$25, keyboard selection, supersession/dedup, ordinary feed survives research disconnect, explicit gap, ${width}px layout`);
    await page.close();
  }assert.deepEqual(errors,[]);
  }finally{await browser.close();await new Promise(r=>server.close(r));}
})().catch(e=>{console.error(e);process.exitCode=1;server.close();});
