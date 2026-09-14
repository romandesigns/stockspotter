# Alpha programme reports

Markdown reports from the Stockspotter Alpha measurement programme: the platform audit, the
remediation batches, the measurement-engine build, and the three capture sessions.

**Session *data* is deliberately not in this repository.** See "Fetching the session data" below.

## The three sessions

| Session | Date | Classification | Why |
|---|---|---|---|
| 001 | 2026-09-10 | `INSTRUMENT VALIDATION SESSION 001` | Discovery capture hit an 8 GiB cap before the open; no regular-session coverage. Long horizons collapsed (1800s observed 0.06%). |
| 002 | 2026-09-11 | `INSTRUMENT VALIDATION SESSION 002` | Discovery repaired, but the 4,096 pending-outcome cap saturated ~789 s into the session. Regular-session 1800s observed 0.26%. |
| **Baseline** | **2026-09-14** | **`ANALYSIS BASELINE 001`** | All measurement invariants passed. Regular-session 1800s observed **76.60%**, zero capacity evictions, zero inverted episodes, continuous discovery coverage. |

All three are immutable. **Do not pool them** — R1, R2 and the capacity repair each changed
measurement semantics, so the three sessions are not directly comparable.

Start with `ANALYSIS-BASELINE-001-FINAL-CAPTURE-VALIDATION-2026-09-14.md`; it carries the full
55-item validation and a side-by-side comparison of all three sessions.

## Why the data is not in git

| | Size | |
|---|---:|---|
| These reports | ~540 KB | fine for git |
| Session artifacts | **~7.0 GB** | **34 files exceed GitHub's 100 MB hard limit** (largest 306 MB) |

GitHub refuses pushes containing files over 100 MB. Git LFS would weld ~7 GB into history
permanently — the opposite of what immutable, checksum-verified evidence needs. The artifacts carry
their own `SHA256SUMS` and `manifest.json`, which is a better integrity guarantee than git provides
for opaque binaries.

## Fetching the session data

The exports live on the VPS and are the source of truth. Pull them directly — do not route them
through another workstation:

```sh
mkdir -p ~/Desktop/wavystack/stockspotter-research/sessions
scp -r wavystack@72.60.30.64:/home/wavystack/export-2026-09-14 \
       ~/Desktop/wavystack/stockspotter-research/sessions/
scp -r wavystack@72.60.30.64:/home/wavystack/export-2026-09-11 \
       ~/Desktop/wavystack/stockspotter-research/sessions/
```

Each is ~3.3 GB. Verify after transfer — the artifact is only trustworthy if this passes:

```sh
cd ~/Desktop/wavystack/stockspotter-research/sessions/export-2026-09-14/session-2026-09-14
gunzip -t *.gz && shasum -a 256 -c SHA256SUMS
```

Expect `OK` on every line. Session 001 (2026-09-10) was never re-exported to `/home/wavystack`; it
exists only on the original workstation under `sessions/2026-09-10/`.

## Layout expected by the reports

```
stockspotter-research/
├── reports/                      <- this directory
└── sessions/
    ├── 2026-09-10/  raw/ + export/session-2026-09-10/
    ├── 2026-09-11/  session-2026-09-11/
    └── 2026-09-14/  session-2026-09-14/
```

## Do not commit session data here

If you ever place `sessions/` inside this repository, add it to `.gitignore` first. A single
accidental `git add -A` would attempt a multi-gigabyte commit and fail the push, or worse, succeed
against a non-GitHub remote and bloat the history irreversibly.
