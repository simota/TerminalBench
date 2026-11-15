# What IQR filtering does to the reported p95

`run` drops trials outside 1.5×IQR before it computes median, p95 and
stddev. This checks, by simulation, how that changes the p95 a report
shows. `tools/p95_sim.py` mirrors the estimators in `src/stats.rs`, draws
20,000 batches of `n` trials from a population, and compares the p95 of
each batch with the p95 of the whole population.

```sh
python3 tools/p95_sim.py bash
```

## Result (2026-11-15, Linux x86_64)

Bias and RMSE are percentages of the population p95.

| Workload | n | Trials removed per batch | Raw p95: bias / RMSE | Filtered p95: bias / RMSE | Batches reporting below the true p95: raw / filtered | Median bias: raw / filtered |
|---|---|---|---|---|---|---|
| symmetric (normal, sd 3%) | 20 | 0.37 | -0.7 / 1.3 | -0.7 / 1.3 | 74% / 74% | -0.01 / -0.02 |
| symmetric (normal, sd 3%) | 50 | 0.42 | -0.0 / 0.8 | -0.2 / 0.8 | 54% / 59% | -0.00 / -0.00 |
| skewed (lognormal, sigma 0.25) | 20 | 0.59 | -5.3 / 11.1 | -7.3 / 11.7 | 74% / 80% | +0.22 / -0.44 |
| skewed (lognormal, sigma 0.25) | 50 | 0.93 | -0.1 / 7.5 | -4.3 / 8.1 | 54% / 75% | +0.13 / -0.38 |
| spiky (5% of trials 1.5-3x slower) | 20 | 1.27 | +22.6 / 52.2 | -3.6 / 4.1 | 73% / 100% | +0.01 / -0.13 |
| spiky (5% of trials 1.5-3x slower) | 50 | 2.76 | +40.2 / 65.2 | -3.7 / 3.7 | 54% / 100% | +0.00 / -0.13 |
| measured: 3000 × `bash -c true` | 20 | 0.83 | -3.3 / 8.3 | -5.9 / 9.0 | 73% / 82% | +0.51 / -0.06 |
| measured: 3000 × `bash -c true` | 50 | 1.41 | +0.3 / 5.6 | -4.1 / 6.4 | 54% / 82% | +0.23 / -0.22 |

## Reading it

- **Filtering biases p95 low, and more trials do not fix it.** On the
  measured timings the raw p95 is unbiased at n=50 (+0.3%), the filtered
  one stays 4.1% low. On skewed data it is -0.1% against -4.3%.
- **Slow trials that are part of the workload disappear.** When 5% of
  trials are 1.5 to 3 times slower, the filtered p95 is below the true p95
  in every batch. Those trials only show up in `outliers_removed`.
- **At the default n=20 the p95 is weak either way.** Nearest-rank p95 of
  20 values is the second largest, and it lands below the population p95
  in about three of four batches even without filtering.
- **Raw p95 is not a free fix.** In the spiky workload the population p95
  sits at the edge of the spike mass, so the raw estimate jumps between
  the fast and the slow mode (RMSE above 50%).
- **The median barely moves.** Filtering shifts it by under half a percent
  in every case.

## Decision

p95 is computed from every timed trial; median and stddev keep the filter.
It is never reported below the median, which Reporting Spec v1 requires.
For metrics where higher is better the same rule applies to the derived
values, so p95 there is the fast tail.
