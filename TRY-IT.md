# Try it yourself

A hands-on walkthrough. It takes about 15 minutes and needs only Rust and a terminal.
Everything uses the sample data in this repo. There is no real switch involved.

## 0. Set up

```
git clone https://github.com/Purnima25-yeggina/sonic-trace.git
cd sonic-trace
cargo build --release
alias st=./target/release/sonic-trace
mkdir -p ~/trace-lab && cp -r fixtures/community fixtures/logs ~/trace-lab/
```

You will edit the copy in `~/trace-lab`, not the files in the repo. Re-run the last
`cp` at any time to undo your edits.

## 1. Look at the raw data first

A SONiC setting passes through four databases. Here is one VLAN in each of them:

```
grep -A2 '"VLAN|Vlan3"'       ~/trace-lab/community/vlan/config_db.json
grep -A3 '"VLAN_TABLE:Vlan3"' ~/trace-lab/community/vlan/appl_db.json
grep -A1 '"VLAN_TABLE|Vlan3"' ~/trace-lab/community/vlan/state_db.json
grep -B1 '"SAI_VLAN_ATTR_VLAN_ID": "3"' ~/trace-lab/community/vlan/asic_db.json
```

Notice that the same VLAN has a different name in each file, and in the last one it has
no name at all, only a long ID. Following one object by hand is the problem this tool
removes.

## 2. Run the tool on four ready-made cases

```
st --snapshot ~/trace-lab/community/vlan vlan Vlan3;  echo "exit code: $?"
st --snapshot ~/trace-lab/community/vlan vlan Vlan8;  echo "exit code: $?"
st --snapshot ~/trace-lab/community/vlan vlan Vlan6;  echo "exit code: $?"
st --snapshot ~/trace-lab/community/vlan vlan Vlan99; echo "exit code: $?"
```

| VLAN | What you should see |
|---|---|
| Vlan3 | every stage `present`, exit code 0 |
| Vlan8 | in APPL_DB but not in ASIC_DB, exit code 1 |
| Vlan6 | the databases disagree, exit code 1 |
| Vlan99 | not configured in CONFIG_DB, exit code 1 |

The Vlan6 data comes from SONiC's own unit tests and is deliberately inconsistent. It
shows how the tool behaves, not a real failure.

## 3. Break something and watch the tool notice

**A. Remove a VLAN from the ASIC database**

```
python3 - <<'PY'
import json,os
p=os.path.expanduser("~/trace-lab/community/vlan/asic_db.json")
d=json.load(open(p))
d={k:v for k,v in d.items() if not (k.startswith("ASIC_STATE:SAI_OBJECT_TYPE_VLAN:") and v.get("SAI_VLAN_ATTR_VLAN_ID")=="3")}
json.dump(d,open(p,"w"),indent=2)
PY
st --snapshot ~/trace-lab/community/vlan vlan Vlan3
```

Vlan3 was healthy. Now the ASIC_DB line says `MISSING`.

**B. Make a port's config disagree with the chip**

```
python3 - <<'PY'
import json,os
p=os.path.expanduser("~/trace-lab/community/port/config_db.json")
d=json.load(open(p)); d["PORT|Ethernet176"]["speed"]="100000"
json.dump(d,open(p,"w"),indent=2)
PY
st --snapshot ~/trace-lab/community/port port Ethernet176
```

Every stage is `present`, but the tool reports `speed: CONFIG_DB 100000, ASIC_DB 25000`.

**C. Delete something a route depends on**

```
python3 - <<'PY'
import json,os
p=os.path.expanduser("~/trace-lab/community/route/asic_db.json")
d=json.load(open(p)); d.pop("ASIC_STATE:SAI_OBJECT_TYPE_NEXT_HOP:oid:0x40000000002e7")
json.dump(d,open(p,"w"),indent=2)
PY
st --snapshot ~/trace-lab/community/route route 10.212.0.0/16
```

The route is still `present`, but its next hop is `dangling`.

Undo your edits: `cp -r fixtures/community ~/trace-lab/`

## 4. Use it from a script

```
st --snapshot ~/trace-lab/community/vlan vlan Vlan8 > /dev/null; echo $?     # 1
st --snapshot ~/trace-lab/community/vlan vlan Ethernet0; echo $?              # 2
st --snapshot ~/trace-lab/community/vlan --json vlan Vlan8 | head -25
```

Exit code 0 means healthy, 1 means the object stopped at a stage, 2 means bad input.

## 5. Add log evidence

```
st --snapshot ~/trace-lab/community/vlan \
   --swss-rec ~/trace-lab/logs/swss.rec \
   --sairedis-rec ~/trace-lab/logs/sairedis.rec \
   --syslog ~/trace-lab/logs/syslog vlan Vlan8
```

Three extra `evidence:` lines appear. The log files are hand-written examples, so their
text is synthetic. The syslog file has errors for `Vlan8` and `Vlan80`; only the `Vlan8`
one is shown.

## 6. Routes, LAGs and bad input

```
st --snapshot ~/trace-lab/community/route route 192.168.0.4/24        # ECMP, 4 next hops
st --snapshot ~/trace-lab/community/route route 10.0.0.16/16          # never reached the ASIC
st --snapshot ~/trace-lab/community/portchannel lag PortChannel001    # a missing member
st --snapshot ~/trace-lab/community/vlan --max-input-mb 0 vlan Vlan3  # size limit
st --snapshot /nonexistent vlan Vlan3                                 # clean error
```

## What this does and does not show

It shows that the tool follows an object through the four databases and reports where it
stopped. It does not show that a real switch produces exactly this data. The sample data
is test data written by SONiC developers, and the failures in section 3 are ones you
created. Treat the diagnoses as leads to investigate, not proven causes.
