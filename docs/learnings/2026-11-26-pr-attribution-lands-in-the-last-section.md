# The PR attribution line lands in the last section

- Status: Open
- Found in: PR #15, the `pr body` check

## What happened

Claude Code ends every PR description with an attribution line, `🤖 Generated with [Claude
Code](...)`. The template's last section is `## Guardrail gaps`, and `cargo xtask pr-body` reads
a section up to the next `#` or `##` heading. So the attribution became part of the section:

```markdown
## Guardrail gaps

None.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
```

The check then saw two lines, `None.` and the attribution, and failed with "names no learning".
Any agent-written PR whose description says `None` here fails the same way.

## Proposed guardrail

In `xtask/src/pr.rs`, end a section at a thematic break (`---`) as well as at a heading, and add
`---` before the attribution line in `.github/pull_request_template.md`. Alternatively, ignore a
final line that starts with `🤖 Generated with`. A test with the attribution after `None` covers
either fix.
