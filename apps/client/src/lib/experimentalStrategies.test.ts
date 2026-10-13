import { describe, expect, test } from "bun:test";
import { decodeExperimentalMessage, reduceExperimentalRows, type ExperimentalStrategyUpdate } from "./experimentalStrategies";

function event(): ExperimentalStrategyUpdate {
  return { type:"experimental_strategy_update",contractVersion:"iac-extreme-minute-v1",eventId:"run:TEST:1",lifecycleId:"run:TEST:0:0",supersedes:null,
    symbol:"TEST",strategy:"iac",kind:"candidate",marketDay:"2026-08-26",eventTimeMs:1000,receivedAtMs:1100,decisionAtMs:1200,
    anchorAvailableAtMs:1200,anchorEventTimeMs:1000,anchorPrice:30,observedPrice:30,reason:"research_thresholds_met",
    evidence:{baselineBars:20,baselineRange:1,baselineVolume:100,compressionRatio:0.5,expansionRatio:2,participationRatio:3,
      moveFraction:0.03,pathEfficiency:null,floatEvidence:"unavailable"} };
}
describe("experimental lane",()=>{
  test("strict decoder keeps >20 and >25 candidates without inheriting price caps",()=>{
    expect(decodeExperimentalMessage(event())).toEqual(event());
    expect(decodeExperimentalMessage({...event(),observedPrice:50})).not.toBeNull();
  });
  test("malformed and noncausal evidence is rejected",()=>{
    for (const bad of [{...event(),anchorPrice:NaN},{...event(),evidence:{}},{...event(),kind:"profit"},
      {...event(),decisionAtMs:100},{...event(),contractVersion:"future"}]) expect(decodeExperimentalMessage(bad)).toBeNull();
  });
  test("replays cannot replace later lifecycle with earlier snapshot",()=>{
    const candidate=event();const held={...candidate,eventId:"run:TEST:2",supersedes:candidate.eventId,kind:"held" as const,decisionAtMs:5000};
    const rows=reduceExperimentalRows([candidate],held);
    expect(rows).toEqual([held]);expect(reduceExperimentalRows(rows,candidate)).toBe(rows);
    expect(reduceExperimentalRows(rows,held)).toBe(rows);
  });
  test("same timestamp transitions retain arrival/supersession order",()=>{
    const a=event();const b={...a,eventId:"run:TEST:2",supersedes:a.eventId,kind:"held" as const};
    expect(reduceExperimentalRows([a],b)).toEqual([b]);expect(reduceExperimentalRows([b],a)).toEqual([b]);
  });
  test("dedicated lifecycle list is bounded independently",()=>{
    let rows:ExperimentalStrategyUpdate[]=[];
    for(let i=0;i<150;i++)rows=reduceExperimentalRows(rows,{...event(),eventId:`e${i}`,lifecycleId:`l${i}`});
    expect(rows).toHaveLength(100);expect(rows[0].lifecycleId).toBe("l149");
  });
  test("health and explicit gap have separate shapes",()=>{
    expect(decodeExperimentalMessage({type:"experimental_strategy_gap"})).toEqual({type:"experimental_strategy_gap"});
    expect(decodeExperimentalMessage({type:"experimental_strategy_status",iacEnabled:true,extremeMomentumEnabled:false,
      healthy:false,sourceAtMs:null,laggedEvents:1,evictions:0,processingWarnings:0,activeSymbols:0,reason:"gap",coverage:"observed_subscribed_symbols"})).not.toBeNull();
  });
  function statusFrame(extra:Record<string,unknown>={}){
    return {type:"experimental_strategy_status",iacEnabled:true,extremeMomentumEnabled:true,healthy:true,sourceAtMs:10_000,
      laggedEvents:0,evictions:0,processingWarnings:0,activeSymbols:7,reason:"observing",coverage:"observed_subscribed_symbols",...extra};
  }
  test("capacity and readiness telemetry is additive, so absent stays valid",()=>{
    // The whole amendment is optional fields: a server that sends none
    // must keep decoding, and one that sends all of them must too.
    expect(decodeExperimentalMessage(statusFrame())).not.toBeNull();
    expect(decodeExperimentalMessage(statusFrame({symbolLimit:16_384,iacReadySymbols:3,extremeReadySymbols:7,
      capacityDroppedUpdates:4,recentCapacityDroppedSymbols:["ZZZ"],idleReclaims:1,idleResets:2,acceptedBarUpdates:900,
      warmupBarUpdates:40,duplicateBarUpdates:6,revisionResets:3,missingMinuteResets:5,invalidBarUpdates:8,
      outputLaggedEvents:11}))).not.toBeNull();
    expect(decodeExperimentalMessage(statusFrame({iacReadySymbols:0,invalidBarUpdates:0}))).not.toBeNull();
  });
  test("invalid counts and incoherent readiness are rejected, not clamped",()=>{
    // A count that is negative, fractional, unsafe, null or a string is
    // not a count, and silently coercing one would let the panel render
    // a coverage figure the server never reported.
    for (const bad of [{invalidBarUpdates:-1},{invalidBarUpdates:1.5},{invalidBarUpdates:"8"},
      {revisionResets:null},{missingMinuteResets:Number.NaN},{idleResets:Number.MAX_SAFE_INTEGER+2},
      {duplicateBarUpdates:-0.5},{outputLaggedEvents:[]},
      // Readiness cannot exceed what is observed; a ceiling cannot sit
      // below the observed count or below one symbol.
      {iacReadySymbols:8},{extremeReadySymbols:99},{symbolLimit:3},{symbolLimit:0},
      {recentCapacityDroppedSymbols:[1]},{recentCapacityDroppedSymbols:["WAY_TOO_LONG_SYMBOL_NAME"]}])
      expect(decodeExperimentalMessage(statusFrame(bad))).toBeNull();
    // Readiness exactly at the observed count is the valid boundary.
    expect(decodeExperimentalMessage(statusFrame({iacReadySymbols:7}))).not.toBeNull();
    expect(decodeExperimentalMessage(statusFrame({symbolLimit:7}))).not.toBeNull();
  });
});
