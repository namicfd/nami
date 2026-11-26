#!/usr/bin/env bash
# Benchmark campaign: every configuration and layout, and Athena++, interleaved within each
# repetition so that a burst of load from elsewhere spreads over all of them.
#
# usage: ./bench.sh <athena-default-binary> <athena-simd-binary> <athena-input-dir> [reps] [cpu]
# Writes results/raw.tsv: rep size code config variant cycles zone_cycles_per_second load1.
set -euo pipefail

athena_default=$1
athena_simd=$2
inputs=$3
reps=${4:-5}
cpu=${5:-8}
here=$(cd "$(dirname "$0")" && pwd)
out="$here/results/raw.tsv"
mkdir -p "$here/results"
printf 'rep\tsize\tcode\tconfig\tvariant\tcycles\tzcps\tload1\n' > "$out"

# Cycles per run, chosen so one run lasts a few seconds at each configuration's speed.
cycles() { # config size
    case "$1:$2" in
        A:64 | D:64) echo 10 ;;
        A:128 | D:128) echo 2 ;;
        *:64) echo 100 ;;
        *:128) echo 20 ;;
    esac
}

rust() { # rep size config variant
    local n
    n=$(cycles "$3" "$2")
    local line
    line=$(taskset -c "$cpu" "$here/bin/lp-$3" run "$4" "$2" "$n" | head -1)
    local zcps=${line#*zone_cycles_per_second=}
    zcps=${zcps%% *}
    printf '%s\t%s\tnami-proto\t%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" "$n" "$zcps" \
        "$(cut -d' ' -f1 /proc/loadavg)" >> "$out"
}

athena() { # rep size label binary
    local n=100
    [ "$2" = 128 ] && n=20
    local dir
    dir=$(mktemp -d)
    local line
    line=$(cd "$dir" && taskset -c "$cpu" "$4" -i "$inputs/lw$2.in" time/nlim=$n time/tlim=1000 \
        | grep 'zone-cycles/cpu_second')
    rm -rf "$dir"
    printf '%s\t%s\tathena\t%s\t-\t%s\t%s\t%s\n' "$1" "$2" "$3" "$n" "${line##*= }" \
        "$(cut -d' ' -f1 /proc/loadavg)" >> "$out"
}

for rep in $(seq 1 "$reps"); do
    for size in 64 128; do
        athena "$rep" "$size" g++ "$athena_default"
        athena "$rep" "$size" g++-simd "$athena_simd"
        for config in A B C; do
            for variant in soa aos hybrid4 hybrid8; do
                rust "$rep" "$size" "$config" "$variant"
            done
        done
        rust "$rep" "$size" CA soa
        echo "rep $rep size $size done" >&2
    done
done
