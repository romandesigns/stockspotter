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
});
