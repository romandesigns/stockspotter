const {chromium}=require(process.env.PLAYWRIGHT_MODULE);
const fs=require('node:fs');
(async()=>{
  const browser=await chromium.launch({executablePath:process.env.CHROME_PATH,headless:true});
  const results={};
  for(const variant of ['before','after']){
    const context=await browser.newContext({viewport:{width:720,height:800}});
    await context.route('**/*',r=>new URL(r.request().url()).hostname==='127.0.0.1'?r.continue():r.abort());
    await context.routeWebSocket('**/*',r=>new URL(r.url()).hostname==='127.0.0.1'?r.connectToServer():r.close());
    const page=await context.newPage();const cdp=await context.newCDPSession(page);
    await cdp.send('Emulation.setCPUThrottlingRate',{rate:4});
    await page.goto(`http://127.0.0.1:19871/?variant=${variant}&symbol=ENGINE`);
    await page.waitForFunction(()=>window.audit?.api);
    results[variant]=await page.evaluate(async()=>{
      const api=window.audit.api; const raf=()=>new Promise(r=>requestAnimationFrame(r));
      const base=1789718400;
      const bars=Array.from({length:500},(_,i)=>({time:base+i*60,open:100,high:101,low:99,close:100+Math.sin(i)*.1,volume:100}));
      const times=[]; const bytes=[];
      for(let i=0;i<220;i++){
        const next=bars.map((b,j)=>j===499?{...b,close:100+i/10000,volume:100+i}:b);
        const start=performance.now();api.setBars(next);const end=performance.now();
        if(i>=20){times.push(end-start);bytes.push(JSON.stringify(next).length);}
        await raf();
      }
      api.chart.timeScale().setVisibleLogicalRange({from:100,to:200});await raf();
      const beforeRange=api.chart.timeScale().getVisibleLogicalRange();
      api.setBars(bars.map((b,i)=>i===499?{...b,close:100.1}:b));await raf();
      const afterRange=api.chart.timeScale().getVisibleLogicalRange();
      times.sort((a,b)=>a-b);
      return {n:times.length,p50:times[99],p95:times[189],p99:times[197],max:times.at(-1),fullArrayBytes:bytes[0],beforeRange,afterRange};
    });
    await context.close();
  }
  fs.writeFileSync('tools/chart-audit/results/engine.json',JSON.stringify(results,null,2));
  console.log(JSON.stringify(results));await browser.close();
})().catch(e=>{console.error(e);process.exit(1);});
