# sonic-trace

Trace one SONiC object through CONFIG_DB, APPL_DB, STATE_DB and ASIC_DB and
report where it stopped. Read-only, offline-first. **Early work in progress.**

```
$ sonic-trace --snapshot fixtures/community/vlan vlan Vlan8
```

Status: only `vlan` (including member ports) is implemented. Input is a
directory of per-database dumps (as `generate_dump` writes them) or one JSON file.
Not an official SONiC project. See `NOTICE` for fixture provenance.
