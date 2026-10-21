# Progress

## Done

- Native-link cone repair committed as `d1fe720c` on the original repair branch and pushed to the fork.
- Upgrade based on upstream v0.15.0 (`fa691707`), with repair `b1032e80` and partial-turn regression adaptation `b36998c2`, pushed on `upgrade/v0.15.0-cone-repair`.
- Format contract, linked-design compatibility, bidirectional edit/refresh/reopen and immutable originals verified using disposable copies. Final plan and four review gates approved.
- Full ci/test gates ran and exited 1 with classified non-repair-caused failures; canonical wasm gate passed; oracle passed 82/82. Boolean property failures reproduced at the clean upstream tag; regenerated oracle readouts matched upstream baseline.
- Public-safe fork guide added in documentation commit `9c41f4d9` and linked from README. Raw private receipts remain local, not part of the published fork.

## Next

- Maintainer decision: headless MCP trial/adoption or upstream PR. Follow `docs/fork-maintenance.md`.
- Optional follow-up: empirical baseline for the colour-vision scenario (currently source-level classification only) and the separate ruled-B-spline classification audit.

## Known limitations

- Candidate build log was lost during a disk incident; its measured SHA and recorded build context corroborate identity, but the binary embeds no commit SHA.
- Existing GPU parity failures, UI test SIGSEGV, upstream boolean failures and a load-sensitive fillet time bound are recorded, not fixed by this repair.
