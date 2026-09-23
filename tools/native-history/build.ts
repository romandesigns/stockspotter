import {resolve} from 'node:path';
const here=import.meta.dir;
const arg=(name:string,fallback:string)=>{const i=Bun.argv.indexOf(name);if(i<0)return fallback;if(!Bun.argv[i+1]||Bun.argv[i+1].startsWith('--'))throw Error(`${name} needs a path`);return Bun.argv[i+1]};
const deps=resolve(arg('--deps-root',resolve(here,'../../apps/mobile')));
const source=resolve(arg('--source-root',resolve(here,'../../apps/mobile/src')),'useChartBars.ts');
const result=await Bun.build({entrypoints:[resolve(here,'fixture.tsx')],outdir:resolve(here,'dist'),target:'browser',plugins:[{name:'fixture',setup(b){
 b.onResolve({filter:/^hook-under-test$/},()=>({path:source}));
 b.onResolve({filter:/^react(?:-dom)?(?:\/.*)?$/},a=>({path:Bun.resolveSync(a.path,deps)}));
 b.onResolve({filter:/^@stockspotter\/shared-types$/},()=>({path:'fetch',namespace:'fixture'}));
 b.onResolve({filter:/^\.\/config$/},()=>({path:'config',namespace:'fixture'}));
 b.onLoad({filter:/.*/,namespace:'fixture'},a=>({contents:a.path==='fetch'?'export const authenticatedFetch=(...a)=>window.fakeFetch(...a);':'export const HTTP_URL="";',loader:'js'}));
}}]});if(!result.success){console.error(result.logs);process.exit(1)}
await Bun.write(resolve(here,'dist/index.html'),'<script type="module" src="/fixture.js"></script>');
