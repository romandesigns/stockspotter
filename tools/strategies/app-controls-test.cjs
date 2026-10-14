const fs=require('node:fs'),path=require('node:path'),http=require('node:http'),assert=require('node:assert/strict');
const {chromium}=require(process.env.PLAYWRIGHT_MODULE || '../chart-recovery/node_modules/playwright-core');
const root=path.resolve(__dirname,'../../apps/client/dist');const errors=[],hellos=[];
const server=http.createServer((req,res)=>{
 const url=new URL(req.url,'http://127.0.0.1');
 if(url.pathname==='/health'){res.setHeader('Content-Type','application/json');res.end('{}');return;}
 if(url.pathname.startsWith('/movers/')){res.setHeader('Content-Type','application/json');res.end(JSON.stringify({gainers:[],mostActive:[]}));return;}
 if(url.pathname.startsWith('/markets/')||url.pathname.startsWith('/bars/')||url.pathname.startsWith('/auto-trader/')){res.setHeader('Content-Type','application/json');res.end('[]');return;}
 const filename=path.resolve(root,'.'+(url.pathname==='/'?'/index.html':url.pathname));
 if(!filename.startsWith(root+path.sep)||!fs.existsSync(filename)||!fs.statSync(filename).isFile()){res.writeHead(404);res.end();return;}
 res.setHeader('Content-Type',filename.endsWith('.js')?'application/javascript':filename.endsWith('.css')?'text/css':'text/html');res.end(fs.readFileSync(filename));
});
(async()=>{
 await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin=`http://127.0.0.1:${server.address().port}`;
 const browser=await chromium.launch({channel:'chromium-headless-shell',headless:true});
 try{
  for(const width of [1920,1280,375]) {
   const page=await browser.newPage({viewport:{width,height:1000}});page.on('pageerror',e=>errors.push(String(e)));
   await page.route('**/*',route=>{
    const requested=new URL(route.request().url());
    if(requested.origin===origin)return route.continue();
    if(requested.hostname==='stockspotter.wavystyle.io' && requested.pathname.startsWith('/api/')){
     const body=requested.pathname.includes('/movers/')?{gainers:[],mostActive:[]}:requested.pathname.endsWith('/health')?{}:requested.pathname.includes('/auto-trader/')?{executionMode:'journal',openPositions:[],recentEntries:[],trades:0,wins:0,losses:0,cumulativePnlUsd:0}:[];
     return route.fulfill({status:200,headers:{'Content-Type':'application/json','Access-Control-Allow-Origin':'*','Access-Control-Allow-Headers':'Authorization,Content-Type','Access-Control-Allow-Methods':'GET,OPTIONS'},body:JSON.stringify(body)});
    }
    return route.abort();
   });
   await page.routeWebSocket('**/*',socket=>{
    socket.onMessage(raw=>{
     const message=JSON.parse(String(raw));
     if(message.type==='hello'){
      hellos.push({experimentalOnly:message.experimentalOnly===true});
      socket.send(JSON.stringify({type:'welcome',protocolVersion:1}));
      if(message.experimentalOnly)socket.send(JSON.stringify({type:'experimental_strategy_status',iacEnabled:true,extremeMomentumEnabled:true,healthy:true,sourceAtMs:Date.now(),laggedEvents:0,evictions:0,processingWarnings:0,activeSymbols:0,reason:'observing',coverage:'observed_subscribed_symbols'}));
     }else if(message.type==='ping')socket.send(JSON.stringify({type:'pong',at:message.at}));
    });
   });
   await page.goto(origin);await page.locator('#access-key').fill('fixture-key-with-no-production-access');await page.getByRole('button',{name:'Continue',exact:true}).click();
   const research=page.getByRole('button',{name:'Research panels',exact:true});
   await research.waitFor();assert.equal(await research.count(),1,'one control instance across layout sizes');
   const box=await research.boundingBox();assert.ok(box&&box.width>0&&box.height>0,'research control must be visible');
   const replay=page.getByRole('button',{name:'Backtest Replay',exact:true});
   assert.equal(await replay.isVisible(),width>1400,'legacy compact fallback stays intact');
   await research.click();const toggle=page.getByRole('switch',{name:'IAC and Extreme Momentum'});await toggle.waitFor();
   if(!await toggle.isChecked())await toggle.click();
   await page.locator('.experimental-strategies').waitFor();
   await toggle.click();await page.waitForFunction(()=>!document.querySelector('.experimental-strategies'));
   await page.keyboard.press('Escape');await page.reload();await research.waitFor();
   assert.equal(await page.locator('.experimental-strategies').count(),0,'saved off survives reload');
   await page.setViewportSize({width:width===1920?1280:1920,height:1000});
   await research.waitFor();assert.equal(await research.count(),1,'resizing must not duplicate controls');
   assert.equal(await research.getAttribute('data-state'),'closed');
   await page.close();console.log(`PASS full built app: reachable research control, legacy fallback, saved off, single control across resize at ${width}px`);
  }
  assert.deepEqual(errors,[]);
 }finally{await browser.close();await new Promise(r=>server.close(r));}
})().catch(error=>{console.error(error);process.exitCode=1;server.close();});
