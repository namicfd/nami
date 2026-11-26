"""Summarise results/raw.tsv: median, min and max zone-cycles per second per configuration."""
import csv, statistics, sys
from collections import defaultdict

rows = list(csv.DictReader(open(sys.argv[1]), delimiter="\t"))
groups = defaultdict(list)
loads = []
for r in rows:
    groups[(int(r["size"]), r["code"], r["config"], r["variant"])].append(float(r["zcps"]))
    loads.append(float(r["load1"]))
print("| size | code | config | variant | runs | median (Mzc/s) | min | max |")
print("|---|---|---|---|---|---|---|---|")
for (size, code, config, variant), v in sorted(groups.items()):
    print(f"| {size}³ | {code} | {config} | {variant} | {len(v)} | {statistics.median(v)/1e6:.2f}"
          f" | {min(v)/1e6:.2f} | {max(v)/1e6:.2f} |")
print(f"\nload average (1 min) during runs: median {statistics.median(loads):.2f},"
      f" max {max(loads):.2f}")

# Ratios within a repetition: each nami run against the faster Athena++ build of the same
# repetition and size, which ran minutes apart under similar background load.
athena = defaultdict(dict)
for r in rows:
    if r["code"] == "athena":
        athena[(r["rep"], r["size"])][r["config"]] = float(r["zcps"])
ratios = defaultdict(list)
for r in rows:
    if r["code"] != "athena":
        base = max(athena[(r["rep"], r["size"])].values())
        ratios[(int(r["size"]), r["config"], r["variant"])].append(float(r["zcps"]) / base)
print("\n| size | config | variant | median ratio to Athena++ (faster build, same rep) | min | max |")
print("|---|---|---|---|---|---|")
for (size, config, variant), v in sorted(ratios.items()):
    print(f"| {size}³ | {config} | {variant} | {statistics.median(v):.2f}× | {min(v):.2f}× | {max(v):.2f}× |")
