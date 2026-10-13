// Loads the fixture bundle twice -- once with the EXPO_PUBLIC_ research
// flag on, once with it absent -- and runs each page's suite. Same runner
// shape as tools/native-history/run.cjs: PLAYWRIGHT_MODULE names the
// installed module, CHROME_PATH optionally overrides the browser, nothing
// is downloaded. Nonzero exit means a failed assertion or a page error.
const http=require('node:http'),fs=require('node:fs'),path=require('node:path');
const {chromium}=require(process.env.PLAYWRIGHT_MODULE);
const ROUTES={'/':'index.html','/fixture.js':'fixture.js'};
(async()=>{
 const server=http.createServer((request,response)=>{
  const url=new URL(request.url,'http://127.0.0.1');
  // The ordinary feed hook does a one-shot catalyst backfill on mount; an
  // empty array is the real "nothing promoted yet" response and keeps the
  // harness off the network.
  if(url.pathname==='/catalysts/today'){response.setHeader('Content-Type','application/json');response.end('[]');return}
  const file=ROUTES[url.pathname];
  if(!file){response.statusCode=404;response.end('not found');return}
  response.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':'text/html');
  response.end(fs.readFileSync(path.join(__dirname,'dist',file)));
 });
 await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
 const origin=`http://127.0.0.1:${server.address().port}`;
 let browser;
 try{
  browser=await chromium.launch(process.env.CHROME_PATH
   ? {executablePath:process.env.CHROME_PATH,headless:true}
   : {channel:'chromium-headless-shell',headless:true});
  const suites=[];
  for(const [suite,query] of [['flag-on','?flag=true'],['flag-absent','']]){
   const page=await browser.newPage();
   const pageErrors=[];
   page.on('pageerror',error=>pageErrors.push(String(error)));
   await page.goto(`${origin}/${query}`);
   await page.waitForFunction(()=>typeof window.run==='function');
   const tests=await page.evaluate(()=>window.run());
   await page.close();
   suites.push({suite,tests,pageErrors,passed:tests.filter(t=>t.pass).length,total:tests.length});
  }
  const result={
   suites,
   passed:suites.reduce((n,s)=>n+s.passed,0),
   total:suites.reduce((n,s)=>n+s.total,0),
   pageErrors:suites.flatMap(s=>s.pageErrors),
  };
  if(process.argv[2])fs.writeFileSync(process.argv[2],JSON.stringify(result,null,2));
  console.log(JSON.stringify(result,null,2));
  process.exitCode=result.pageErrors.length||result.passed!==result.total?1:0;
 }finally{
  if(browser)await browser.close();
  server.close();
 }
})().catch(error=>{console.error(error);process.exitCode=1});
