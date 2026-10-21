# Fork maintenance and validation state

This fork keeps the upstream stable release as its base and carries one small kernel repair on
top. It is not a replacement for upstream, and this page is the state of record for the fork: the
branches, the design format, the validation that was actually run, the known gaps, and how to
trial the candidate binary without changing the installed application.

## Branches and commits

| Ref | Commit | Contents |
|---|---|---|
| `v0.15.0` (tag) | `fa691707c224cae580c42201ed3a22d5e4b8c716` | Upstream 0.15.0 release; the fork base |
| `upgrade/v0.15.0-cone-repair` | `b36998c2` | Base plus the two commits below |
| `fix/step-out-cone-classification-band` | `d1fe720c` | Original repair branch, kept unchanged |

On `upgrade/v0.15.0-cone-repair`, in order:

- `b1032e80` `fix(kernel): classify revolved-profile cones by half-angle band in STEP export`:
  a cherry-pick of `d1fe720c`.
- `b36998c2` `test(kernel): re-anchor the near-plane cone regressions to a partial turn`: the only
  deliberate deviation from the original repair.

Both branches are pushed to the fork remote; `git remote -v` shows the remote names and URLs.
The base is tag-pinned on purpose. Do not merge the current upstream `main` into this branch:
the fork tracks the stable tag, and `main` has moved on to a newer design format.

## The repair

`analytic_revolution` in `crates/kernel/src/step_out.rs` classifies revolved-profile cones by
their half-angle band when writing STEP, instead of trusting the near-plane angle alone. The
production hunk is byte-identical to `d1fe720c`; the STEP importer band is untouched.

The regression tests were re-anchored to a partial turn in `b36998c2`. The v0.15.0 base heals a
near-plane full-revolution cone into a plane before the writer runs, so the original full-turn
fixtures would pass without exercising the repair. Assertions, band constants and tolerances
are unchanged; only fixture construction and its volume scaling were adapted.

## Design format

The `v0.15.0` base reads and writes design format **26**. Format 27 belongs to the upstream
`main` tip, which has two pending format change files; the tag has none. The fork does not bump
the format guard or the version. Designs at formats 13, 21 and 25 open and validate on this
base.

## Validation state

This is not a green-CI claim. The quick gates and the reference suite pass; the full suites exit
non-zero with every failure classified as upstream, environment or contention, and none reaches
the repair surface.

- Quick gates passed in the full run: `fmt`, assets, icons, licences, sorted lists and layer
  checks, and `clippy -D warnings`.
- `cargo xtask wasm` passed with 12 crates after the `wasm32-unknown-unknown` target was
  installed. In the earlier CI attempt the same step failed only because that target was
  missing.
- `cargo xtask oracle`: exit 0, **82/82** cases pass. The run regenerated `docs/oracle.md` with a
  two-line surface-distance readout change in the two FreeCAD loft cases; both remain `pass`.
- `cargo xtask ci`: exit 1. Failures:
  - `wasm32-unknown-unknown check`: environment, target missing during that attempt; the gate
    passes with the target installed.
  - `gpu lib solidcraft_gpu` (three torus-parity tests): pre-existing environment on the test
    machine.
  - `ui-egui lib solidcraft_ui_egui` (SIGSEGV mid-suite): pre-existing environment.
  - `ui-egui test scenarios::colour_vision_setting`: upstream/environment, classified at source
    level only; see the limitations below.
  - `kernel test boolean_props` (four tests): upstream regression in `v0.15.0`, reproduced
    identically at a clean `fa691707` baseline.
- `cargo xtask test`: exit 1 with the same suite failures plus an `engine lib` fillet
  wall-clock test just over its 30 s bound (31.29 s) under load; classified as contention.

Runtime validation of the candidate used disposable copies only: standalone designs at formats
13, 21 and 25, a linked assembly with relative part links, bidirectional parameter edits,
explicit link refresh, save/reopen, and sibling isolation. Originals were re-hashed unchanged.
Observed maxima across the cycles were 1.945e-5 relative volume, 4.291e-6 relative area and
1.78e-10 mm bounds, inside the acceptance limits of 0.01 percent and 1e-4 mm. One behaviour is
recorded rather than explained: the first refresh of an older linked assembly reports both
bodies updated with no source edit, attributed in the receipts to exporter-version drift in
embedded STEP blobs; sibling isolation holds after one normalising refresh.

## Candidate binary and its limits

The candidate CLI is
`target/fork-upgrade-v0.15.0-r1/release/solidcraft-cli`, version `solidcraft-cli 0.15.0`,
SHA-256 `5d074670988500f69dc066105874ca9585aee4c4d1db2dc28755d623d55ecc54`, 387726888 bytes.

Two provenance gaps are recorded, not hidden:

- The original build log was lost during a disk-full incident. Build completion is a prior
  observation, so artifact identity rests on the pinned target path, the measured hash and the
  recorded build context.
- The binary embeds no git commit SHA, so it is corroborated against this tree but not
  cryptographically bound to it.

## Trial the candidate through the MCP launcher

The harness MCP launcher (`$HOME/.config/opencode/scripts/start-solidcraft-mcp.sh`) uses the
installed release CLI by default and keeps its state under `$SOLIDCRAFT_ROOT/state` (the
release folder's `state/data`, `state/config` and `state/cache`). `SOLIDCRAFT_MCP_CLI` overrides
only the binary; the state directory does not move. That is the sanctioned trial path. There is
no installation switch.

From the fork checkout, with the port unused:

```sh
cd /path/to/fork-checkout
SOLIDCRAFT_MCP_CLI="$PWD/target/fork-upgrade-v0.15.0-r1/release/solidcraft-cli" \
SOLIDCRAFT_CONTROL_PORT=17878 \
opencode
```

The path above is checkout-specific and is shown as an example. `17878` is the port used for
the trial; choose another unused port if it is taken.

- Use a non-default port for a headless trial. The launcher probes the configured port and
  bridges to an app that answers with the shared control token, so the default `7878` can
  connect to an already running installed desktop app instead of starting a candidate session.
- Trial on copies in a disposable folder. Linked assemblies resolve part paths relative to the
  host file (for example `../parts/...`), so copying a host alone breaks the links; copy the
  folder structure.
- Leaving out both variables and starting plain `opencode` returns to the installed release
  binary and its state. No launcher, installation or state folder was changed.

No upstream pull request and no release were created for this fork. Those remain separate
decisions.

## Rechecking the state

```sh
git fetch fork
git rev-parse v0.15.0
git log --oneline v0.15.0..fork/upgrade/v0.15.0-cone-repair
git ls-remote --heads fork
```

Expected tag: `fa691707c224cae580c42201ed3a22d5e4b8c716`. Expected branch tip:
`b36998c2` on `upgrade/v0.15.0-cone-repair` and `d1fe720c` on
`fix/step-out-cone-classification-band`. Refetch upstream before any future integration work;
the upstream tip is not the fork base.

## Known limitations

- `colour_vision_setting` is classified at source level only: the scenario has no path into the
  STEP writer and fails with a headless widget-lookup symptom. An empirical `ui-egui` baseline
  was deferred for disk reasons, so the classification is not empirical.
- The build-log and embedded-SHA gaps above remain.
- The repair and the launcher trial are documented here; upstreaming the two commits remains
  open.
