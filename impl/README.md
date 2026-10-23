# Implementation record

Everything about **how** this software is built lives here. It is the working notebook for
whoever is editing the code: architecture, protocol references, decisions with their evidence,
task lists and verification commands.

It is not the interface contract. What the software is *supposed to do* is in
[`spec/`](../spec/README.md), in behavioural language, and that file is changed by the owner
of the project first. This folder follows it.

## Structure

```
impl/
├── README.md                       # this file
├── governance/
│   └── constitution.md             # rules every change must satisfy, plus the gates
├── protocol/                       # reference corpus: facts the code is written from
│   ├── a2dp-integration.md         #   A2DP / aptX Adaptive negotiation and framing
│   ├── codec-core.md               #   encoder core, tables
│   ├── research-rtp-framing.md     #   public-source research on the RTP envelope
│   ├── research-a2dp-caps-public.md
│   ├── research-foss-comparison.md
│   ├── px7s2e-a2dp-caps.md         #   a sink's advertised capabilities
│   ├── table-extraction.md         #   how the codec tables are obtained
│   └── QUESTIONS.md                #   questions waiting on the reference side
└── features/
    └── NNN-slug/
        ├── design.md               # how the behaviour in spec/ is produced
        ├── research.md             # decisions, alternatives rejected, measurements
        ├── tasks.md                # ordered work items with status and traceability
        └── quickstart.md           # commands a human runs to verify it on hardware
```

## How to use it

1. Read the behaviour first: `spec/features/<feature>/spec.md`.
2. Read `governance/constitution.md` — it is binding, including the pre-commit gates.
3. Check `protocol/` for the facts before inventing anything; if a fact is missing, add the
   question to `protocol/QUESTIONS.md` rather than guessing in code.
4. Record decisions in `features/<feature>/research.md` as they are made, with the evidence
   that settled them and the alternative that was rejected.
5. Keep `tasks.md` honest: a task is done when a test or a documented hardware observation
   proves its acceptance, not when the code compiles.

## Rules of this folder

- Measurements are quoted with how they were obtained (command, capture, log line), because a
  number without provenance is a rumour.
- A claim that was later shown wrong is deleted or corrected with a dated note — not quietly
  softened. Several conclusions in this project were retracted after better measurement; the
  record says so.
- Telemetry is not perception. Frame counts, backlog and SSRC describe framing; only listening
  describes sound. Say which one a statement rests on.
- No vendor binaries, decompiler output, or extracted tables are stored here. Facts are
  restated in our own words, with pointers to public sources.
