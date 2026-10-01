# Step 4 frozen preregistration — generation 2 (bound to `de5b55e`)

This package **supersedes the implementation binding** of generation 1
(`step4-freeze-20260930`, bound to `210f58e`). **It is not a redesign.** The
only semantic difference between the two preregistrations is
`implementationSha`. Every other frozen research value is byte-identical:
the old file equals the new one with the SHA swapped.

**Why generation 1 was superseded:** the production Docker image build
failed at `210f58e`. The ws and python Dockerfiles did not copy the
`ops/observation` test inputs, so the 2026-10-01 deploy stopped at build and
nothing was deployed. `de5b55e` adds those two `COPY` lines, and nothing else.

`* -text` (on the artifact branch) keeps git from ever rewriting these bytes.

| Artifact | Identity (what the protocol binds) |
|---|---|
| `step4-preregistration-v1.json` | RFC 8785 JCS SHA-256 `746446819af285d20a749a81eb27cf8984d7d488860beb4f8f575106942be7f8` |
| `step4-freeze-manifest-v1.json` | JCS SHA-256 `ef5075a16d1bf44e41d8bc11e7892299e30905ac86264417799c5e82b660dbf2` |
| `trade-conditions-v2.final.json` | JCS SHA-256 `fd5776c179bbe2a5b2195dc093ea893c660c0703bc01d8b7a65f3dfffcf58bb7` (unchanged) |
| `trading-status-policy-v1.final.json` | JCS SHA-256 `aee1a87e0a26f2f36a26cab684799fb5ff351e58f85d659d662c3bd4d6353b10` (unchanged) |
| `STEP3-FINAL-VERDICT-AND-L2-SCOPE-20260928.md` | raw SHA-256 `46dfd17c727a03423d16174fe844b6f9f01b91488c9631348ea3331fe4531518` (protocolSha256 = gateSha256; unchanged) |
| `OUTCOME-SOURCE-CONTRACT.md` | raw SHA-256 `8ccce4b994cf0526e5f2cf737b079df5c9f3f6d32f8ca0efa3fed40fdcd3915b` (LF git blob at `de5b55e`; unchanged) |

- **implementationSha:** `de5b55e66bd77b70fa7a026d178fb09634704264`.
- **OI config fingerprint:** `oi-cfg-73ccdbaf661996ed` (unchanged).
- **Generator inputs:**
  - `freeze-template.json`: byte-identical to generation 1;
  - `approved-values.json`: identical to generation 1 except `implementationSha`.
- **Raw file hashes:** in `SHA256SUMS`.

## Re-verify

Run these from a checkout of `de5b55e`:

```
python ops/observation/observation_archive.py prereg-sha <artifact.json>
python ops/observation/prereg_freeze.py manifest <prereg> <conditions> <status> <STEP3 doc> <OUTCOME doc> <out>
```

The second command must reproduce `step4-freeze-manifest-v1.json` byte for
byte. The manifest keys documents by basename, so keep the file names.
