# sonic-trace

[![CI](https://github.com/Purnima25-yeggina/sonic-trace/actions/workflows/ci.yml/badge.svg)](https://github.com/Purnima25-yeggina/sonic-trace/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Rust 1.99](https://img.shields.io/badge/Rust-1.99-orange.svg)](rust-toolchain.toml)
[![PRs welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](CONTRIBUTING.md)

Find where a SONiC config gets stuck. `sonic-trace` follows one object across
CONFIG_DB, APPL_DB, STATE_DB and ASIC_DB and names the stage that dropped it.
Offline, read-only, written in Rust.

> **Status: early work in progress, tested on sample data only.** It has not yet been
> run on a dump from a real switch. See [TRY-IT.md](TRY-IT.md) to run it yourself.
> `vlan` (with member ports), `port`, `lag` and
> `route` are implemented. Not an official SONiC project.

## Why

A SONiC config change passes through several daemons, and each stage keeps its
own Redis database with its own naming. When something is missing in the ASIC,
finding the cause means querying each database by hand. `sonic-trace` does that
comparison and reports the first stage where the object is missing or the
databases disagree.

## Usage

```
sonic-trace [OPTIONS] <COMMAND> <TARGET>
```

For example:

```
sonic-trace --snapshot fixtures/community/vlan vlan Vlan8
            \__________________________________/ \__/ \____/
              where the data comes from (option)   |      the object to trace
                                                   what kind of object (command)
```

| Part | Meaning |
|---|---|
| `sonic-trace` | The program. |
| `--snapshot fixtures/community/vlan` | An **option**: read the data from this folder. A snapshot is a copy of the databases saved at one moment, so the tool never touches a switch. |
| `vlan` | The **command**: which kind of object to trace. |
| `Vlan8` | The **target**: the name of the one object to trace. |

**Options go before the command.** `sonic-trace vlan Vlan8 --snapshot dir` is rejected.

### Check a whole dump at once

`check` traces every VLAN, port, LAG and route in the snapshot and lists what failed,
then what passed, then a summary. It takes the `generate_dump` archive as is; there is
no need to unpack it.

```
sonic-trace --snapshot sonic_dump_sw1_20261111_120000.tar.gz check
sonic-trace --snapshot sonic_dump_sw1_20261111_120000.tar.gz check --details   # full trace of each failure
sonic-trace --snapshot sonic_dump_sw1_20261111_120000.tar.gz check | grep ^FAIL  # failures only
```

On the synthetic bundle in `fixtures/bundle`:

```
FAILED (4)
FAIL vlan  Vlan6                stopped at APPL_DB: configured and STATE_DB says ok, but APPL_DB has no entry: the databases disagree
FAIL vlan  Vlan8                stopped at ASIC_DB: in APPL_DB but not in ASIC_DB; orchagent has not programmed it (check swss.rec / syslog)
FAIL route 30.0.0.0/24          stopped at ASIC_DB: in APPL_DB but not in ASIC_DB; orchagent has not programmed it (check swss.rec / syslog)
FAIL route 40.0.0.0/24          stopped at ASIC_NEXTHOP: programmed, but the next hops differ from APPL_DB

PASSED (6)
PASS vlan  Vlan2                programmed through every stage
PASS vlan  Vlan3                programmed through every stage
PASS vlan  Vlan4                programmed through every stage
PASS vlan  Vlan7                programmed in ASIC_DB, but vlanmgrd has not reported state=ok in STATE_DB
PASS route 10.1.0.32/32         programmed through every stage
PASS route Vrf-red:20.0.0.0/24  programmed through every stage

SUMMARY
  type    total  passed  failed  error
  vlan        6       4       2      0
  port        0       0       0      0
  lag         0       0       0      0
  route       4       2       2      0
  all        10       6       4      0
=> some objects did not pass; trace one for details, e.g. sonic-trace --snapshot <bundle> route <prefix>
```

- To see the full trace of every object of one kind, give `all` as the target:
  `vlan all`, `port all`, `lag all` or `route all`. It ends with a count, e.g.
  `6 vlan objects: 4 passed, 2 failed, 0 could not be traced`.
- Objects come from the dump itself: `VLAN|`, `PORT|`, `PORTCHANNEL|` and `STATIC_ROUTE|`
  in CONFIG_DB, and `ROUTE_TABLE:` in APPL_DB. A route found under two names (a host
  route with and without its mask, or a static route and its APPL_DB entry) is traced once.
- A `PASS` line can still carry a note, as `Vlan7` does above: the object is programmed,
  but something along the way looks off.
- `ERR` lines are objects the tool found but could not trace, such as a key in a format it
  does not know. They are counted in the `error` column.
- Exit code: `0` when every object passed, `1` otherwise. `--json` gives one entry per
  object, with its full trace.
- A `.tar.gz` is read without unpacking it in place: only `dump/<DB>.json*` and the
  `swss.rec`, `sairedis.rec` and `syslog` logs are taken out, into a private temporary
  folder that is removed when the tool exits. Paths inside the archive are never used as
  output paths.

### Commands and what to give them

| Command | Target to give | Examples | Rejected |
|---|---|---|---|
| `vlan` | `Vlan` followed by a number from 1 to 4094 | `Vlan8`, `Vlan100` | `Vlan0`, `Vlan4095`, `vlan8` (case matters), `8` |
| `port` | `Ethernet` followed by a number | `Ethernet0`, `Ethernet160` | `eth0`, `Ethernet` |
| `lag` | `PortChannel` followed by digits | `PortChannel001`, `PortChannel1` | `PortChannel`, `Po1` |
| `check` | nothing; add `--details` for the full trace of each failure | `check`, `check --details` | |
| any of `vlan`, `port`, `lag`, `route` | `all`: every object of that kind, full trace each | `vlan all`, `route all` | |
| `route` | `[<VRF>:]<address>[/<mask>]` | `10.0.0.16/16`, `fe80::/64`, `10.1.0.32` (host route), `Vrf-red:20.0.0.0/24` | `10.0.0.0/33`, `1.2.3`, `Vrf:1.1.1.0/24` |

Route targets in more detail:
- **Host routes** (/32 and /128) can be given with or without the mask. SONiC writes them
  to APPL_DB without it (`ROUTE_TABLE:10.1.0.32`) and to ASIC_DB with it; the tool
  checks both forms. An address with no mask is treated as a host route.
- **VRF routes** start with the VRF name, as APPL_DB writes them: `Vrf-red:20.0.0.0/24`.
  Without a VRF, the default VRF is meant, and routes in other VRFs are not matched.
- The address is put in canonical form (`FE80:0::1` becomes `fe80::1`), but host bits
  are kept: `192.168.0.4/24` is looked up as written, not as `192.168.0.0/24`.

### Options

You must give **exactly one** way to get the data: `--snapshot` or `--live`.

| Option | What it does | Default |
|---|---|---|
| `--snapshot <PATH>` | Read saved databases. `PATH` is an unpacked `generate_dump` bundle, its `dump/` folder, any folder holding one dump per database, or a single JSON file with all four. | required unless `--live` |
| `--live` | Instead of files, run `sonic-db-dump` on this switch, once per database. Cannot be combined with `--snapshot`. | off |
| `--db-dump-cmd <CMD>` | The program `--live` runs. | `sonic-db-dump` |
| `--asic <N>` | For switches with several chips: use chip N. With `--snapshot`, it reads `<DB>.json.N`, the names `generate_dump` uses (an `asicN/` subfolder also works). With `--live`, it adds `--netns asicN`. | the host namespace |
| `--swss-rec <FILE>` | orchagent's `swss.rec` log, plain or `.gz`, for evidence lines. | found in the bundle's `log/` folder, if present |
| `--sairedis-rec <FILE>` | The `sairedis.rec` log, plain or `.gz`, for evidence lines. | found in the bundle's `log/` folder, if present |
| `--syslog <FILE>` | The syslog file, plain or `.gz`, for evidence lines. | found in the bundle's `log/` folder, if present |
| `--max-input-mb <N>` | Refuse to read more than N MiB in total, across all files. | 256 |
| `--json` | Print JSON instead of text. | text |
| `-h`, `--help` | Show help. `sonic-trace vlan --help` shows help for one command. | |
| `-V`, `--version` | Show the version. | |

### What a snapshot contains

The easiest input is an unpacked support bundle from `generate_dump`. Point `--snapshot` at
its top folder. The layout below is taken from the `generate_dump` script:

```
sonic_dump_<host>_<date>/
  dump/
    CONFIG_DB.json      one file per database, in sonic-db-dump's format
    APPL_DB.json
    STATE_DB.json
    ASIC_DB.json
    APPL_DB.json.1      the same for ASIC 1 on a multi-ASIC switch (--asic 1)
    ...
  log/
    swss.rec.gz         logs are gzip-compressed; used for evidence if present
    swss.rec.1.gz ...   older rotations (.1 is the newest); read too
    sairedis.rec.gz, sairedis.rec.N.gz
    syslog.gz, syslog.N.gz
```

You can also point it at the `dump/` folder, or at any folder of per-database files
(lowercase names such as `config_db.json` work too). Missing database files are skipped,
but at least one must exist. `fixtures/bundle/` is a synthetic example of this layout,
generated by `tools/gen-bundle-fixture.py`.

### What you get back

A report on the terminal, and an exit code.

| Exit code | Meaning |
|---|---|
| `0` | The object is present at every stage that applies. |
| `1` | The tool ran, and the object stopped at a stage or the databases disagree. Includes "not found anywhere". |
| `2` | Bad input: a wrong name for the command, a missing file, an over-size input, or invalid arguments. |

Stage lines show `present`, `MISSING`, or `n/a` (the stage doesn't apply to this object,
for example CONFIG_DB for a route learned from BGP).

### More examples

```
sonic-trace --snapshot sonic_dump_sw1_20261111_120000.tar.gz check
sonic-trace --snapshot sonic_dump_sw1_20261111_120000.tar.gz route 10.1.0.32
sonic-trace --snapshot sonic_dump_sw1_20261111/ port Ethernet160
sonic-trace --snapshot sonic_dump_sw1_20261111/ lag PortChannel001
sonic-trace --snapshot sonic_dump_sw1_20261111/ route 10.1.0.32
sonic-trace --snapshot sonic_dump_sw1_20261111/ route Vrf-red:20.0.0.0/24
sonic-trace --snapshot sonic_dump_sw1_20261111/ --json vlan Vlan8
sonic-trace --snapshot sonic_dump_sw1_20261111/ --asic 1 port Ethernet0
sonic-trace --snapshot my-dumps/ --syslog /var/log/syslog vlan Vlan8
sonic-trace --live route 10.1.1.0/24
```

## Example output

Run on the community test data in `fixtures/community/vlan`:

```
$ sonic-trace --snapshot fixtures/community/vlan vlan Vlan8
trace: Vlan8
  CONFIG_DB  present  VLAN|Vlan8
  APPL_DB    present  VLAN_TABLE:Vlan8
  STATE_DB   present  VLAN_TABLE|Vlan8 state=ok
  ASIC_DB    MISSING  SAI VLAN with VLAN_ID=8
=> in APPL_DB but not in ASIC_DB; orchagent has not programmed it (check swss.rec / syslog)
   evidence: members: CONFIG_DB 2, APPL_DB 2, STATE_DB 2, ASIC_DB 0
```

```
$ sonic-trace --snapshot fixtures/community/vlan vlan Vlan6
trace: Vlan6
  CONFIG_DB  present  VLAN|Vlan6
  APPL_DB    MISSING  VLAN_TABLE:Vlan6
  STATE_DB   present  VLAN_TABLE|Vlan6 state=ok
  ASIC_DB    present  ASIC_STATE:SAI_OBJECT_TYPE_VLAN:oid:0x2600000000061a
=> configured and STATE_DB says ok, but APPL_DB has no entry: the databases disagree
   evidence: members: CONFIG_DB 2, APPL_DB 0, STATE_DB 2, ASIC_DB 2
   evidence: member Ethernet32 is in CONFIG_DB but not in APPL_DB
   evidence: member Ethernet40 is in CONFIG_DB but not in APPL_DB
```

```
$ sonic-trace --snapshot fixtures/community/port port Ethernet160
trace: Ethernet160
  CONFIG_DB    present  PORT|Ethernet160
  APPL_DB      present  PORT_TABLE:Ethernet160
  STATE_DB     present  PORT_TABLE|Ethernet160 state=ok
  ASIC_HOSTIF  present  ASIC_STATE:SAI_OBJECT_TYPE_HOSTIF:oid:0xd000000000a49
  ASIC_PORT    MISSING  ASIC_STATE:SAI_OBJECT_TYPE_PORT:oid:0x10000000002e6
=> host interface points at oid:0x10000000002e6 but ASIC_DB has no SAI port object with that OID
```

```
$ sonic-trace --snapshot fixtures/community/route route 10.0.0.16/16
trace: 10.0.0.16/16
  CONFIG_DB    n/a      not a static route
  APPL_DB      present  ROUTE_TABLE:10.0.0.16/16
  ASIC_ROUTE   MISSING  SAI route entry for 10.0.0.16/16
  ASIC_NEXTHOP n/a      no ASIC route
=> in APPL_DB but not in ASIC_DB; orchagent has not programmed it (check swss.rec / syslog)
   evidence: route uses nexthop group testnhg
   evidence: no SAI next hop exists for 10.0.0.1; its neighbor may be unresolved (check ARP/NDP)
```

For ports, it also compares configured speed and admin state with the SAI port object.

For routes, it resolves the ASIC next hop (single next hop, ECMP group, router
interface or port) and compares the next-hop addresses with APPL_DB. A stage
that does not apply, such as CONFIG_DB for a BGP route, is shown as `n/a`.

For LAGs, the ASIC LAG object has no name in SAI, so it is identified by its member
ports (resolved through their host interfaces). An empty LAG cannot be identified and
is shown as `n/a`.

## Logs, multi-ASIC and live mode

```
sonic-trace --snapshot sonic_dump_sw1/ vlan Vlan8          # logs in log/ are used automatically
sonic-trace --snapshot my-dumps/ --swss-rec swss.rec --sairedis-rec sairedis.rec.gz \
            --syslog syslog vlan Vlan8
sonic-trace --snapshot sonic_dump_sw1/ --asic 1 port Ethernet0   # dump/<DB>.json.1
sonic-trace --live route 10.1.1.0/24                       # runs sonic-db-dump on the switch
```

- When an object stops at a stage, the tool adds evidence from the logs. In a bundle,
  `log/swss.rec.gz`, `log/sairedis.rec.gz` and `log/syslog.gz` are found and decompressed
  automatically, together with their rotations (`swss.rec.1.gz`, `swss.rec.2.gz`, ...),
  read oldest first. A file given with `--swss-rec` and the like is read on its own.
- Rotations can be large. The current files must fit in `--max-input-mb`; what is left is
  shared between the three logs' rotations, newest first. A "no record" line says what it
  checked, e.g. `(checked 3 files from 2000-01-01.10:00:00; 5 older rotations not read
  (size limit, raise --max-input-mb))`, so a missing record is never claimed for a period
  the tool did not read.
- Syslog lines match whole words only (`Vlan8` does not match `Vlan80`). rsyslog's `#012`
  newline escapes are flattened, long messages are shortened around the match, and a
  message repeated many times is shown once with a count (`seen 12 times`).
- `sairedis.rec` has two kinds of record: single calls (`c`, `r`, `s`) and bulk calls
  (`C`, `R`, `S`, many objects on one line). Routes are usually created in bulk; both
  kinds are read.
- `sairedis.rec` records a SAI call but, for single creates, not its result, so the
  evidence says a call was recorded, not that it worked. A missing record is weak
  evidence: recordings rotate and start at boot.
- `--live` runs `sonic-db-dump -n <DB>` for each database. The dumps are taken one after
  another, so the snapshot is not atomic; a change in progress can look like a mismatch.
  Rerun to confirm.

Exit codes: `0` programmed everywhere, `1` stopped at a stage, `2` input error.
Add `--json` for machine-readable output.

## Input

`--snapshot` takes a `generate_dump` bundle (see "What a snapshot contains"), a
directory of per-database dumps, or one JSON file keyed by database name. Both the `sonic-db-dump` format
(`{"type":"hash","value":{...}}`) and plain `{key: {field: value}}` are accepted.

## Build

```
cargo build --release
tools/check        # fmt, clippy and all tests, same as CI
```

The Rust toolchain is pinned in `rust-toolchain.toml`.

## Design

- **Safe on bad input.** The workspace denies `unsafe`, `unwrap`, `expect`, `panic!`,
  slice indexing and string slicing (see `Cargo.toml`). A truncated or odd dump
  gives an error and exit code 2.
- **Fuzzed.** `tools/fuzz` runs libFuzzer targets for the snapshot parser (with all
  traces) and the log parser. A first 45-second run per target, about 1.6 million and
  0.7 million executions, found no crashes. That is a smoke test, not a guarantee.
- **Bounded input.** It refuses to read more than 256 MiB in total (change it with
  `--max-input-mb`). Tests feed every truncation and thousands of corrupted copies of
  the fixtures through the parser and all traces, and nothing may panic.
- **Read-only and private.** It writes nothing, makes no network connections and
  sends no telemetry.
- **Pinned output.** Golden tests in `tests/golden/` fix the exact text, JSON and
  exit codes.
- **Split crates.** `trace-core` holds the report model and does no I/O,
  `trace-sonic` holds the SONiC knowledge, and `sonic-trace` is the CLI. This keeps
  SONiC details out of the core.

## Limits

- Only VLANs, ports, LAGs and routes so far.
- Routes in a VRF are matched to their virtual router through the VRF's interfaces
  (`INTF_TABLE` in APPL_DB and the router interfaces in ASIC_DB), because SAI objects
  carry no VRF names. If no interface of the VRF is found, the tool falls back to any
  non-default virtual router and says so. The default virtual router comes from syncd's
  `HIDDEN` and `RIDTOVID` entries.
- The "neighbor may be unresolved" hint is a heuristic, not a proven cause.
- A route on the ASIC through a next hop but with no APPL_DB entry is reported as a
  possible stale route; one through a router interface or the CPU port is treated as a
  normal connected or local route.
- Link-local IPv6 routes (`fe80::/10`) in APPL_DB are not programmed one by one:
  orchagent sends all of `fe80::/10` to the CPU with one route per VRF, and
  `route_check.py` skips them. The tool reports them as normal when that CPU route
  exists, and flags them when it is missing.
- ASIC member ports are compared by count, because ASIC_DB stores OIDs.
- Memory: the size limit bounds what is read, not what is held. In a test with a
  synthetic 300,000-route dump (80 MB of JSON), a lookup took about 1.7 seconds and
  about 970 MB of RAM, roughly 12 times the input. A dump at the default 256 MiB limit
  could need around 3 GB. Reducing this is on the roadmap.
- The key formats, the bundle layout and the log formats were taken from the SONiC
  source (sonic-swss, sonic-sairedis, sonic-utilities), not from a live switch. Test data
  is from sonic-utilities' unit tests plus synthetic bundles, not real captures.
- Multi-ASIC logs: the tool reads the bundle's single `log/` folder; per-ASIC log files
  have not been checked.

## Roadmap

Not done yet:
- More objects: ACL, neighbor, VXLAN.
- Testing against dumps from a real switch and from several vendors' SAIs.
- Lower memory use on large dumps.
- For ports, compare CONFIG_DB with APPL_DB too, so a lost change is pinned to the
  stage where it stopped.
- A plain-English `--explain` mode (OIDs and table names are hard to read).
- A consistent live snapshot (today the databases are dumped one after another).
- Longer fuzzing runs in CI.

Ideas, not commitments: a comparison of two dumps. (A CI gate that fails when an object
is stuck is now `check`, which exits 1 when anything did not pass.)

## Contributing

See `CONTRIBUTING.md`. Security reports go through `SECURITY.md`.

## License

Apache-2.0. See `LICENSE`; fixture provenance is in `NOTICE`.
