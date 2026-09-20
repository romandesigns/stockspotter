import {mkdir} from "node:fs/promises";
const root=process.cwd();
await mkdir("tools/chart-audit/results",{recursive:true});
const build=await Bun.build({entrypoints:["tools/chart-audit/browser.tsx"],outdir:"tools/chart-audit/.generated",target:"browser",minify:true,
  define:{"process.env.NODE_ENV":'"production"',"import.meta.env":'({DEV:true,VITE_WS_URL:"ws://127.0.0.1:"+(new URLSearchParams(location.search).get("variant")==="before"?19872:19873),VITE_HTTP_URL:"http://127.0.0.1:19871"})'},
  plugins:[{name:"audit-client-resolution",setup(build){
    build.onResolve({filter:/^(react|react-dom|lightweight-charts|@stockspotter\/shared-types)(\/.*)?$/},args=>({path:Bun.resolveSync(args.path,root+"/apps/client")}));
    // The production config reads import.meta through a TS cast; use a fixture-only
    // module replacement rather than rely on a bundler's env rewriting semantics.
    build.onLoad({filter:/[\\/]lib[\\/]config\.ts$/},()=>({loader:"ts",contents:'export function resolveWsUrl(){return "ws://127.0.0.1:"+(new URLSearchParams(location.search).get("variant")==="before"?19872:19873)} export function resolveHttpUrl(){return "http://127.0.0.1:19871"}'}));
  }}]});
if(!build.success)throw new Error(build.logs.join("\n"));
let upstream:any=null; let subscribed=false; let source:any[]=[];
const server=Bun.serve({hostname:"127.0.0.1",port:19871,
  async fetch(req,server){
    const url=new URL(req.url);
    if(url.pathname==="/source"){if(server.upgrade(req))return;return new Response("upgrade required",{status:400});}
    if(url.pathname==="/browser.js")return new Response(Bun.file("tools/chart-audit/.generated/browser.js"),{headers:{"content-type":"text/javascript"}});
    if(url.pathname==="/clock")return Response.json({now:performance.timeOrigin+performance.now()});
    if(url.pathname==="/ready")return Response.json({subscribed});
    if(url.pathname==="/receipts")return Response.json(source);
    if(url.pathname==="/catalysts/today")return Response.json([]);
    if(url.pathname==="/start"){
      const symbol=url.searchParams.get("symbol")!;
      const count=Number(url.searchParams.get("count")||600);
      const delay=Number(url.searchParams.get("delay")||10);
      const run=async()=>{
        for(let i=1;i<=count;i++){
          const sourceAt=performance.timeOrigin+performance.now();
          const trade={T:"t",S:symbol,p:100+i/10000,s:1,t:new Date(Date.parse("2026-09-18T08:00:00Z")+i).toISOString(),c:["@"]};
          source.push({symbol,seq:i,source:sourceAt});upstream.send(JSON.stringify([trade]));
          await Bun.sleep(delay);
        }
      };void run();return Response.json({started:true});
    }
    return new Response('<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1"><style>body{margin:0;background:#111827;color:white;font-family:Arial}#root{width:100%}</style></head><body><div id="root"></div><script type="module" src="/browser.js"></script></body></html>',{headers:{"content-type":"text/html"}});
  },
  websocket:{open(ws){upstream=ws;ws.send('[{"T":"success","msg":"connected"}]');},message(ws,message){
    const msg=JSON.parse(String(message));
    if(msg.action==="auth")ws.send('[{"T":"success","msg":"authenticated"}]');
    if(msg.action==="subscribe"){subscribed=true;ws.send(JSON.stringify([{T:"subscription",bars:["AUDIT"],trades:["AUDIT"]}]));}
  }},
});
console.log(`offline fixture source/UI: ${server.url}`);
