// Lifecycle regression for the real mobile experimental hook.
//
// What this exists to catch: React Native delivers WebSocket events over
// the bridge asynchronously, so a socket that was still CONNECTING when
// AppState went to "background" can hand JavaScript an open/message/close
// AFTER the effect cleanup closed it and AFTER the foreground re-run opened
// a replacement. Every assertion below drives that ordering explicitly --
// a late event is delivered to a socket the hook has already retired, and
// the current view is then checked for contamination.
//
// The fake socket never "opens by itself": nothing happens until a test
// calls deliverOpen/deliverMessage/deliverClose, and those deliver
// regardless of readyState precisely because the bridge does too.
//
// Every state transition is driven through React's own `act`, not through
// a counted number of macrotask ticks: a socket event dispatched from
// outside React schedules its re-render and its passive effect (the
// cleanup that closes the socket) on React's scheduler, and "await two
// setTimeout(0)s" is a race that passes or fails depending on how many
// renders already happened in that scenario -- observed failing exactly
// that way before this was switched to `act`.
import React, {act, useLayoutEffect} from 'react';
import {createRoot} from 'react-dom/client';
import {setAccessKey} from '@stockspotter/shared-types';
import {EXPERIMENTAL_STRATEGIES_ENABLED, useExperimentalStrategies} from 'hook-under-test';
import {useRealtimeFeed} from 'normal-feed-under-test';

(globalThis as any).IS_REACT_ACT_ENVIRONMENT=true;
const appState=(globalThis as any).__appState as {listeners:Set<unknown>;reset():void;emit(next:string):void};

type Listener=(event:any)=>void;
let sockets:FakeSocket[]=[];
class FakeSocket {
 static readonly CONNECTING=0; static readonly OPEN=1; static readonly CLOSING=2; static readonly CLOSED=3;
 url:string; readyState=0; sent:string[]=[]; closeCalls=0;
 private handlers=new Map<string,Listener[]>();
 constructor(url:string){this.url=url;sockets.push(this)}
 addEventListener(type:string,fn:Listener){this.handlers.set(type,[...(this.handlers.get(type)??[]),fn])}
 removeEventListener(type:string,fn:Listener){this.handlers.set(type,(this.handlers.get(type)??[]).filter(x=>x!==fn))}
 /** Recorded unconditionally, including on a closed socket: "did a retired
  * socket write to the wire" is the assertion, not whether it would throw. */
 send(data:string){this.sent.push(data)}
 close(){this.closeCalls++;if(this.readyState===FakeSocket.CLOSED)return;this.readyState=FakeSocket.CLOSED;this.dispatch('close',{})}
 dispatch(type:string,event:any){for(const fn of [...(this.handlers.get(type)??[])])fn(event)}
 /** A bridge-queued open. A socket closed while CONNECTING stays CLOSED --
  * the event arrives late, it does not resurrect the connection. */
 deliverOpen(){if(this.readyState===FakeSocket.CONNECTING)this.readyState=FakeSocket.OPEN;this.dispatch('open',{})}
 deliverMessage(payload:unknown){this.dispatch('message',{data:typeof payload==='string'?payload:JSON.stringify(payload)})}
 deliverClose(){this.dispatch('close',{})}
 deliverError(){this.dispatch('error',{})}
}
(globalThis as any).WebSocket=FakeSocket;

// Timer/interval recorders. Only >=1s timeouts are recorded, which is
// exactly the hook's reconnect backoff band (2.4s..72s). Real ids are
// returned so the hook's own clearTimeout still works.
let scheduled:{ms:number;id:any;fire():void}[]=[];
let intervals:{ms:number}[]=[];
const realSetTimeout=globalThis.setTimeout.bind(globalThis);
const realSetInterval=globalThis.setInterval.bind(globalThis);
(globalThis as any).setTimeout=(fn:any,ms?:number,...rest:any[])=>{
 const id=realSetTimeout(fn,ms as any,...rest);
 if(typeof fn==='function'&&typeof ms==='number'&&ms>=1000)scheduled.push({ms,id,fire(){clearTimeout(id);fn()}});
 return id;
};
(globalThis as any).setInterval=(fn:any,ms?:number,...rest:any[])=>{
 if(typeof ms==='number')intervals.push({ms});
 return realSetInterval(fn,ms as any,...rest);
};
/** Drops any still-pending recorded timer, so one scenario's unfired
 * backoff can never run inside the next one. */
const clearScheduled=()=>{for(const timer of scheduled)clearTimeout(timer.id);scheduled=[]};

const statusFrame=(overrides:Record<string,unknown>={})=>({type:'experimental_strategy_status',iacEnabled:true,extremeMomentumEnabled:true,
 healthy:true,sourceAtMs:10_000,laggedEvents:0,evictions:0,processingWarnings:0,activeSymbols:3,reason:'observing',
 coverage:'observed_subscribed_symbols',...overrides});
const updateFrame=(overrides:Record<string,unknown>={})=>({type:'experimental_strategy_update',contractVersion:'iac-extreme-minute-v1',
 eventId:'run:TEST:1',lifecycleId:'run:TEST:0:0',supersedes:null,symbol:'TEST',strategy:'iac',kind:'candidate',marketDay:'2026-10-13',
 eventTimeMs:1000,receivedAtMs:1100,decisionAtMs:1200,anchorAvailableAtMs:1200,anchorEventTimeMs:1000,anchorPrice:30,observedPrice:30,
 reason:'research_thresholds_met',evidence:{baselineBars:20,baselineRange:1,baselineVolume:100,compressionRatio:0.5,expansionRatio:2,
 participationRatio:3,moveFraction:0.03,pathEfficiency:null,floatEvidence:'unavailable'},...overrides});
const barFrame=(secondsAgo:number,close:number)=>({type:'bar_update',symbol:'TEST',timestamp:new Date(Date.now()-secondsAgo*1000).toISOString(),
 open:close,high:close,low:close,close,volume:1,intervalSecs:60});

interface Commit {enabled:boolean;connection:string;gap:boolean;rows:number;status:string|null;normalStatus?:string;normalBars?:number}
let commits:Commit[]=[];
function ResearchProbe(){
 const feed=useExperimentalStrategies();
 // useLayoutEffect, not useEffect: every committed render is observed
 // before passive effects can paper over a transient wrong value.
 useLayoutEffect(()=>{commits.push({enabled:feed.enabled,connection:feed.connection,gap:feed.gap,rows:feed.rows.length,
  status:feed.status?feed.status.reason:null})});
 return null;
}
function BothFeedsProbe(){
 // Ordinary feed FIRST, so sockets[0] is the ordinary one and sockets[1]
 // the research one -- the ordering the independence assertions rely on.
 const normal=useRealtimeFeed();
 const feed=useExperimentalStrategies();
 useLayoutEffect(()=>{commits.push({enabled:feed.enabled,connection:feed.connection,gap:feed.gap,rows:feed.rows.length,
  status:feed.status?feed.status.reason:null,normalStatus:normal.status,normalBars:normal.barsBySymbol.get('TEST')?.length??0})});
 return null;
}

const assert=(ok:boolean,message:string)=>{if(!ok)throw Error(message)};
const latest=()=>commits[commits.length-1];

let root:ReturnType<typeof createRoot>|null=null;
let host:HTMLElement|null=null;
/** Runs `body` (socket deliveries, AppState transitions, a retry firing)
 * and then lets React finish every render and effect it provoked. */
const flush=async(body?:()=>void)=>{await act(async()=>{if(body)body()})};
const mount=async(Component:()=>null)=>{
 host=document.createElement('div');document.body.append(host);
 root=createRoot(host);
 await flush(()=>root!.render(<Component/>));
};
const unmount=async()=>{
 const current=root;root=null;
 if(current)await flush(()=>current.unmount());
 host?.remove();host=null;
};
function reset(){
 clearScheduled();
 sockets=[];commits=[];intervals=[];
 appState.reset();
 setAccessKey('fixture-token');
}

type Scenario={name:string;run:()=>Promise<void>};

const ON_SCENARIOS:Scenario[]=[
 {name:'the inlined flag is what the hook gates on',run:async()=>{
  assert(EXPERIMENTAL_STRATEGIES_ENABLED===true,'page was loaded with the flag on but the module gate is false');
  reset();await mount(ResearchProbe);
  assert(latest().enabled===true,'hook reports disabled with the flag on');
  assert(sockets.length===1,`expected exactly one research socket, got ${sockets.length}`);
  assert(sockets[0].url.endsWith('/ws'),`unexpected socket url ${sockets[0].url}`);
  assert(appState.listeners.size===1,'hook did not subscribe to AppState');
  assert(intervals.some(i=>i.ms===15_000),'hook started no 15s heartbeat');
  await unmount();
 }},

 {name:'open delivered after background cleanup touches nothing',run:async()=>{
  reset();await mount(ResearchProbe);
  assert(latest().connection==='connecting','first render should be connecting');
  const stale=sockets[0];
  assert(stale.readyState===FakeSocket.CONNECTING,'socket should still be CONNECTING');
  await flush(()=>appState.emit('background'));
  assert(stale.closeCalls===1,`cleanup did not close the CONNECTING socket (closeCalls=${stale.closeCalls})`);
  assert(latest().connection==='closed','backgrounded hook did not report closed');
  const timers=scheduled.length;
  // The bridge hands over everything that was already in flight.
  await flush(()=>{stale.deliverOpen();stale.deliverMessage({type:'welcome'});stale.deliverMessage(statusFrame());
   stale.deliverMessage(updateFrame())});
  assert(stale.sent.length===0,`retired socket wrote ${stale.sent.length} frame(s) to the wire`);
  assert(sockets.length===1,'a retired socket opened a replacement connection');
  assert(latest().connection==='closed','a late open/welcome marked the backgrounded view open');
  assert(latest().status===null,'a late status frame reached the live view');
  assert(latest().rows===0,'a late update frame reached the live row list');
  assert(scheduled.length===timers,'a retired socket scheduled a timer');
  // Foregrounding opens a fresh socket. Nothing was ever received, so the
  // retired late open must not have made this look like a reconnect.
  await flush(()=>appState.emit('active'));
  assert(sockets.length===2,'foregrounding did not reconnect');
  const fresh=sockets[1];
  await flush(()=>fresh.deliverOpen());
  assert(fresh.sent.length===1,`fresh socket sent ${fresh.sent.length} hello frames`);
  const hello=JSON.parse(fresh.sent[0]);
  assert(hello.type==='hello'&&hello.client==='mobile','fresh socket sent something other than a mobile hello');
  assert(hello.experimentalStrategies===true&&hello.experimentalOnly===true,'research hello is not experimental-only');
  assert(hello.token==='fixture-token','hello carried no access key');
  assert(latest().gap===false,'a late open on a retired socket faked a gap on the next connection');
  await unmount();
 }},

 {name:'message delivered after background cleanup touches nothing',run:async()=>{
  reset();await mount(ResearchProbe);
  const stale=sockets[0];
  await flush(()=>{stale.deliverOpen();stale.deliverMessage({type:'welcome'});stale.deliverMessage(statusFrame());
   stale.deliverMessage(updateFrame())});
  assert(latest().connection==='open','welcome did not open the view');
  assert(latest().status==='observing','status frame did not reach the view');
  assert(latest().rows===1,`expected 1 row, got ${latest().rows}`);
  await flush(()=>appState.emit('background'));
  assert(stale.closeCalls===1,'cleanup did not close the open socket');
  const before=latest();
  // Four bridge-queued frames, each a different write path into state or
  // onto the wire: a new row, a status change, a decoder failure (gap),
  // and a ping the hook would otherwise answer.
  await flush(()=>{
   stale.deliverMessage(updateFrame({eventId:'run:TEST:9',lifecycleId:'run:TEST:9:0'}));
   stale.deliverMessage(statusFrame({healthy:false,reason:'upstream_feed_gap'}));
   stale.deliverMessage('not json at all');
   stale.deliverMessage({type:'ping',at:'2026-10-13T00:00:00.000Z'});
  });
  assert(latest().rows===before.rows,'a late update frame added a row after teardown');
  assert(latest().status===before.status,'a late status frame changed status after teardown');
  assert(latest().gap===before.gap,'a late undecodable frame flagged a gap after teardown');
  assert(stale.sent.length===1,`retired socket answered the wire (${stale.sent.length} frames sent)`);
  await unmount();
 }},

 {name:'retired socket cannot schedule a retry or close the live view',run:async()=>{
  reset();await mount(ResearchProbe);
  const stale=sockets[0];
  await flush(()=>{stale.deliverOpen();stale.deliverMessage({type:'welcome'})});
  await flush(()=>appState.emit('background'));
  await flush(()=>appState.emit('active'));
  assert(sockets.length===2,'foregrounding did not open a replacement socket');
  const live=sockets[1];
  await flush(()=>{live.deliverOpen();live.deliverMessage({type:'welcome'})});
  assert(latest().connection==='open','replacement socket did not open the view');
  const timers=scheduled.length;
  // Trailing error + close from the connection the hook already retired.
  await flush(()=>{stale.deliverError();stale.deliverClose()});
  assert(latest().connection==='open','a retired socket closed the current view');
  assert(scheduled.length===timers,'a retired socket scheduled a reconnect timer');
  assert(sockets.length===2,'a retired socket opened another connection');
  assert(stale.closeCalls===1,'a retired socket was closed again by its own error handler');
  await unmount();
 }},

 {name:'a live close still retries, backs off and reports the gap',run:async()=>{
  reset();await mount(ResearchProbe);
  const first=sockets[0];
  await flush(()=>{first.deliverOpen();first.deliverMessage({type:'welcome'})});
  assert(latest().gap===false,'a first connection reported a gap');
  const timers=scheduled.length;
  await flush(()=>first.close());
  assert(latest().connection==='closed','a live close did not close the view');
  assert(scheduled.length===timers+1,'a live close scheduled no reconnect');
  const retry=scheduled[scheduled.length-1];
  assert(retry.ms>=2400&&retry.ms<=3600,`first backoff ${retry.ms}ms outside the 3s +-20% band`);
  await flush(()=>retry.fire());
  assert(sockets.length===2,'the reconnect timer opened no socket');
  await flush(()=>sockets[1].deliverOpen());
  assert(sockets[1].sent.length===1,'the reconnected socket sent no hello');
  assert(latest().gap===true,'a real reconnect did not record a gap');
  await unmount();
 }},

 {name:'a background round trip records a gap, a never-opened one does not',run:async()=>{
  reset();await mount(ResearchProbe);
  await flush(()=>{sockets[0].deliverOpen();sockets[0].deliverMessage({type:'welcome'})});
  await flush(()=>appState.emit('background'));
  await flush(()=>appState.emit('active'));
  await flush(()=>sockets[1].deliverOpen());
  assert(latest().gap===true,'a background round trip after a real connection recorded no gap');
  await unmount();
  // Second mount: backgrounded before the socket ever opened.
  reset();await mount(ResearchProbe);
  await flush(()=>appState.emit('background'));
  await flush(()=>appState.emit('active'));
  await flush(()=>sockets[1].deliverOpen());
  assert(latest().gap===false,'a socket that never opened produced a phantom gap');
  await unmount();
 }},

 {name:'inactive is not background',run:async()=>{
  reset();await mount(ResearchProbe);
  const live=sockets[0];
  await flush(()=>{live.deliverOpen();live.deliverMessage({type:'welcome'})});
  await flush(()=>appState.emit('inactive'));
  assert(live.closeCalls===0,'an iOS "inactive" transition tore the socket down');
  assert(sockets.length===1,'an iOS "inactive" transition reconnected');
  assert(latest().connection==='open','an iOS "inactive" transition closed the view');
  await unmount();
 }},

 {name:'unmount releases the AppState subscription and the socket',run:async()=>{
  reset();await mount(ResearchProbe);
  const live=sockets[0];
  await flush(()=>{live.deliverOpen();live.deliverMessage({type:'welcome'})});
  assert(appState.listeners.size===1,'no AppState subscription to release');
  const timers=scheduled.length;
  await unmount();
  assert(appState.listeners.size===0,'unmount left an AppState subscription behind');
  assert(live.closeCalls===1,'unmount left the socket open');
  await flush(()=>{live.deliverClose();live.deliverOpen();live.deliverMessage(updateFrame())});
  assert(sockets.length===1,'an unmounted hook reconnected');
  assert(scheduled.length===timers,'an unmounted hook scheduled a retry');
  assert(live.sent.length===1,'an unmounted hook wrote to a dead socket');
 }},

 {name:'the research lane cannot disturb the ordinary feed',run:async()=>{
  reset();await mount(BothFeedsProbe);
  assert(sockets.length===2,`expected one ordinary and one research socket, got ${sockets.length}`);
  const [ordinary,research]=sockets;
  assert(ordinary!==research,'both hooks shared a socket');
  await flush(()=>{ordinary.deliverOpen();research.deliverOpen()});
  const ordinaryHello=JSON.parse(ordinary.sent[0]);
  const researchHello=JSON.parse(research.sent[0]);
  assert(ordinaryHello.experimentalStrategies===undefined&&ordinaryHello.experimentalOnly===undefined,
   'the ordinary mobile hello carries experimental opt-in fields');
  assert(researchHello.experimentalOnly===true,'the research hello is not experimental-only');
  await flush(()=>{
   ordinary.deliverMessage({type:'welcome',protocolVersion:1,serverTime:new Date().toISOString()});
   ordinary.deliverMessage(barFrame(120,10));
   research.deliverMessage({type:'welcome'});research.deliverMessage(statusFrame());research.deliverMessage(updateFrame());
  });
  assert(latest().normalBars===1,`ordinary feed holds ${latest().normalBars} bars, expected 1`);
  assert(latest().rows===1,'research lane holds no row');
  // Backgrounding is the research lane's own policy. The ordinary feed has
  // no AppState policy at all and must be completely unaffected by it.
  await flush(()=>appState.emit('background'));
  assert(research.closeCalls===1,'backgrounding did not tear down the research socket');
  assert(ordinary.closeCalls===0,'backgrounding closed the ordinary feed socket');
  assert(ordinary.readyState===FakeSocket.OPEN,'the ordinary feed socket is no longer open');
  await flush(()=>ordinary.deliverMessage(barFrame(30,11)));
  assert(latest().normalBars===2,'the ordinary feed stopped delivering while the research lane was down');
  assert(latest().normalStatus==='open','the ordinary feed lost its open status');
  // Late research frames on the retired socket reach neither view.
  const ordinaryBars=latest().normalBars;
  await flush(()=>{research.deliverOpen();research.deliverMessage(updateFrame({eventId:'run:TEST:7',lifecycleId:'run:TEST:7:0'}))});
  assert(latest().rows===1,'a late research frame added a row');
  assert(latest().normalBars===ordinaryBars,'a late research frame disturbed the ordinary feed');
  assert(research.sent.length===1,'the retired research socket wrote to the wire');
  await flush(()=>appState.emit('active'));
  assert(sockets.length===3,'foregrounding opened no new research socket');
  assert(ordinary.closeCalls===0,'foregrounding closed the ordinary feed socket');
  await unmount();
 }},
];

const OFF_SCENARIOS:Scenario[]=[
 {name:'flag absent opens no socket, no timer and no AppState subscription',run:async()=>{
  assert(EXPERIMENTAL_STRATEGIES_ENABLED===false,'page was loaded without the flag but the module gate is true');
  reset();await mount(ResearchProbe);
  assert(latest().enabled===false,'hook reports enabled with the flag absent');
  assert(sockets.length===0,`the flag-off build opened ${sockets.length} socket(s)`);
  assert(appState.listeners.size===0,'the flag-off build subscribed to AppState');
  assert(intervals.length===0,`the flag-off build started ${intervals.length} interval(s)`);
  assert(scheduled.length===0,'the flag-off build scheduled a timer');
  assert(latest().connection==='closed','the flag-off build reports a live connection');
  assert(latest().rows===0&&latest().status===null&&latest().gap===false,'the flag-off build invented state');
  // AppState churn must stay inert with the flag off.
  await flush(()=>{appState.emit('background');appState.emit('active')});
  assert(sockets.length===0,'the flag-off build connected on an AppState change');
  assert(intervals.length===0,'the flag-off build started a timer on an AppState change');
  await unmount();
 }},
];

(globalThis as any).run=async()=>{
 const results:{name:string;pass:boolean;error?:string}[]=[];
 for(const scenario of (EXPERIMENTAL_STRATEGIES_ENABLED?ON_SCENARIOS:OFF_SCENARIOS)){
  try{await scenario.run();results.push({name:scenario.name,pass:true})}
  catch(error){results.push({name:scenario.name,pass:false,error:String(error)})}
  finally{try{await unmount()}catch{/* already unmounted */}clearScheduled()}
 }
 return results;
};
