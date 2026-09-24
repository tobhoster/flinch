# Contributing

Bug reports, ideas and pull requests are welcome. For anything bigger than a
fix, open an issue first so we can agree on the approach before you write it.
Security problems go to a [private report](SECURITY.md), never an issue.

## Build and test

What CI runs:

```bash
cargo clippy --workspace --all-targets --locked
cargo test --workspace --locked
cd frontend && npm ci && npm run build     # the UI, into frontend/dist
```

To try a change in the UI, serve the demo snapshot:

```bash
FLINCH_STATE_DIR=/tmp/flinch-demo cargo run -p flinch-web --bin flinch-demo
FLINCH_STATE_DIR=/tmp/flinch-demo FLINCH_WEB_DIR=frontend/dist FLINCH_WEB_TOKEN=demo \
  cargo run -p flinch-web --bin flinch-web
```

Open <http://localhost:7911> and unlock it with `demo`.

## Rules for changes

- **Fail closed.** Anything that can change what gets deleted keeps the item
  when evidence is missing, partial or ambiguous, and comes with a test that
  fails if it does not.
- **Tests defend behaviour**, not wiring: table-driven cases (`rstest`) and
  property tests (`proptest`), in the module's `tests.rs`.
- **Formatting.** The code is not formatted to rustfmt's defaults, so do not
  run `cargo fmt` over files you are not changing; match the style around you.
- **No real data.** Never commit API keys, tokens, hostnames or anything from
  your own library. The demo snapshot is made with
  `crates/flinch-web/demo/anonymize.py`, which renames titles and randomizes
  sizes, dates and ids.

By contributing, you agree that your work is licensed under the
[MIT license](LICENSE).
