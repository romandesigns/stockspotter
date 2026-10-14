import { useEffect, useState } from "react";
import { decodeExperimentalMessage, reduceExperimentalRows, type ExperimentalStrategyUpdate, type ExperimentalStrategyStatus } from "./experimentalStrategies";
import { getAccessKey } from "@stockspotter/shared-types";
import { resolveWsUrl } from "./config";
import { researchBuildDefault, RESEARCH_RECONNECT_BASE_MS, retryResearchRejection } from "./researchPreferences";
import type { ConnectionStatus } from "./useRealtimeFeed";

export function useExperimentalStrategies(enabled = researchBuildDefault()) {
  const [error, setError] = useState<string | null>(null);
  const [rows,setRows]=useState<ExperimentalStrategyUpdate[]>([]);
  const [status,setStatus]=useState<ExperimentalStrategyStatus|null>(null);
  const [gap,setGap]=useState(false);
  const [connection,setConnection]=useState<ConnectionStatus>("closed");
  useEffect(()=>{
    if(!enabled){setRows([]);setStatus(null);setGap(false);setError(null);setConnection("closed");return;}
    let stopped=false;let reconnect:ReturnType<typeof setTimeout>|undefined;
    let socket:WebSocket|undefined;let lastTransportAt=Date.now();let connectedBefore=false;
    let reconnectDelay=RESEARCH_RECONNECT_BASE_MS; let terminalRejected=false;
    function connect(){
      if(stopped)return;
      setError(null);setConnection("connecting");const currentSocket=new WebSocket(resolveWsUrl());socket=currentSocket;
      currentSocket.addEventListener("open",()=>{
        if(stopped || socket !== currentSocket)return;
        if(connectedBefore)setGap(true);else {setRows([]);setStatus(null);setGap(false);}
        connectedBefore=true;lastTransportAt=Date.now();
        socket?.send(JSON.stringify({type:"hello",protocolVersion:1,client:"web",token:getAccessKey(),experimentalStrategies:true,experimentalOnly:true}));
      });
      currentSocket.addEventListener("message",raw=>{
        if(stopped || socket !== currentSocket)return;
        lastTransportAt=Date.now();
        try{
          const parsed:unknown=JSON.parse(raw.data as string);
          if(typeof parsed!=="object"||parsed===null||!("type" in parsed)) {setGap(true);return;}
          if(parsed.type==="welcome"){setConnection("open");return;}
          if(parsed.type==="ping"&&"at" in parsed){socket?.send(JSON.stringify({type:"pong",at:parsed.at}));return;}
          if(parsed.type==="pong")return;
          if(parsed.type==="hello_rejected"){
            const reason="reason" in parsed ? parsed.reason : undefined;
            terminalRejected=!retryResearchRejection(reason);
            setError(typeof reason === "string" ? reason.slice(0, 120) : "Connection refused");
            if(terminalRejected)clearInterval(heartbeat);
            setConnection("closed");socket?.close();return;
          }
          const message=decodeExperimentalMessage(parsed);
          if(!message){setGap(true);return;}
          if(message.type==="experimental_strategy_status"){setStatus(message);if(message.healthy&&(message.iacEnabled||message.extremeMomentumEnabled))reconnectDelay=RESEARCH_RECONNECT_BASE_MS;}
          else if(message.type==="experimental_strategy_gap")setGap(true);
          else setRows(previous=>reduceExperimentalRows(previous,message));
        }catch{setGap(true);}
      });
      currentSocket.addEventListener("close",()=>{
        if(stopped || socket !== currentSocket)return;
        setConnection("closed");
        if(!stopped&&!terminalRejected){reconnect=setTimeout(connect,Math.round(reconnectDelay*(0.8+Math.random()*0.4)));reconnectDelay=Math.min(60000,reconnectDelay*2);}
      });
      currentSocket.addEventListener("error",()=>{if(!stopped && socket === currentSocket)currentSocket.close();});
    }
    const heartbeat=setInterval(()=>{
      if(socket?.readyState===WebSocket.OPEN){
        if(Date.now()-lastTransportAt>45000){socket.close();return;}
        socket.send(JSON.stringify({type:"ping",at:new Date().toISOString()}));
      }
    },15000);
    connect();
    return()=>{stopped=true;clearInterval(heartbeat);if(reconnect)clearTimeout(reconnect);socket?.close();};
  },[enabled]);
  return {enabled, rows: enabled ? rows : [], status: enabled ? status : null, gap: enabled && gap, connection: enabled ? connection : "closed" as ConnectionStatus, error: enabled ? error : null};
}
