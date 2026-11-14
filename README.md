# sonic-trace

[![CI](https://github.com/Purnima25-yeggina/sonic-trace/actions/workflows/ci.yml/badge.svg)](https://github.com/Purnima25-yeggina/sonic-trace/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Rust 1.99](https://img.shields.io/badge/Rust-1.99-orange.svg)](rust-toolchain.toml)
[![PRs welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](CONTRIBUTING.md)

`sonic-trace` is a command-line tool for debugging [SONiC](https://sonicfoundation.dev/)
switches. On a SONiC switch, a config change — a VLAN, a port, a route — travels through
four internal databases before it reaches the switching chip: CONFIG_DB → APPL_DB →
STATE_DB → ASIC_DB. When something is missing in the chip, finding out which stage dropped
it normally means querying each database and reading daemon logs by hand.

sonic-trace does that automatically. It follows one object through all four stages, names
the stage that dropped it and the component responsible, and backs it up with evidence
from `swss.rec`, `sairedis.rec` and syslog. It reads a support bundle offline, or follows
changes live on a running switch. It is **read-only** — it never changes your config.

> **Status: work in progress (0.1.0).** Commands and output may still change. Validated on
> real support bundles and a live switch. See [COMMANDS.md](COMMANDS.md) for the full
> command guide and [TRY-IT.md](TRY-IT.md) for a hands-on walkthrough.

## What sonic-trace focuses on

- **Diagnosing the pipeline.** It reports the first stage where an object is missing or the
  databases disagree, and names the daemon responsible — the slow part of the hunt, done
  for you.
- **Working from what you already have.** It reads a `generate_dump` support bundle
  (`.tar.gz`) directly, so you can triage offline on your laptop with zero switch impact.
- **Safe on bad input.** A truncated or hostile dump should never crash it, hang it, or
  grow its memory without limit. Input is bounded and every parser is fuzzed.
- **Read-only and private.** It writes nothing to the switch, makes no network connections
  of its own, and sends no telemetry.

## Features

**Input**

- Read a `generate_dump` bundle as a `.tar.gz`, an unpacked folder, its `dump/` folder, a
  folder of per-database `*.json` files, or a single JSON file.
- Read the databases live from a switch with `--live` (one snapshot) or `live` (follow
  changes).
- Use evidence from `swss.rec`, `sairedis.rec` and syslog — gzipped and rotated logs
  included.

**What it traces**

- VLANs (with their member ports), front-panel ports, PortChannels (LAGs), and routes —
  including host routes, VRF routes, ECMP groups and link-local routes.

**Checking a whole switch**

- `check` traces every object in a bundle and prints a pass/fail summary with an exit code,
  so it fits in a script or CI. `vlan all` (and `port`/`lag`/`route all`) print the full
  trace of every object of one kind.

**Live mode**

- `live` follows changes on a running switch from your keystroke to the chip, with the time
  each change took and a warning when one gets stuck. Filter by kind (`live vlan`) or by
  name (`live Vlan100`).

**Output**

- Human-readable text, or `--json` for scripts. Exit codes: `0` healthy, `1` something
  stopped, `2` bad input.

## Getting started

### Prerequisites

- Rust, installed through [rustup](https://rustup.rs/). The first build installs the pinned
  toolchain (`rust-toolchain.toml`).
- A C linker: `build-essential` on Debian/Ubuntu, Xcode Command Line Tools on macOS.

No special permissions are needed to read a bundle. Reading a switch live (`--live` /
`live`) needs `sudo`, to reach Redis and the logs under `/var/log/`.

### Install

```bash
git clone https://github.com/Purnima25-yeggina/sonic-trace.git
cd sonic-trace
cargo build --release
```

The binary is `target/release/sonic-trace`. To run it on a switch, build a static binary
and copy it over:

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
scp target/x86_64-unknown-linux-musl/release/sonic-trace admin@<switch>:/tmp/
```

## Usage

```bash
# Health-check a whole support bundle: what passed, what failed
sonic-trace --snapshot dump.tar.gz check

# Trace one object in detail
sonic-trace --snapshot dump.tar.gz route 10.1.1.0/24
sonic-trace --snapshot dump.tar.gz vlan Vlan100

# Only the failures, or machine-readable output
sonic-trace --snapshot dump.tar.gz check | grep '^FAIL'
sonic-trace --snapshot dump.tar.gz --json route 10.1.1.0/24

# On a switch: one live snapshot, or follow changes as you configure (Ctrl+C to stop)
sudo sonic-trace --live check
sudo sonic-trace live vlan
```

**[COMMANDS.md](COMMANDS.md) is the full guide** — every command and option, with a quick
start and copy-paste examples. Or run `sonic-trace --help`.

## Example output

A VLAN that reached APPL_DB but was never programmed into the chip:

```
$ sonic-trace --snapshot fixtures/community/vlan vlan Vlan8
trace: Vlan8
  CONFIG_DB    present  VLAN|Vlan8
  APPL_DB      present  VLAN_TABLE:Vlan8
  STATE_DB     present  VLAN_TABLE|Vlan8 state=ok
  ASIC_DB      MISSING  SAI VLAN with VLAN_ID=8
=> in APPL_DB but not in ASIC_DB; orchagent has not programmed it (check swss.rec / syslog)
   evidence: members: CONFIG_DB 2, APPL_DB 2, STATE_DB 2, ASIC_DB 0
```

A whole-bundle check:

```
$ sonic-trace --snapshot dump.tar.gz check
...
SUMMARY
  type    total  passed  failed  error
  vlan        6       4       2      0
  route       4       2       2      0
  all        10       6       4      0
=> some objects did not pass; trace one for details
```

For ports it compares configured speed and admin state with the SAI port object; for
routes it resolves the ASIC next hop (single, ECMP group, router interface or port) and
compares against APPL_DB; for LAGs, which SAI gives no name, it identifies the ASIC object
by its member ports. A stage that does not apply (e.g. CONFIG_DB for a BGP route) shows as
`n/a`.

## Following changes live

On a switch, `live` prints each change as it flows from config to the chip. Run it in one
terminal and configure in another:

```
$ sudo sonic-trace live vlan
following all vlans — CONFIG_DB, APPL_DB, STATE_DB, ASIC_DB, swss.rec, sairedis.rec, syslog (times UTC; Ctrl+C to stop)
05:11:13.411  vlan  Vlan999   CONFIG_DB    set
05:11:13.423  vlan  Vlan999   APPL_DB      queued
05:11:13.423  vlan  Vlan999   STATE_DB     set
05:11:13.434  vlan  Vlan999   swss.rec     orchagent received SET
05:11:13.434  vlan  Vlan999   APPL_DB      set
05:11:13.436  vlan  Vlan999   ASIC_DB      set   ✅ reached the ASIC 25 ms after the CONFIG_DB change
```

It reads Redis keyspace notifications for the four databases and tails the three logs. A
CONFIG_DB or APPL_DB change starts a timer that the first ASIC_DB change stops; if nothing
reaches the chip within `--stuck-after` seconds, it prints a `⚠` warning (and says so if a
SAI call was recorded but ASIC_DB never changed). syslog errors naming a followed object
are shown too. It needs `redis-cli` (present on SONiC switches) and `sudo`.

`live` differs from `--live`: `--live` takes one snapshot and traces an object; `live`
keeps following until Ctrl+C. See [COMMANDS.md](COMMANDS.md) for both.

## Notable techniques

- **Lints that rule out panics.** The workspace forbids `unsafe` and denies `unwrap`,
  `expect`, `panic!`, slice indexing and string slicing (`Cargo.toml`). Parsers read bytes
  through checked accessors, so a short or malformed dump becomes a decode error (exit 2),
  not a crash.
- **Fuzzed parsers.** libFuzzer targets exercise the snapshot parser (with every trace) and
  the log and live-notification parsers; hundreds of thousands of executions per run, no
  crashes found. A smoke test, not a guarantee.
- **Bounded input.** It reads at most 256 MiB in total (`--max-input-mb`). Tests feed every
  truncation and thousands of corrupted copies of the fixtures through the parser, and
  nothing may panic.
- **Formats from the source, not guesswork.** Key formats, the bundle layout and the log
  formats come from the SONiC source (sonic-swss, sonic-sairedis, sonic-utilities).
- **Test data generated from code.** No real captures are checked in. Fixtures are
  synthetic or from sonic-utilities' own tests; golden tests in `tests/golden/` pin the
  exact text, JSON and exit codes.
- **Split crates.** `core` holds the report model and does no I/O, `sonic` holds the SONiC
  knowledge, and `cli` is the program — keeping SONiC details out of the core.

## Project structure

```
sonic-trace/
├── crates/                     three Rust crates, smallest dependency first
│   ├── core/                   report model and pipeline types — no I/O, no SONiC knowledge
│   │   └── src/lib.rs
│   ├── sonic/                  the SONiC knowledge: parsing and per-object traces
│   │   ├── src/
│   │   │   ├── snapshot.rs      load databases from a bundle, folder, .tar.gz or live switch
│   │   │   ├── archive.rs       read a generate_dump .tar.gz without unpacking it
│   │   │   ├── logs.rs          evidence from swss.rec / sairedis.rec / syslog (gzip, rotated)
│   │   │   ├── vlan.rs port.rs lag.rs route.rs   the four object traces
│   │   │   ├── check.rs         trace everything and summarise pass/fail
│   │   │   └── live/            follow changes on a running switch
│   │   │       ├── mod.rs        the tracker (pure: classify changes, time them)
│   │   │       └── io.rs         redis-cli notifications and log followers
│   │   └── tests/              per-object and real-format regression tests
│   └── cli/                    the sonic-trace command-line program
│       ├── src/main.rs
│       └── tests/              end-to-end tests (golden output, check, live)
├── fixtures/                   synthetic sample data (safe to run on; no real switch data)
│   ├── bundle/                 a realistic generate_dump bundle (dump/ + gzipped logs)
│   ├── community/              per-object dumps from SONiC's own tests
│   └── multi-asic/             a two-ASIC example
├── tests/golden/               expected text / JSON / exit codes, pinned
├── fuzz/                       libFuzzer targets and seed corpus
├── tools/                      helper scripts (check, fuzz, gen-bundle-fixture.py)
├── .github/workflows/ci.yml    build, test, clippy, fmt on every push
├── COMMANDS.md                 friendly command guide
├── TRY-IT.md                   hands-on walkthrough
├── CONTRIBUTING.md  SECURITY.md  LICENSE  NOTICE
├── README.md
├── Cargo.toml  Cargo.lock      workspace manifest and lockfile
└── rust-toolchain.toml         pinned Rust version
```

## Build and test

```bash
cargo build --release
tools/check        # fmt, clippy and all tests — the same as CI
```

## Limits

- Only VLANs, ports, LAGs and routes so far (ACL, neighbor and VXLAN are on the roadmap).
- Several diagnoses are heuristics, not proven causes — e.g. "neighbor may be unresolved".
- ASIC member ports are compared by count, because ASIC_DB stores OIDs, not names.
- Memory use is roughly 12× the input size (a large dump can need gigabytes); reducing this
  is on the roadmap.
- The log and key formats come from the SONiC source; multi-ASIC per-chip logs and
  orchagent's ZMQ mode are not covered yet.

## Roadmap

- More object types: ACL, neighbor, VXLAN.
- Validation across more platforms and vendor SAIs.
- A plain-English `--explain` mode (OIDs and table names are hard to read).
- Lower memory use on large dumps.

Tested so far on real support bundles and a live switch. Contributions welcome — a
comparison of two dumps is one open idea.

## Contributing

Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md). Security reports go
through [SECURITY.md](SECURITY.md), not public issues.

## License

Apache-2.0. See [LICENSE](LICENSE); fixture provenance is in [NOTICE](NOTICE).
