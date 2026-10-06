// Runs the Ross five-pillar rule tests inside the suite that actually runs.
//
// The nine rule tests live beside the rule, in
// packages/shared-types/src/rossFivePillars.test.ts, but packages/shared-types
// has no test script and nothing invokes one: CI, the root `test` script and
// the web image builder (apps/client/Dockerfile) all run `bun test` from
// apps/client only. So the rule the badge renders from shipped with passing
// tests that no gate ever executed -- a threshold could change and every
// check would stay green.
//
// Importing the file registers its `describe` block in this run. The tests
// stay where they are, next to the code they cover, rather than being copied
// here: a copy would drift, and mobile uses the same rule. Same reasoning as
// attentionParity.test.ts reaching into apps/mobile -- the assertion lives in
// the suite that runs. `tsc -b` typechecks the imported file too, because
// tests are deliberately inside this project (see tsconfig.app.json).

import "../../../../packages/shared-types/src/rossFivePillars.test";
