# sonic-trace

Find where a SONiC config gets stuck. `sonic-trace` follows one object across
CONFIG_DB, APPL_DB, STATE_DB and ASIC_DB and names the stage that dropped it.
Offline, read-only, written in Rust.

> **Status: early work in progress.** `vlan` (with member ports), `port` and
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
cargo test
```

## Limits

- Only VLANs, ports and routes so far.
- Routes: virtual routers (VRFs) are not distinguished; the first match is shown.
  The "neighbor may be unresolved" hint is a heuristic, not a proven cause.
- It compares databases; it does not read `swss.rec`, `sairedis.rec` or syslog yet.
- ASIC member ports are compared by count, because ASIC_DB stores OIDs.
- The key formats were checked against the SONiC source, not against a live
  switch. Test data is from sonic-utilities' unit tests, not real captures.

## License

Apache-2.0. See `LICENSE`; fixture provenance is in `NOTICE`.
