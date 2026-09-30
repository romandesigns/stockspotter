# Step 4 frozen preregistration: artifact-only branch

This orphan branch holds **only** the frozen Step-4 artifacts. It shares no
history with the code, so it can never be deployed. The release it binds is
`release/stockspotter-step4-20260930` at exactly the `implementationSha`
below. That branch's HEAD must stay equal to it, which is why the artifacts
do not live there.

`* -text` keeps git from ever rewriting these bytes.

| Artifact | Identity (what the protocol binds) |
|---|---|
| `step4-preregistration-v1.json` | RFC 8785 JCS SHA-256 `a5a46c8385b396d5a658c0c863a908e1b1ec80447696270bd8e7ecd57080e726` |
| `step4-freeze-manifest-v1.json` | JCS SHA-256 `9bec29c03bfd632539caa0eedefdfa9e3547cd787f1e418bd8a848175550f37e` |
| `trade-conditions-v2.final.json` | JCS SHA-256 `fd5776c179bbe2a5b2195dc093ea893c660c0703bc01d8b7a65f3dfffcf58bb7` |
| `trading-status-policy-v1.final.json` | JCS SHA-256 `aee1a87e0a26f2f36a26cab684799fb5ff351e58f85d659d662c3bd4d6353b10` |
| `STEP3-FINAL-VERDICT-AND-L2-SCOPE-20260928.md` | raw SHA-256 `46dfd17c727a03423d16174fe844b6f9f01b91488c9631348ea3331fe4531518` (protocolSha256 = gateSha256) |
| `OUTCOME-SOURCE-CONTRACT.md` | raw SHA-256 `8ccce4b994cf0526e5f2cf737b079df5c9f3f6d32f8ca0efa3fed40fdcd3915b` (the LF git blob at the implementation commit) |

- **implementationSha:** `210f58ef256122c45ad0a497cc54db9ce493f572`.
- **OI config fingerprint:** `oi-cfg-73ccdbaf661996ed`.
- **Generator inputs:** `freeze-template.json` and `approved-values.json`.
- **Raw file hashes:** in `SHA256SUMS`.

## Re-verify

Run these from a checkout of the implementation commit:

```
python ops/observation/observation_archive.py prereg-sha <artifact.json>
python ops/observation/prereg_freeze.py manifest <prereg> <conditions> <status> <STEP3 doc> <OUTCOME doc> <out>
```

The second command must reproduce `step4-freeze-manifest-v1.json` byte for
byte.
