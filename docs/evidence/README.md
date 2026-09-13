# Evidence archives

These ZIP files retain the raw output and notes for earlier measurements.
They are historical records, not current API documentation. Documentation
edits must not change their contents or silently replace an earlier run.

| Archive | Recorded comparison |
| --- | --- |
| [layout-2026-09-10.zip](layout-2026-09-10.zip) | Resting-order and risk-index storage changes |
| [admission-2026-09-11.zip](admission-2026-09-11.zip) | Event admission token comparison |
| [journal-status-2026-09-11.zip](journal-status-2026-09-11.zip) | Controlled journal persistence status |
| [cache-2026-09-12.zip](cache-2026-09-12.zip) | Level quantity totals, compact book indexes, and dense/sparse routing |
| [engine-2026-09-12.zip](engine-2026-09-12.zip) | Journaled engine compared with direct component composition |

Read each archive's notes for source revisions, binary identity, workload,
sample count, and host settings. Compare only equivalent workloads. Some
archives include exploratory results that were not the final candidate.

The [performance report](../PERFORMANCE.md) explains timing boundaries,
withdrawn results, allocation coverage, and desktop limitations. The
[layout report](../LAYOUT.md) records memory costs. These archives do not
establish dedicated Linux latency or full-service soak qualification.
