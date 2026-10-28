<!-- One PR belongs to one work package (docs/M1_PLAN.md, docs/M2_PLAN.md, docs/M3_PLAN.md, docs/M4_PLAN.md). -->

## Work package

WP<!-- number from the milestone plan -->, PR "<!-- name from the plan, if it has one -->"

## Summary

<!-- What this PR does, in a few sentences. -->

## Owned paths touched

<!-- List every path changed. All must belong to this WP (its milestone plan, "Owned paths" and "Ownership"),
     except tool-generated `Cargo.lock` and `schemas/`. -->

-

- [ ] No file outside this WP's owned paths changed (except `Cargo.lock` / `schemas/` written by tools)
- [ ] Every new diagnostic code has `crates/<crate>/explain/RE####.md` and an entry in the crate's `DIAGNOSTICS`
- [ ] New dependencies (if any) have an ADR in `docs/DECISIONS.md` and a row in `docs/DEPENDENCIES.md`
- [ ] Frozen contracts (each milestone plan, "Cross-WP contracts") are unchanged, or the defining and consuming WPs reviewed the change

## Acceptance output

<!-- Paste the output of the WP's acceptance commands, run in the reference container
     with `export CARGO_BUILD_JOBS=2`. -->

```text

```

## Doc changes requested

<!-- README.md and docs/ are maintained by the maintainer after WP0. List the changes
     this PR needs there (file, section, what to change), or "none". -->
