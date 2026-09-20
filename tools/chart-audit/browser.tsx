import React, {useEffect, useMemo, useRef} from "react";
import {createRoot} from "react-dom/client";
import {setAccessKey} from "../../packages/shared-types/src/access";
import {useRealtimeFeed} from "../../apps/client/src/lib/useRealtimeFeed";
import {toChartBars, mergeBars} from "../../apps/client/src/lib/derive";
import {mountSuperChart as candidate} from "../../apps/client/src/lib/superChartEngine";
import {mountSuperChart as baseline} from "./baseline-engine";

const params = new URLSearchParams(location.search);
const variant = params.get("variant")!;
const symbol = params.get("symbol")!;
const interval = Number(params.get("interval") || 60);
const base = Date.parse("2026-09-18T08:00:00Z") / 1000;
const history = Array.from({length:500},(_,i)=>({time:base-(500-i)*interval,open:100,high:100.5,low:99.5,close:100+Math.sin(i)*.1,volume:100}));
const epoch = () => performance.timeOrigin + performance.now();
const samples: any[] = [];
const longTasks: any[] = [];
new PerformanceObserver(list=>longTasks.push(...list.getEntries().map(e=>({start:performance.timeOrigin+e.startTime,duration:e.duration})))).observe({type:"longtask",buffered:true});
let lastCanvasDraw = 0;
// Observe actual 2D draw commands, not merely React commit or setData submission.
for (const method of ["fillRect","stroke","fill","drawImage","fillText"] as const) {
  const original = CanvasRenderingContext2D.prototype[method];
  (CanvasRenderingContext2D.prototype as any)[method] = function(...args:any[]) {
    lastCanvasDraw = epoch(); return (original as any).apply(this,args);
  };
}
const audit = (window as any).audit = {samples,longTasks,ready:false,api:null,received:[],epoch};
const NativeWebSocket = window.WebSocket;
// Timestamp dispatch before the real hook's listener; do not alter its messages.
window.WebSocket = class extends NativeWebSocket {
  constructor(url:string|URL, protocols?:string|string[]) {
    super(url,protocols);
    this.addEventListener("message",event=>{
      const msg=JSON.parse(event.data);
      if(msg.type==="welcome") audit.ready=true;
      if(msg.type==="bar_update"&&msg.symbol===symbol&&msg.intervalSecs===interval)
        audit.received.push({seq:Math.round((msg.close-100)*10000),at:epoch()});
    });
  }
};
setAccessKey("chart-audit-loopback-fixture-only");
function App() {
  const feed=useRealtimeFeed(); const el=useRef<HTMLDivElement>(null); const api=useRef<any>(null);
  const raw=(interval===30?feed.subMinuteBarsBySymbol:feed.barsBySymbol).get(symbol);
  const bars=useMemo(()=>mergeBars(history,toChartBars(raw??[])),[raw]);
  useEffect(()=>{
    api.current=(variant==="before"?baseline:candidate)(el.current!,"scanner",{bars:history,height:500});
    audit.api=api.current;
    return ()=>{api.current.destroy();api.current.chart.remove();};
  },[]);
  useEffect(()=>{
    if(!api.current)return;
    const start=epoch(); api.current.setBars(bars); const submit=epoch();
    if(!raw?.length)return;
    const seq=Math.round((bars.at(-1)!.close-100)*10000);
    // Chart's RAF was scheduled by setBars first. A following RAF verifies drawing occurred;
    // timestamp is command completion, NOT display scan-out or physical presentation.
    requestAnimationFrame(()=>{
      samples.push({seq,start,submit,draw:lastCanvasDraw,check:epoch(),volume:bars.at(-1)!.volume});
    });
  },[bars]);
  return <div ref={el} style={{width:"100%",height:500,position:"relative"}}/>;
}
createRoot(document.getElementById("root")!).render(<App/>);

// Real engine data parity against full replacement, including an old-bar correction,
// cap eviction, timeframe reset and empty reset; run separately from latency trials.
audit.checkParity=()=>{
  const host=document.createElement("div");host.style.cssText="width:900px;height:500px";document.body.append(host);
  const reference=baseline(host,"scanner",{bars:history,height:500});
  const cases=[history,history.map((b,i)=>i===499?{...b,close:102,high:103,volume:125}:b)];
  cases.push([...cases[1],{...history[499],time:base,close:101,volume:5}]);
  cases.push(cases[2].map((b,i)=>i===250?{...b,close:99,low:98}:b));
  cases.push(cases[3].slice(1));cases.push(history.filter((_,i)=>i%5===0));cases.push([]);
  const failures=[];
  for(let i=0;i<cases.length;i++) {
    reference.setBars(cases[i]);audit.api.setBars(cases[i]);
    for(const [key,series] of Object.entries(reference.series)) {
      if(JSON.stringify((series as any).data())!==JSON.stringify(audit.api.series[key].data()))failures.push({case:i,series:key});
    }
  }
  reference.destroy();reference.chart.remove();host.remove();return {cases:cases.length,series:12,failures};
};
