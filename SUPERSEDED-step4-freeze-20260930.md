# SUPERSEDED — Step 4 freeze generation 1 (`step4-freeze-20260930`, bound to `210f58e`)

**Status:** SUPERSEDED. The cause was a FAILED PRODUCTION IMAGE-BUILD GATE.
Recorded 2026-10-01.

**Reason:** the production Docker image build failed because the
`ops/observation` test inputs were absent from the ws and python build
contexts. Both images run their test suites during `docker build`; CI tests
the full checkout and stayed green. The 2026-10-01 deploy of `210f58e`
stopped at build, and nothing was deployed.

**This is not an invalid research design.** The design remains frozen. Only
the implementation binding was superseded, by a release-engineering
correction (`de5b55e`: two Dockerfile `COPY` lines).

| | Generation 1 (superseded) | Generation 2 (current) |
|---|---|---|
| implementationSha | `210f58ef256122c45ad0a497cc54db9ce493f572` | `de5b55e66bd77b70fa7a026d178fb09634704264` |
| prereg JCS | `a5a46c8385b396d5a658c0c863a908e1b1ec80447696270bd8e7ecd57080e726` | `746446819af285d20a749a81eb27cf8984d7d488860beb4f8f575106942be7f8` |
| manifest JCS | `9bec29c03bfd632539caa0eedefdfa9e3547cd787f1e418bd8a848175550f37e` | `ef5075a16d1bf44e41d8bc11e7892299e30905ac86264417799c5e82b660dbf2` |
| Every other frozen value | — | identical |

Generation 1's files are kept intact and unmodified. No session was ever
captured or designated under generation 1.
