"""How 1.5xIQR filtering before p95 changes the reported tail.

Mirrors src/stats.rs (nearest-rank p95, Tukey fences at the (n+1)/4 and
3(n+1)/4 ranks). For each workload it draws batches of n trials, computes
p95 on the raw batch and on the IQR-filtered batch, and compares both with
the p95 of the whole population.

usage: python3 tools/p95_sim.py [SHELL]   (SHELL defaults to bash)
"""
import math, random, statistics as st, subprocess, sys, time

def p95(xs):
    s = sorted(xs); n = len(s)
    return s[min(n, max(1, -(-n * 95 // 100))) - 1]

def iqr_filter(xs):
    s = sorted(xs); n = len(s)
    if n < 4:
        return list(xs)
    q1 = s[max(1, (n + 1) // 4) - 1]; q3 = s[min(n, 3 * (n + 1) // 4) - 1]
    lo, hi = q1 - 1.5 * (q3 - q1), q3 + 1.5 * (q3 - q1)
    kept = [v for v in xs if lo <= v <= hi]
    return kept or list(xs)

def evaluate(name, population, n, batches, rnd):
    truth = p95(population); true_med = st.median(population)
    raw, filt, removed, med_raw, med_filt = [], [], 0, [], []
    for _ in range(batches):
        b = rnd.sample(population, n); f = iqr_filter(b)
        raw.append(p95(b)); filt.append(p95(f)); removed += len(b) - len(f)
        med_raw.append(st.median(b)); med_filt.append(st.median(f))
    def bias(v, t): return (st.mean(v) - t) / t * 100
    def rmse(v, t): return math.sqrt(st.mean([(x - t) ** 2 for x in v])) / t * 100
    below = sum(1 for x in filt if x < truth) / batches * 100
    below_raw = sum(1 for x in raw if x < truth) / batches * 100
    print(f"| {name} | {n} | {removed / batches:.2f} | {bias(raw, truth):+.1f} / {rmse(raw, truth):.1f} | "
          f"{bias(filt, truth):+.1f} / {rmse(filt, truth):.1f} | {below_raw:.0f} / {below:.0f} | "
          f"{bias(med_raw, true_med):+.2f} / {bias(med_filt, true_med):+.2f} |")

def main():
    rnd = random.Random(7)
    shell = sys.argv[1] if len(sys.argv) > 1 else "bash"
    pops = {}
    pops["symmetric (normal, sd 3%)"] = [rnd.gauss(100, 3) for _ in range(50000)]
    pops["skewed (lognormal, sigma 0.25)"] = [100 * rnd.lognormvariate(0, 0.25) for _ in range(50000)]
    pops["spiky (5% of trials 1.5-3x slower)"] = [
        rnd.gauss(100, 2) * (rnd.uniform(1.5, 3) if rnd.random() < 0.05 else 1) for _ in range(50000)]
    real = []
    for _ in range(3000):
        t = time.perf_counter(); subprocess.run([shell, "-c", "true"], stdin=subprocess.DEVNULL); real.append((time.perf_counter() - t) * 1e6)
    pops[f"measured: 3000 x `{shell} -c true`"] = real[50:]
    print("| Workload | n | Trials removed per batch | Raw p95: bias % / RMSE % | Filtered p95: bias % / RMSE % | Batches reporting below true p95: raw % / filtered % | Median bias %: raw / filtered |")
    print("|---|---|---|---|---|---|---|")
    for name, pop in pops.items():
        for n in (20, 50):
            evaluate(name, pop, n, 20000, rnd)

if __name__ == "__main__":
    main()
