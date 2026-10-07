import React, {useLayoutEffect} from 'react';
import {createRoot} from 'react-dom/client';
import {flushSync} from 'react-dom';
import {useChartBars} from 'hook-under-test';
let pending:any[]=[]; let commits:any[]=[];
(window as any).fakeFetch=(url:string)=>new Promise((resolve,reject)=>pending.push({url,resolve,reject}));
const bar=(price:number,time=1800000000)=>({time,open:price,high:price,low:price,close:price,volume:1});
const live=(price:number)=>[{symbol:'test',timestamp:new Date(1800000060*1000).toISOString(),open:price,high:price,low:price,close:price,volume:1}];
function View(p:any){const bars=useChartBars(p.symbol,p.live,p.range);useLayoutEffect(()=>{commits.push({symbol:p.symbol,range:p.range,prices:bars.map(b=>b.close)});});return null;}
const tick=()=>new Promise(r=>setTimeout(r,0));
const assert=(ok:boolean,message:string)=>{if(!ok)throw Error(message)};
(window as any).run=async()=>{
 const results=[];
 for(const name of ['first history','null symbol','symbol switch','rapid return','range switches','late responses','request failure','live ticks']){
  pending=[];commits=[];const el=document.createElement('div');document.body.append(el);const root=createRoot(el);
  let props:any={symbol:'A',range:'1D',live:[]};
  const render=(next:any)=>{props={...props,...next};flushSync(()=>root.render(<View {...props}/>));};
  const settle=async(index:number,price:number)=>{pending[index].resolve({ok:true,json:async()=>[bar(price)]});await tick();await tick();};
  const since=(index:number,forbidden:number)=>assert(commits.slice(index).every(c=>!c.prices.includes(forbidden)),`stale ${forbidden} in committed render: ${JSON.stringify(commits.slice(index))}`);
  try{
   render({});await tick();assert(commits[0].prices.length===0,'initial empty');
   if(name==='late responses'){
    render({symbol:'B'});await tick();render({range:'1W'});await tick();const start=commits.length;
    await settle(0,10);await settle(1,20);since(start,10);since(start,20);await settle(2,30);assert(commits.at(-1).prices.includes(30),'current response missing');
   }else{
    await settle(0,10);assert(commits.at(-1).prices.includes(10),'history absent');const start=commits.length;
    if(name==='first history'){assert(pending.length===1,'duplicate fetch');assert(commits.filter(c=>c.prices.includes(10)).length===1,'duplicate history commit');}
    if(name==='null symbol'){render({symbol:null});await tick();since(start,10);assert(commits.at(-1).prices.length===0,'null history');}
    if(name==='symbol switch'){render({symbol:'B',live:live(20)});await tick();since(start,10);assert(commits.at(-1).prices.includes(20),'live missing');}
    if(name==='rapid return'){render({symbol:'B'});await tick();render({symbol:'A'});await tick();since(start,10);await settle(1,20);since(start,20);await settle(2,30);assert(commits.at(-1).prices.includes(30),'refetch absent');}
    if(name==='range switches'){render({range:'1W'});await tick();since(start,10);await settle(1,20);const next=commits.length;render({range:'1M'});await tick();since(next,20);await settle(2,30);assert(commits.at(-1).prices.includes(30),'range history absent');}
    if(name==='request failure'){render({symbol:'B',live:live(20)});await tick();pending[1].reject(Error('expected'));await tick();since(start,10);assert(JSON.stringify(commits.at(-1).prices)==='[20]','failure not live only');}
    if(name==='live ticks'){render({live:live(20)});await tick();assert(JSON.stringify(commits.at(-1).prices)==='[10,20]','history cleared');assert(pending.length===1,'live refetched');}
   }
   results.push({name,pass:true});
  }catch(e){results.push({name,pass:false,error:String(e)});}finally{flushSync(()=>root.unmount());el.remove();}
 }
 return results;
};
