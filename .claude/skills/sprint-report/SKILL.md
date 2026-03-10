---
name: sprint-report
description: Generate a sprint status report for the current phase. Use --detailed for a full per-sprint breakdown or --table for a condensed summary table.
---

# Sprint Report Skill

Generate a formatted status report for all sprints in the current phase.

## Usage

```
/sprint-report [--table | --detailed]
```

Default: `--table`

---

## Invocation

Build the fenced JSON context and pipe it directly via `--var-file -` (stdin),
or write to a temp file and pass the path:

```bash
# Preferred: pipe JSON directly without a temp file
echo '<json>' | sc-compose render .claude/skills/sprint-report/report.md.j2 --var-file -

# Alternative: write to a temp file first
echo '<json>' > /tmp/sprint-report.json
sc-compose render .claude/skills/sprint-report/report.md.j2 --var-file /tmp/sprint-report.json
```

Output the rendered result directly in the conversation.

---

## Icon Reference

**Dev**
| State | Icon |
|-------|------|
| ASSIGNED | 📥 |
| IN_PROGRESS | 🌀 |
| DONE | ✅ |
| FINDINGS | 🚩 |
| FIXING | 🔨 |

**QA**
| State | Icon |
|-------|------|
| ASSIGNED | 📥 |
| IN_PROGRESS | 🌀 |
| FINDINGS | 🚩 |
| PASSED | ✅ |

**CI**
| State | Icon |
|-------|------|
| RUNNING | 🌀 |
| BLOCKED | 🚧 |
| FAIL | ❌ |
| PASS | ✅ |
| MERGED | 🏁 |
| READY TO MERGE | 🚀 |

---

## Variables

`sprint_rows` — newline-separated table rows, one per sprint:
```
| AJ.1 | 🏁 | ✅ | 🏁 | #615 |
| AJ.2 | | ✅ | 🏁 | #616 |
```

`integration_row` — single integration branch row:
```
| **integrate** | | — | 🌀 | — |
```

---

## JSON Input

```json
{
  "sprint_rows": "| AJ.1 | 🏁 | ✅ | 🏁 | #615 |\n| AJ.2 | | ✅ | 🏁 | #616 |",
  "integration_row": "| **integrate** | | — | 🌀 | — |"
}
```
