# Contributing

Thanks for helping. This is an early, small project, so please open an issue
before starting anything large.

## Before you push

Run `tools/check`. It runs formatting, Clippy (warnings are errors) and all tests.

## Rules that keep the tool safe

- **No panics on input.** The workspace denies `unsafe`, `unwrap`, `expect`, `panic!`,
  slice indexing and string slicing. Dumps come from machines in trouble, so a bad
  or truncated file must give an error, not a crash. Test code may use `unwrap`.
- **Read-only.** The tool must never write to a database or to a switch.
- **Say what you verified.** A diagnosis message must name its evidence. If the data
  is not enough to be sure, say "may" or report nothing. A wrong cause is worse
  than none.
- **Check formats against the SONiC source**, and cite the file in the pull request.

## Tests

- Unit and integration tests are in `crates/*/tests`.
- Golden tests in `crates/cli/tests/golden.rs` pin the exact output and exit code.
  After an intended change, run `UPDATE_GOLDEN=1 cargo test -p sonic-trace` and review
  the diff in `tests/golden/`.
- Fixtures in `fixtures/community/` are copied unmodified from sonic-utilities.
  Add new data under a different folder and record its origin in `NOTICE`.
- Never add data from a customer or employer. Use public or hand-made data only.

## Commits

Use Conventional Commits, for example `fix(route): handle missing next-hop group`.
Sign off each commit with `git commit -s`.
