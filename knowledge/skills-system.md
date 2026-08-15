# Skills system

`crates/xaft-skills` loads agent knowledge files that extend capabilities
with domain conventions and tool-usage patterns.

## Layout

```text
.xaft/skills/           # project-local skills (loaded first)
  my-skill.md
~/.config/xaft/skills/  # global skills (loaded second, deduped by name)
  shared.md
SKILL.md                # bundle at the working dir root
```

Each `.md` may start with YAML frontmatter:

```markdown
---
name: my-skill
description: Explains something
priority: 10
tags: rust, async
---
# The actual skill content…
```

## Loading

`SkillLoader::for_working_dir` + `load_all()` discovers, reads (32 KiB per
file / 128 KiB total caps), parses frontmatter, dedupes by name (local wins),
sorts by priority desc then name, and builds the prompt section via
`build_prompt_section`.

## TUI trigger

The `$` trigger opens a skill-only picker (`SkillTriggerHandler` in
`crates/xaft-tui/src/trigger/skill.rs`) listing loaded skills, filtering by
name/description prefix, and inserting `$<name> ` on selection. `AppState::
init_skills` replaces the handler once skills are loaded.

## Related

- `crates/xaft-skills/src/`
- PRD-52 in `prds/`
