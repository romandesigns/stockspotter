// Bundles the REAL mobile experimental hook (and the real ordinary feed
// hook alongside it) for a browser React runtime, so the socket lifecycle
// can be driven deterministically. Same shape, same constraints and same
// honesty limits as tools/native-history/build.ts -- see the README.
//
// Only four things are substituted, and only because they are not
// JavaScript the hook owns:
//   - `react-native`     -> an AppState stub the test drives (the one RN
//                           surface this hook touches; nothing else from
//                           react-native is in its import graph).
//   - `./config`         -> fixed WS_URL/HTTP_URL, so no `process.env`
//                           read survives into the browser bundle.
//   - `react`/`react-dom`-> resolved from the mobile app's own installed
//                           copies, so fixture and hook share ONE React.
//   - the EXPO_PUBLIC_ flag -> a `globalThis` read, so one bundle can be
//                           loaded once with the flag on and once with it
//                           absent and the real module-level gate
//                           (`EXPERIMENTAL_STRATEGIES_ENABLED`) is what
//                           each page actually evaluates.
// The decoder, the row reducer, the backoff, the AppState policy, the
// heartbeat and every guard under test are the real modules, unmodified.
import {resolve} from 'node:path';
const here=import.meta.dir;
const arg=(name:string,fallback:string)=>{const i=Bun.argv.indexOf(name);if(i<0)return fallback;if(!Bun.argv[i+1]||Bun.argv[i+1].startsWith('--'))throw Error(`${name} needs a path`);return Bun.argv[i+1]};
const deps=resolve(arg('--deps-root',resolve(here,'../../apps/mobile')));
const sourceRoot=resolve(arg('--source-root',resolve(here,'../../apps/mobile/src')));
const stub:Record<string,string>={
 'react-native':`const listeners=new Set();
export const AppState={currentState:'active',addEventListener(type,handler){if(type!=='change')return {remove(){}};listeners.add(handler);return {remove(){listeners.delete(handler)}}}};
globalThis.__appState={
 listeners,
 reset(){listeners.clear();AppState.currentState='active'},
 emit(next){AppState.currentState=next;for(const handler of [...listeners])handler(next)},
};`,
 'config':'export const WS_URL="ws://experimental.fixture.invalid/ws";export const HTTP_URL="";',
};
const result=await Bun.build({
 entrypoints:[resolve(here,'fixture.tsx')],
 outdir:resolve(here,'dist'),
 target:'browser',
 define:{
  'process.env.NODE_ENV':'"development"',
  'process.env.EXPO_PUBLIC_EXPERIMENTAL_STRATEGIES':'globalThis.__EXPO_PUBLIC_EXPERIMENTAL_STRATEGIES',
 },
 plugins:[{name:'fixture',setup(b){
  b.onResolve({filter:/^hook-under-test$/},()=>({path:resolve(sourceRoot,'useExperimentalStrategies.ts')}));
  b.onResolve({filter:/^normal-feed-under-test$/},()=>({path:resolve(sourceRoot,'useRealtimeFeed.ts')}));
  b.onResolve({filter:/^react(?:-dom)?(?:\/.*)?$/},a=>({path:Bun.resolveSync(a.path,deps)}));
  b.onResolve({filter:/^@stockspotter\/shared-types$/},()=>({path:Bun.resolveSync('@stockspotter/shared-types',deps)}));
  b.onResolve({filter:/^react-native$/},()=>({path:'react-native',namespace:'fixture'}));
  b.onResolve({filter:/^\.\/config$/},()=>({path:'config',namespace:'fixture'}));
  b.onLoad({filter:/.*/,namespace:'fixture'},a=>({contents:stub[a.path],loader:'js'}));
 }}],
});
if(!result.success){console.error(result.logs);process.exit(1)}
// The flag is read from the query string BEFORE the module graph loads, so
// `EXPERIMENTAL_STRATEGIES_ENABLED` -- a module-level const, exactly as Expo
// inlines it -- is evaluated once per page load against that value.
await Bun.write(resolve(here,'dist/index.html'),`<!doctype html><meta charset="utf-8">
<script>globalThis.__EXPO_PUBLIC_EXPERIMENTAL_STRATEGIES=new URLSearchParams(location.search).get('flag')??undefined;</script>
<script type="module" src="/fixture.js"></script>
`);
