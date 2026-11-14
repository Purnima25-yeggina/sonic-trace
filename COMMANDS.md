# sonic-trace — command guide

A friendly guide to every command and option. If you just want to get going, read the
**Quick start**; the rest is reference you can come back to.

> In the examples, `dump.tar.gz` is a SONiC support bundle (`sonic_dump_*.tar.gz` from
> `generate_dump`). Replace names like `Vlan100` or `10.1.1.0/24` with your own.

---

## Quick start

```bash
# 1. Check a whole support bundle: what passed, what failed
sonic-trace --snapshot dump.tar.gz check

# 2. Look at one object in detail
sonic-trace --snapshot dump.tar.gz route 10.1.1.0/24

# 3. On a switch, watch changes reach the chip as you configure (Ctrl+C to stop)
sudo sonic-trace live vlan
```

That's 90% of everyday use. Everything below is the detail.

---

## The one rule to remember

Options have a place:

- **Global options go *before* the command:**
  `sonic-trace --snapshot dump.tar.gz vlan Vlan100` ✅
- **A command's own options go *after* it:**
  `sonic-trace live --stuck-after 10 vlan` ✅

`sonic-trace vlan Vlan100 --snapshot dump.tar.gz` is rejected, because `--snapshot` is a
global option and came too late.

---

## Where the data comes from

sonic-trace never guesses — it reads real switch data. You tell it where from:

| You have… | Use |
|---|---|
| A support bundle on your laptop | `--snapshot dump.tar.gz` (no need to unzip it) |
| An unpacked bundle folder | `--snapshot sonic_dump_sw1/` |
| You're logged into the switch | `--live` (takes one snapshot now) or the `live` command (follows changes) |

Running offline with `--snapshot` on your own machine is the safest option — it never
touches the switch at all.

---

## 1. Commands

Pick one command per run. It says **what kind of thing** you want to look at.

| Command | What it does | What to give it |
|---|---|---|
| `vlan <NAME>` | Trace one VLAN | `Vlan100`, or `all` for every VLAN |
| `port <NAME>` | Trace one front-panel port | `Ethernet0`, or `all` |
| `lag <NAME>` | Trace one PortChannel | `PortChannel001`, or `all` |
| `route <PREFIX>` | Trace one route | `10.1.1.0/24`, `10.1.0.32` (host route), `Vrf-red:10.1.1.0/24`, `fe80::/64`, or `all` |
| `check` | Trace **everything** and summarise pass/fail | nothing |
| `live [TARGETS…]` | On a switch, **follow changes live** as you configure | kinds and/or names, or nothing for all |
| `help` | Show built-in help | — |

**Tracing one object** tells you, stage by stage, whether it made it from your
configuration all the way into the switching chip, and where it stopped if it didn't.

**`check`** runs that trace for every object in the dump and prints a short pass/fail
report — the fastest way to health-check a switch from a support bundle.

**`live`** is for a running switch: you configure in one terminal, and it prints each
change as it flows to the chip in another.

---

## 2. Global options (before the command)

| Option | Meaning | Default |
|---|---|---|
| `--snapshot <PATH>` | Read offline data: a `sonic_dump_*.tar.gz`, an unpacked bundle, its `dump/` folder, a folder of `<DB>.json` files, or one JSON file | — |
| `--live` | Read the databases from **this switch** right now (one snapshot, via `sonic-db-dump`) | — |
| `--db-dump-cmd <CMD>` | The command `--live` uses to read the databases | `sonic-db-dump` |
| `--asic <N>` | On a multi-ASIC switch, look at ASIC N | — |
| `--swss-rec <FILE>` | Point at orchagent's `swss.rec` log (plain or `.gz`) | found in the bundle; `/var/log/swss/swss.rec` for `live` |
| `--sairedis-rec <FILE>` | Point at the `sairedis.rec` log | found in the bundle; `/var/log/swss/sairedis.rec` for `live` |
| `--syslog <FILE>` | Point at syslog | found in the bundle; `/var/log/syslog` for `live` |
| `--max-input-mb <N>` | Most data to read, in MiB (raise it to read older log rotations) | `256` |
| `--json` | Print JSON instead of text, for scripts | — |
| `-h`, `--help` | Show help | — |
| `-V`, `--version` | Show the version | — |

You usually only need `--snapshot` (offline) or `--live` (on the switch). The log options
are for when the logs aren't where the tool expects.

---

## 3. Command options (after the command)

| Command | Option | Meaning | Default |
|---|---|---|---|
| `check` | `--details` | Also print the full trace of each **failed** object | off |
| `live` | `--stuck-after <SECONDS>` | Warn if a change hasn't reached the ASIC within this many seconds | `5` |
| `live` | `--redis-cli <CMD>` | How to reach Redis, if `redis-cli` isn't the right command | `redis-cli` |

---

## 4. Common tasks

### A. Health-check a support bundle (offline — any machine, nothing touched on the switch)

```bash
sonic-trace --snapshot dump.tar.gz check                   # full report
sonic-trace --snapshot dump.tar.gz check | tail -8         # just the summary table
sonic-trace --snapshot dump.tar.gz check | grep '^FAIL'    # only the failures
sonic-trace --snapshot dump.tar.gz check --details         # failures with full traces
sonic-trace --snapshot dump.tar.gz --json check            # machine-readable
```

### B. Investigate one object

```bash
sonic-trace --snapshot dump.tar.gz vlan Vlan100
sonic-trace --snapshot dump.tar.gz port Ethernet0
sonic-trace --snapshot dump.tar.gz lag PortChannel001
sonic-trace --snapshot dump.tar.gz route 10.1.1.0/24
sonic-trace --snapshot dump.tar.gz route 10.1.0.32          # host route; the /32 is optional
sonic-trace --snapshot dump.tar.gz route Vrf-red:20.0.0.0/24  # a route inside a VRF
```

### C. Full detail for every object of one kind

```bash
sonic-trace --snapshot dump.tar.gz vlan all
sonic-trace --snapshot dump.tar.gz route all | less         # long output; page through it
```

### D. Read more log history

If a "no record" line says older log rotations were skipped, raise the limit:

```bash
sonic-trace --snapshot dump.tar.gz --max-input-mb 2048 route 10.1.1.0/24
```

### E. Other ways to point at the data

```bash
sonic-trace --snapshot sonic_dump_sw1/ vlan Vlan100             # an unpacked bundle
sonic-trace --snapshot sonic_dump_sw1/dump/ vlan Vlan100        # just the dump folder
sonic-trace --snapshot my-dumps/ --syslog ./syslog vlan Vlan100 # your own files
sonic-trace --snapshot sonic_dump_sw1/ --asic 1 port Ethernet0  # a multi-ASIC switch
```

### F. One snapshot on a running switch

```bash
sudo sonic-trace --live vlan Vlan100
sudo sonic-trace --live check
```

> ⚠️ `--live` reads every database with Redis `KEYS`, which briefly pauses Redis while it
> runs — fine once (it's what `show techsupport` does), but **don't run it in a loop** on a
> busy switch. To watch changes over time, use the `live` command instead (task G), which
> doesn't do this. For zero impact, run `generate_dump` on the switch and analyse the
> bundle offline with `--snapshot`.

### G. Follow changes live on a switch

In one terminal you run `live`; in another you configure. Each change prints as it reaches
the chip, with the time it took. `live` runs until you press Ctrl+C.

```bash
sudo sonic-trace live                        # everything: VLANs, ports, LAGs, routes
sudo sonic-trace live vlan                   # only VLANs
sudo sonic-trace live vlan route             # VLANs and routes
sudo sonic-trace live Vlan100                # a single object
sudo sonic-trace live Ethernet4 10.1.1.0/24  # a few specific objects
sudo sonic-trace live --stuck-after 10 vlan  # warn after 10 s instead of 5
sudo sonic-trace live --redis-cli "redis-cli -s /var/run/redis/redis.sock"
```

### H. Use it in a script or CI

```bash
# Fail the job if anything is wrong
sonic-trace --snapshot dump.tar.gz check > /dev/null && echo "all good" || echo "problems found"

# Pull one field out with jq
sonic-trace --snapshot dump.tar.gz --json route 10.1.1.0/24 | jq '.diagnosis.stopped_at'
```

---

## 5. Exit codes

The exit code lets scripts react without reading the text.

| Code | Meaning |
|---|---|
| `0` | Healthy — every object passed |
| `1` | Something stopped at a stage, or wasn't found |
| `2` | Bad input — a wrong name, an unreadable file, or Redis unreachable |

---

## 6. What can't be combined

- `--snapshot` with `--live`, or `--snapshot` with the `live` command (pick offline **or** live)
- `--json` and `--asic` with the `live` command
- `--details` only applies to `check`

---

## Target formats in detail

What each command accepts, and what it rejects:

| Command | Give it | Accepted | Rejected |
|---|---|---|---|
| `vlan` | `Vlan` + a number 1–4094 | `Vlan8`, `Vlan100` | `Vlan0`, `Vlan4095`, `vlan8` (case matters), `8` |
| `port` | `Ethernet` + a number | `Ethernet0`, `Ethernet160` | `eth0`, `Ethernet` |
| `lag` | `PortChannel` + digits | `PortChannel001`, `PortChannel1` | `PortChannel`, `Po1` |
| `route` | `[<VRF>:]<address>[/<mask>]` | `10.0.0.16/16`, `fe80::/64`, `10.1.0.32`, `Vrf-red:20.0.0.0/24` | `10.0.0.0/33`, `1.2.3`, `Vrf:1.1.1.0/24` |

Routes, in a bit more detail:

- **Host routes** (`/32`, `/128`) can be written with or without the mask. SONiC stores
  them in APPL_DB without it (`ROUTE_TABLE:10.1.0.32`) and in ASIC_DB with it; the tool
  checks both. An address with no mask is treated as a host route.
- **VRF routes** start with the VRF name, exactly as APPL_DB writes it:
  `Vrf-red:20.0.0.0/24`. With no VRF, the default VRF is meant, and routes in other VRFs
  are not matched.
- The address is normalised (`FE80:0::1` → `fe80::1`), but host bits are kept:
  `192.168.0.4/24` is looked up as written, not as `192.168.0.0/24`.

## What a support bundle looks like

The easiest input is an unpacked `generate_dump` bundle — point `--snapshot` at its top
folder (or give it the `.tar.gz` directly):

```
sonic_dump_<host>_<date>/
  dump/
    CONFIG_DB.json      one file per database
    APPL_DB.json
    STATE_DB.json
    ASIC_DB.json
    APPL_DB.json.1      ASIC 1 on a multi-ASIC switch (use --asic 1)
  log/
    swss.rec.gz         logs are gzip-compressed; used for evidence
    swss.rec.1.gz ...   older rotations (.1 is newest) are read too
    sairedis.rec.gz
    syslog.gz
```

You can also point `--snapshot` at the `dump/` folder, or at any folder holding one dump
per database (lowercase names like `config_db.json` work too). Missing database files are
skipped, but at least one must exist.

## A note on running on a switch

- Running with `sudo` is only needed to read the log files under `/var/log/`.
- The tool is **read-only**: it reads the databases and logs and never changes your
  configuration.
- On a switch that isn't your own lab box, prefer the offline route — `generate_dump` then
  `--snapshot` on your machine — which has zero impact on the switch.
- Tip: if you copied the binary to a switch for testing, run it from `/tmp/sonic-trace`
  and delete it when you're done (`rm -f /tmp/sonic-trace`). Nothing else is installed.
