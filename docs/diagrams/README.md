# Diagrams

The homepage shows a compact overview. Expand its detailed sections to inspect
queue ownership, transaction order, or recovery. Images link to full-size SVGs.

| Diagram | Source | SVG |
| --- | --- | --- |
| Execution overview | [Mermaid](system-overview.mmd) | [Image](system-overview.svg) |
| Routed commands and events | [Mermaid](packet-to-report.mmd) | [Image](packet-to-report.svg) |
| Journaled engine | [Mermaid](journaled-engine.mmd) | [Image](journaled-engine.svg) |
| New-order transaction | [Mermaid](new-order-transaction.mmd) | [Image](new-order-transaction.svg) |
| Shutdown and recovery | [Mermaid](recovery-lifecycle.mmd) | [Image](recovery-lifecycle.svg) |

## Render

With Node.js, npm, and PowerShell installed, run from the repository root:

```text
pwsh -File scripts/diagrams/render.ps1
```

The script uses Mermaid CLI 11.17.0 and the checked-in configuration. The first
run may download the renderer and a headless browser. To use an installed
Chrome, set `PUPPETEER_EXECUTABLE_PATH` and `PUPPETEER_SKIP_DOWNLOAD=true` first.

The SVGs use text labels and system fonts, with no embedded HTML or remote font
files. IDs use a fixed seed. Rendering twice with the same renderer, browser,
and fonts should produce identical files.

## Review

Check arrows against the implementation before editing labels. The router and
journaled engine are separate APIs, not stages of one implemented service.
Keep that distinction visible, along with application versus durability.

Render every changed source and inspect its SVG at homepage width. Check label
clipping, arrow crossings, and readable text. Commit source and SVG together.
Historical benchmark archives do not belong in diagram regeneration.
