const fs=require('node:fs'),path=require('node:path'),http=require('node:http'),assert=require('node:assert/strict');
const {chromium}=require(process.env.PLAYWRIGHT_MODULE || '../chart-recovery/node_modules/playwright-core');
const root=path.resolve(__dirname,'../..');
const css=fs.readdirSync(path.join(root,'apps/client/dist/assets')).find(x=>x.endsWith('.css'));
const server=http.createServer((req,res)=>{
  if(req.url==='/fixture.js'){res.setHeader('Content-Type','application/javascript');res.end(fs.readFileSync(path.join(__dirname,'dist/browser-fixture.js')));}
  else if(req.url==='/style.css'){res.setHeader('Content-Type','text/css');res.end(fs.readFileSync(path.join(root,'apps/client/dist/assets',css)));}
  else {res.setHeader('Content-Type','text/html');res.end('<div id="root"></div><link rel="stylesheet" href="/style.css"><script type="module" src="/fixture.js"></script>');}
});
(async()=>{
 await new Promise(r=>server.listen(0,'127.0.0.1',r)); const url=`http://127.0.0.1:${server.address().port}`;
 const browser=await chromium.launch({channel:'chromium-headless-shell',headless:true}); const errors=[];
 try {
  const page=await browser.newPage(); page.on('pageerror',e=>errors.push(String(e)));
  await page.route('**/*',route=>route.request().url().startsWith(url)?route.continue():route.abort());
  const researchHellos=()=>page.evaluate(()=>window.sent.filter(m=>m.type==='hello'&&m.experimentalOnly===true).length);
  await page.goto(url); await page.waitForFunction(()=>window.sent?.length===2);
  await page.getByRole('button',{name:'Research panels',exact:true}).click();
  const toggle=page.getByRole('switch',{name:'IAC and Extreme Momentum'});
  await toggle.click(); await page.waitForFunction(()=>!document.querySelector('.experimental-strategies'));
  await page.reload(); await page.waitForFunction(()=>window.sent?.length===1);
  assert.equal(await researchHellos(),0,'persisted off must open no research socket');
  await page.getByRole('button',{name:'Research panels',exact:true}).click();
  await toggle.click(); await page.waitForFunction(()=>window.sent?.length===2);
  await page.clock.install();
  await page.evaluate(()=>window.push({type:'hello_rejected',reason:'Experimental strategies disabled'}));
  await page.getByText('Research feed: Experimental strategies disabled.',{exact:false}).first().waitFor();
  await page.clock.runFor(120000);
  assert.equal(await researchHellos(),1,'disabled refusal must not retry over two minutes');
  await page.evaluate(()=>window.setResearchEnabled(false));
  await page.waitForFunction(()=>!document.querySelector('.experimental-strategies'));
  await page.evaluate(()=>window.setResearchEnabled(true)); await page.clock.runFor(1);
  await page.waitForFunction(()=>window.sent.filter(m=>m.type==='hello'&&m.experimentalOnly===true).length===2);
  await page.evaluate(()=>window.push({type:'hello_rejected',reason:'Experimental connection capacity reached'}));
  await page.clock.runFor(await page.evaluate(()=>window.researchReconnectBaseMs * 1.2 + 1));
  assert.equal(await researchHellos(),3,'capacity refusal must retry after bounded backoff');
  await page.evaluate(()=>window.setResearchEnabled(false)); await page.clock.runFor(120000);
  assert.equal(await researchHellos(),3,'switching off must cancel pending research reconnects');
  await page.reload(); await page.clock.runFor(1); await page.waitForFunction(()=>window.sent?.length===1);
  await page.evaluate(()=>window.setResearchEnabled(true)); await page.clock.runFor(1); await page.waitForFunction(()=>window.sent?.length===2);
  await page.goto(url+'?build-default=off'); await page.clock.runFor(1); await page.waitForFunction(()=>window.sent?.length===2);
  assert.equal(await researchHellos(),1,'saved on must override a disabled build default');
  await page.evaluate(()=>window.push({type:'hello_rejected',reason:'Unauthorized'}));
  await page.getByText('Research connection refused',{exact:true}).first().waitFor();
  assert.equal(await page.getByText('Waiting for the research connection.',{exact:true}).count(),0);
  assert.deepEqual(errors,[]);
  console.log('PASS real hook/control: saved off opens no research socket, manual retry, permanent refusal quiet, capacity retry, disable cleanup');
 }finally{await browser.close();await new Promise(r=>server.close(r));}
})().catch(e=>{console.error(e);process.exitCode=1;server.close();});
