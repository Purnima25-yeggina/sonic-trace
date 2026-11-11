# sonic-trace

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

## Example

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
sonic-trace --snapshot bundle/ --swss-rec swss.rec --sairedis-rec sairedis.rec \
            --syslog syslog vlan Vlan8
sonic-trace --snapshot bundle/ --asic 1 port Ethernet0     # bundle/asic1/...
sonic-trace --live route 10.1.1.0/24                       # runs sonic-db-dump on the switch
```

- When an object stops at a stage, the tool adds evidence from the logs. A file named
  `swss.rec`, `sairedis.rec` or `syslog` inside the snapshot directory is used
  automatically. Syslog lines match whole words only (`Vlan8` does not match `Vlan80`).
- `sairedis.rec` records a SAI call but, for single creates, not its result, so the
  evidence says a call was recorded, not that it worked. A missing record is weak
  evidence: recordings rotate and start at boot.
- `--live` runs `sonic-db-dump -n <DB>` for each database. The dumps are taken one after
  another, so the snapshot is not atomic; a change in progress can look like a mismatch.
  Rerun to confirm.

Exit codes: `0` programmed everywhere, `1` stopped at a stage, `2` input error.
Add `--json` for machine-readable output.

## Input

`--snapshot` takes either a directory of per-database dumps (`CONFIG_DB.json`,
`APPL_DB.json`, `STATE_DB.json`, `ASIC_DB.json`, as `generate_dump` writes
them) or one JSON file keyed by database name. Both the `sonic-db-dump` format
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
- Routes: virtual routers (VRFs) are not distinguished; the first match is shown.
  The "neighbor may be unresolved" hint is a heuristic, not a proven cause.
- It compares databases; it does not read `swss.rec`, `sairedis.rec` or syslog yet.
- ASIC member ports are compared by count, because ASIC_DB stores OIDs.
- Memory: the size limit bounds what is read, not what is held. In a test with a
  synthetic 300,000-route dump (80 MB of JSON), a lookup took about 1.7 seconds and
  about 970 MB of RAM, roughly 12 times the input. A dump at the default 256 MiB limit
  could need around 3 GB. Reducing this is on the roadmap.
- The key formats were checked against the SONiC source, not against a live
  switch. Test data is from sonic-utilities' unit tests, not real captures.

## Roadmap

Not done yet:
- Per-VRF routes (the first match is shown today).
- More objects: ACL, neighbor, VXLAN.
- Testing against dumps from a real switch and from several vendors' SAIs.
- Lower memory use on large dumps.
- For ports, compare CONFIG_DB with APPL_DB too, so a lost change is pinned to the
  stage where it stopped.
- A plain-English `--explain` mode (OIDs and table names are hard to read).
- A consistent live snapshot (today the databases are dumped one after another).
- Longer fuzzing runs in CI.

Ideas, not commitments: a comparison of two dumps, and a CI mode that fails when an
object is stuck.

## Contributing

See `CONTRIBUTING.md`. Security reports go through `SECURITY.md`.

## License

Apache-2.0. See `LICENSE`; fixture provenance is in `NOTICE`.
