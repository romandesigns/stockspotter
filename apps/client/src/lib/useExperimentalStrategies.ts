import { useEffect, useState } from "react";
import { decodeExperimentalMessage, reduceExperimentalRows, getAccessKey, type ExperimentalStrategyUpdate, type ExperimentalStrategyStatus } from "@stockspotter/shared-types";
import { resolveWsUrl } from "./config";
import type { ConnectionStatus } from "./useRealtimeFeed";

export function useExperimentalStrategies(enabled = (import.meta as { env?: Record<string,string> }).env?.VITE_EXPERIMENTAL_STRATEGIES === "true") {
  const [rows,setRows]=useState<ExperimentalStrategyUpdate[]>([]);
  const [status,setStatus]=useState<ExperimentalStrategyStatus|null>(null);
  const [gap,setGap]=useState(false);
  const [connection,setConnection]=useState<ConnectionStatus>("closed");
  useEffect(()=>{
    if(!enabled)return;
    let stopped=false;let reconnect:ReturnType<typeof setTimeout>|undefined;
    let socket:WebSocket|undefined;let lastTransportAt=Date.now();let connectedBefore=false;
    let reconnectDelay=3000;
    function connect(){
      if(stopped)return;
      setConnection("connecting");socket=new WebSocket(resolveWsUrl());
      socket.addEventListener("open",()=>{
        if(connectedBefore)setGap(true);connectedBefore=true;lastTransportAt=Date.now();
        socket?.send(JSON.stringify({type:"hello",protocolVersion:1,client:"web",token:getAccessKey(),experimentalStrategies:true,experimentalOnly:true}));
      });
      socket.addEventListener("message",raw=>{
        lastTransportAt=Date.now();
        try{
          const parsed:unknown=JSON.parse(raw.data as string);
          if(typeof parsed!=="object"||parsed===null||!("type" in parsed)) {setGap(true);return;}
          if(parsed.type==="welcome"){setConnection("open");return;}
          if(parsed.type==="ping"&&"at" in parsed){socket?.send(JSON.stringify({type:"pong",at:parsed.at}));return;}
          if(parsed.type==="pong")return;
          if(parsed.type==="hello_rejected"){setConnection("closed");socket?.close();return;}
          const message=decodeExperimentalMessage(parsed);
          if(!message){setGap(true);return;}
          if(message.type==="experimental_strategy_status"){setStatus(message);if(message.healthy&&(message.iacEnabled||message.extremeMomentumEnabled))reconnectDelay=3000;}
          else if(message.type==="experimental_strategy_gap")setGap(true);
          else setRows(previous=>reduceExperimentalRows(previous,message));
        }catch{setGap(true);}
      });
      socket.addEventListener("close",()=>{
        setConnection("closed");
        if(!stopped){reconnect=setTimeout(connect,Math.round(reconnectDelay*(0.8+Math.random()*0.4)));reconnectDelay=Math.min(60000,reconnectDelay*2);}
      });
      socket.addEventListener("error",()=>socket?.close());
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
  return {enabled,rows,status,gap,connection};
}
