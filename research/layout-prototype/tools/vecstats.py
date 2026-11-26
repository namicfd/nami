import re, subprocess, sys
# usage: vecstats.py binary  -> per sweep/convert function: total insns, ymm insns, key packed/scalar ops
binary = sys.argv[1]
out = subprocess.run(["objdump", "-d", "--no-show-raw-insn", "-M", "intel", "-C", binary],
                     capture_output=True, text=True, check=True).stdout
fn = None; stats = {}
for line in out.splitlines():
    m = re.match(r"^[0-9a-f]+ <(.*)>:$", line)
    if m:
        name = m.group(1)
        fn = name if re.search(r"layout_prototype::(soa|aos|hybrid|kernels)::", name) else None
        if fn: stats[fn] = {}
        continue
    if fn and "\t" in line:
        ins = line.split("\t", 1)[1].strip()
        op = ins.split()[0] if ins else ""
        s = stats[fn]
        s["total"] = s.get("total", 0) + 1
        if "ymm" in ins: s["ymm"] = s.get("ymm", 0) + 1
        for key in ["sqrtsd", "vsqrtsd", "vsqrtpd", "vdivsd", "vdivpd", "vmulpd", "vmulsd", "vaddpd", "vaddsd", "vblendvpd", "call"]:
            if op == key:
                k = key + ("(ymm)" if "ymm" in ins and key.endswith("pd") else "")
                s[k] = s.get(k, 0) + 1
for fn, s in sorted(stats.items()):
    short = re.sub(r"layout_prototype::", "", fn)
    if s.get("total", 0) < 40: continue
    print(f"{short[:60]:60s} " + " ".join(f"{k}={v}" for k, v in sorted(s.items())))
