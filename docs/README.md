# Documentation

The workspace is version 0.19.0. The libraries implement matching, risk,
sessions, journaling, recovery, events, and routing. Their integration and
production qualification are still in progress.

## Start here

| Document | What it covers |
| --- | --- |
| [Architecture](ARCHITECTURE.md) | Entry points, ownership, data layout, and execution order |
| [Engine API](ENGINE.md) | Builder, admission, queue pressure, shutdown, and restart |
| [Protocol](PROTOCOL.md) | Message layouts, order policies, sequencing, and ownership |
| [Operations](OPERATIONS.md) | Persistence progress, failure handling, and recovery limits |

Use [Quick Start](../QUICKSTART.md) for build and test commands. The
[homepage diagrams](../README.md#workflow-diagrams) provide a short overview.

## Evidence and development

| Document | What it covers |
| --- | --- |
| [Performance](PERFORMANCE.md) | Workloads, measurement boundaries, results, and known benchmark gaps |
| [Layout](LAYOUT.md) | Storage changes and their performance tradeoffs |
| [Soak](SOAK.md) | Seeded fault workloads, capture commands, and qualification status |
| [Safety](SAFETY.md) | Unsafe boundaries, invariants, and configured verification |
| [Review](REVIEW.md) | Review order, evidence to inspect, and open review work |
| [Roadmap](ROADMAP.md) | Implemented milestones and remaining release criteria |
| [Engineering lessons](LEARNINGS.md) | Implementation decisions and mistakes found by tests |
| [Diagram sources](diagrams/README.md) | Editable Mermaid files and the rendering command |
| [Evidence archives](evidence/README.md) | Raw benchmark bundles and provenance |

No dedicated Linux latency result or completed multi-hour qualification is
published. Events acknowledge application, not durable storage. The routed
matching path and journaled engine are separate entry points.
