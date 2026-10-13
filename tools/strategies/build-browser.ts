import { resolve } from "node:path";
const client = resolve("apps/client");
const result = await Bun.build({ entrypoints:["tools/strategies/browser-fixture.tsx"], outdir:"tools/strategies/dist",target:"browser",
  plugins:[{name:"resolve-client-workspace",setup(build){
    build.onResolve({filter:/^(react|react-dom|@stockspotter\/shared-types)(\/|$)/}, args => ({path:Bun.resolveSync(args.path,client)}));
  }}]});
if (!result.success) throw new Error(result.logs.map(String).join("\n"));
