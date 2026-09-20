const { chromium } = require(process.env.PLAYWRIGHT_MODULE);
const fs = require('node:fs');
const base = 'http://127.0.0.1:19871';
const dist = values => {
  const x = values.filter(Number.isFinite).sort((a,b)=>a-b);
  const p=q=>x.length?+x[Math.min(x.length-1,Math.ceil(q*x.length)-1)].toFixed(3):null;
  return {n:x.length,p50:p(.5),p90:p(.9),p95:p(.95),p99:p(.99),max:p(1),over100:x.filter(v=>v>100).length,over500:x.filter(v=>v>500).length};
};
(async()=>{
  const browser = await chromium.launch({executablePath:process.env.CHROME_PATH,headless:true,args:['--disable-background-timer-throttling','--disable-renderer-backgrounding','--disable-backgrounding-occluded-windows']});
  const results={date:new Date().toISOString(),browser:browser.version(),profiles:[],errors:[]};
  const profiles=[
    {name:'sparse-premarket',width:1440,height:900,cpu:1,count:2,delay:10,interval:30},
    {name:'desktop-100hz',width:1440,height:900,cpu:1,count:600,delay:10,interval:60},
    {name:'phone-4x-100hz-stalls',width:390,height:844,cpu:4,count:600,delay:10,interval:30,stalls:true},
    {name:'foldable-4x-250hz-resize',width:720,height:800,cpu:4,count:1200,delay:4,interval:60,fold:true},
  ];
  for(let pi=0;pi<profiles.length;pi++){
    const profile=profiles[pi], symbol=`AUDIT${pi}`;
    const pages=[];
    for(const variant of ['before','after']){
      const context=await browser.newContext({viewport:{width:profile.width,height:profile.height},deviceScaleFactor:profile.cpu>1?2:1});
      // Explicitly forbid any accidentally introduced non-loopback request.
      await context.route('**/*',route=>new URL(route.request().url()).hostname==='127.0.0.1'?route.continue():route.abort());
      await context.routeWebSocket('**/*',route=>new URL(route.url()).hostname==='127.0.0.1'?route.connectToServer():route.close());
      const page=await context.newPage();
      page.on('pageerror',e=>results.errors.push({profile:profile.name,variant,error:String(e)}));
      const cdp=await context.newCDPSession(page);
      await cdp.send('Emulation.setCPUThrottlingRate',{rate:profile.cpu});
      await page.goto(`${base}/?variant=${variant}&symbol=${symbol}&interval=${profile.interval}`);
      await page.waitForFunction(()=>window.audit?.ready && window.audit.api);
      pages.push({page,context,variant});
    }
    if(profile.stalls)for(const {page} of pages)await page.evaluate(()=>{
      for(const [after,duration] of [[2000,750],[4000,150]])setTimeout(()=>{const end=performance.now()+duration;while(performance.now()<end){}},after);
    });
    await fetch(`${base}/start?symbol=${symbol}&count=${profile.count}&delay=${profile.delay}`);
    if(profile.fold){
      for(let i=0;i<6;i++){
        await new Promise(r=>setTimeout(r,400));
        for(const {page} of pages)await page.setViewportSize(i%2?{width:720,height:800}:{width:360,height:780});
      }
    }
    // Wait for actual source completion, then a 1200ms observation window for tails.
    let source;
    for(let i=0;i<400;i++){
      source=(await (await fetch(`${base}/receipts`)).json()).filter(x=>x.symbol===symbol);
      if(source.length===profile.count)break;
      await new Promise(r=>setTimeout(r,50));
    }
    await new Promise(r=>setTimeout(r,1200));
    const ingress=(await (await fetch('http://127.0.0.1:19874/ingress')).json()).filter(x=>x.symbol===symbol);
    const out={...profile,symbol,sourceCount:source.length,ingressCount:ingress.length,variants:{},source,ingress};
    for(const {page,context,variant} of pages){
      const raw=await page.evaluate(()=>({samples:window.audit.samples,received:window.audit.received,longTasks:window.audit.longTasks}));
      const drawn=raw.samples.filter(x=>x.draw>=x.submit && x.seq>=1);
      const latencies=[],inside=[],upstream=[];let missing=0;
      for(const input of source){
        const received=ingress.find(x=>x.seq===input.seq);
        const draw=drawn.find(x=>x.seq>=input.seq);
        if(received)upstream.push(received.ingress-input.source);
        if(draw){latencies.push(draw.draw-input.source);if(received)inside.push(draw.draw-received.ingress);}else missing++;
      }
      const renderDispatch=raw.received.map(r=>{const draw=drawn.find(d=>d.seq>=r.seq);return draw?draw.draw-r.at:NaN;});
      out.variants[variant]={sourceToCanvas:dist(latencies),ingressToCanvas:dist(inside),sourceToIngress:dist(upstream),dispatchToCanvas:dist(renderDispatch),setBars:dist(raw.samples.map(s=>s.submit-s.start)),missingAtEnd:missing,drawnFrames:drawn.length,receivedFrames:raw.received.length,raw};
      await page.screenshot({path:`tools/chart-audit/results/${profile.name}-${variant}.png`});
      if(variant==='after'){
        out.parity=await page.evaluate(()=>window.audit.checkParity());
        if(out.parity.failures.length)throw new Error(JSON.stringify(out.parity));
      }
      await context.close();
    }
    results.profiles.push(out);
    fs.writeFileSync('tools/chart-audit/results/latency.json',JSON.stringify(results,null,2));
    console.log(JSON.stringify({profile:profile.name,before:out.variants.before.sourceToCanvas,after:out.variants.after.sourceToCanvas,missing:[out.variants.before.missingAtEnd,out.variants.after.missingAtEnd],parity:out.parity}));
  }
  await browser.close();
  if(results.errors.length)throw new Error(JSON.stringify(results.errors));
})().catch(e=>{console.error(e);process.exit(1);});
