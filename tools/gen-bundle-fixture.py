#!/usr/bin/env python3
"""Generate fixtures/bundle: a support bundle laid out the way generate_dump writes one.

Layout (from sonic-utilities scripts/generate_dump):
  dump/<DB>.json     one per database, sonic-db-dump format {"key":{"type":"hash","value":{..}}}
  dump/<DB>.json.N   the same for ASIC N on a multi-ASIC switch
  log/<name>.gz      logs are stored gzip-compressed, by base name (swss.rec.gz, ...)

Content is synthetic, built from the formats in the SONiC source, not captured from a
switch. Run from the repository root: python3 tools/gen-bundle-fixture.py
"""
import gzip, json, os, shutil

ROOT = "fixtures/bundle"
COMMUNITY = "fixtures/community"
DEFAULT_VR, RED_VR = "oid:0x3000000000002", "oid:0x3000000000099"
SW = "oid:0x21000000000000"


def wrap(db):
    return {k: {"type": "hash", "value": v} for k, v in db.items()}


def load(kind, name):
    p = f"{COMMUNITY}/{kind}/{name}.json"
    return json.load(open(p)) if os.path.exists(p) else {}


def route_key(dest, vr):
    return "ASIC_STATE:SAI_OBJECT_TYPE_ROUTE_ENTRY:" + json.dumps(
        {"dest": dest, "switch_id": SW, "vr": vr}, separators=(",", ":"))


def write(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        json.dump(wrap(data), f, indent=1, sort_keys=True)


def write_gz(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as raw:
        with gzip.GzipFile(fileobj=raw, mode="wb", mtime=0, filename="") as f:
            f.write(text.encode())


shutil.rmtree(ROOT, ignore_errors=True)

# Default namespace: the community VLAN data plus routes that exercise real key formats.
config = load("vlan", "config_db")
appl = load("vlan", "appl_db")
state = load("vlan", "state_db")
asic = load("vlan", "asic_db")

appl.update({
    # fpmsyncd writes host routes without the mask.
    "ROUTE_TABLE:10.1.0.32": {"nexthop": "10.0.0.1", "ifname": "Ethernet0"},
    # VRF routes carry the VRF name; the VRF is bound to Ethernet4.
    "ROUTE_TABLE:Vrf-red:20.0.0.0/24": {"nexthop": "10.0.0.9", "ifname": "Ethernet4"},
    "INTF_TABLE:Ethernet0": {},
    "INTF_TABLE:Ethernet4": {"vrf_name": "Vrf-red"},
    # Stuck: in APPL_DB, never programmed.
    "ROUTE_TABLE:30.0.0.0/24": {"nexthop": "10.0.0.1", "ifname": "Ethernet0"},
    # Programmed through the wrong next hop.
    "ROUTE_TABLE:40.0.0.0/24": {"nexthop": "10.0.0.1", "ifname": "Ethernet0"},
})
asic.update({
    # syncd keeps the default virtual router as a real ID under HIDDEN; RIDTOVID maps it.
    "HIDDEN": {"SAI_SWITCH_ATTR_DEFAULT_VIRTUAL_ROUTER_ID": "oid:0x6000000000abc"},
    "RIDTOVID": {"oid:0x6000000000abc": DEFAULT_VR},
    "ASIC_STATE:SAI_OBJECT_TYPE_VIRTUAL_ROUTER:" + DEFAULT_VR: {"NULL": "NULL"},
    "ASIC_STATE:SAI_OBJECT_TYPE_VIRTUAL_ROUTER:" + RED_VR: {"SAI_VIRTUAL_ROUTER_ATTR_ADMIN_V4_STATE": "true"},
    "ASIC_STATE:SAI_OBJECT_TYPE_HOSTIF:oid:0xd0000000000f0": {"SAI_HOSTIF_ATTR_NAME": "Ethernet0", "SAI_HOSTIF_ATTR_OBJ_ID": "oid:0x1000000000010"},
    "ASIC_STATE:SAI_OBJECT_TYPE_HOSTIF:oid:0xd0000000000f4": {"SAI_HOSTIF_ATTR_NAME": "Ethernet4", "SAI_HOSTIF_ATTR_OBJ_ID": "oid:0x1000000000014"},
    "ASIC_STATE:SAI_OBJECT_TYPE_ROUTER_INTERFACE:oid:0x60000000000a0": {"SAI_ROUTER_INTERFACE_ATTR_PORT_ID": "oid:0x1000000000010", "SAI_ROUTER_INTERFACE_ATTR_VIRTUAL_ROUTER_ID": DEFAULT_VR},
    "ASIC_STATE:SAI_OBJECT_TYPE_ROUTER_INTERFACE:oid:0x60000000000a4": {"SAI_ROUTER_INTERFACE_ATTR_PORT_ID": "oid:0x1000000000014", "SAI_ROUTER_INTERFACE_ATTR_VIRTUAL_ROUTER_ID": RED_VR},
    "ASIC_STATE:SAI_OBJECT_TYPE_NEXT_HOP:oid:0x4000000000001": {"SAI_NEXT_HOP_ATTR_IP": "10.0.0.1", "SAI_NEXT_HOP_ATTR_ROUTER_INTERFACE_ID": "oid:0x60000000000a0", "SAI_NEXT_HOP_ATTR_TYPE": "SAI_NEXT_HOP_TYPE_IP"},
    "ASIC_STATE:SAI_OBJECT_TYPE_NEXT_HOP:oid:0x4000000000009": {"SAI_NEXT_HOP_ATTR_IP": "10.0.0.9", "SAI_NEXT_HOP_ATTR_ROUTER_INTERFACE_ID": "oid:0x60000000000a4", "SAI_NEXT_HOP_ATTR_TYPE": "SAI_NEXT_HOP_TYPE_IP"},
    route_key("10.1.0.32/32", DEFAULT_VR): {"SAI_ROUTE_ENTRY_ATTR_NEXT_HOP_ID": "oid:0x4000000000001"},
    route_key("20.0.0.0/24", RED_VR): {"SAI_ROUTE_ENTRY_ATTR_NEXT_HOP_ID": "oid:0x4000000000009"},
    route_key("40.0.0.0/24", DEFAULT_VR): {"SAI_ROUTE_ENTRY_ATTR_NEXT_HOP_ID": "oid:0x4000000000009"},
})
for name, db in [("CONFIG_DB", config), ("APPL_DB", appl), ("STATE_DB", state), ("ASIC_DB", asic)]:
    write(f"{ROOT}/dump/{name}.json", db)

# ASIC 1 of a multi-ASIC switch: the community route data, as <DB>.json.1.
for name in ["config_db", "appl_db", "asic_db"]:
    write(f"{ROOT}/dump/{name.upper()}.json.1", load("route", name))

# Logs, gzip-compressed as generate_dump stores them. routeorch uses the bulk API, so
# route creates appear as one "C" line with "||"-separated entries (sairedis Recorder).
bulk = "|".join([
    "2026-11-11.10:00:04.000000", "C", "SAI_OBJECT_TYPE_ROUTE_ENTRY",
]) + "".join(
    "||" + json.dumps({"dest": d, "switch_id": SW, "vr": vr}, separators=(",", ":"))
    + "|SAI_ROUTE_ENTRY_ATTR_NEXT_HOP_ID=" + nh
    for d, vr, nh in [
        ("10.1.0.32/32", DEFAULT_VR, "oid:0x4000000000001"),
        ("20.0.0.0/24", RED_VR, "oid:0x4000000000009"),
        ("40.0.0.0/24", DEFAULT_VR, "oid:0x4000000000009"),
    ])
write_gz(f"{ROOT}/log/sairedis.rec.gz", open("fixtures/logs/sairedis.rec").read() + bulk + "\n")
write_gz(f"{ROOT}/log/swss.rec.gz", open("fixtures/logs/swss.rec").read()
         + "2026-11-11.10:00:05.000100|ROUTE_TABLE:30.0.0.0/24|SET|nexthop:10.0.0.1|ifname:Ethernet0\n"
         + "2026-11-11.10:00:05.000200|ROUTE_TABLE:40.0.0.0/24|SET|nexthop:10.0.0.1|ifname:Ethernet0\n")
write_gz(f"{ROOT}/log/syslog.gz", open("fixtures/logs/syslog").read())
print("wrote", ROOT)
