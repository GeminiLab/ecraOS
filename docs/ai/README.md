# AI instruction maintenance

The repository separates always-needed project context from instructions loaded
for the current work. It currently has three general-purpose skills, without
module-specific or technical-topic rule sets.

## Responsibilities

| Location | Contents |
| --- | --- |
| [AGENTS.md](../../AGENTS.md) | Project map, core agreements, and explicit skill triggers |
| [ecra-code](../../.agents/skills/ecra-code/SKILL.md) | Implementation and review workflow, with a Rust style reference |
| [ecra-docs](../../.agents/skills/ecra-docs/SKILL.md) | Doc comments, standalone documents, and examples |
| [ecra-verify](../../.agents/skills/ecra-verify/SKILL.md) | Test selection, checks, and evidence, with a build/test reference |

Keep planning lightweight in the root instructions. Add a separate workflow only
when real tasks demonstrate a need for it.

## Loading behavior

Codex discovers repository skills under `.agents/skills/`. A skill's name and
description help it select the skill, while the full instructions are read when
used. Root instructions explicitly require the relevant skills as an additional
routing mechanism. Other tools can follow the same paths manually.

A Markdown link does not include the linked file automatically. Read each skill
before its relevant action, and resolve its reference links relative to that skill.
Multiple skills can apply together:

- A new Rust item needs code and documentation rules, with verification selected
  before implementation when behavior changes.
- A prose-only update needs documentation and verification rules.
- A review needs the code rules and any documentation or verification rules
  relevant to the requested review.

Stage changes can require more instructions. Files already read are not automatically
removed from the conversation when the stage ends. After editing root instructions,
start a new session to check startup loading. Confirm newly added skills appear in
the skill list, restarting if needed.

## Updating and reviewing

- Edit each rule at its authoritative location and update routing when applicability changes.
- Keep skill descriptions specific about when they apply. Keep procedures in the body.
- Add reference files when they keep conditional detail out of routine work.
- Keep command details aligned with the scripts and CI. Do not copy full scripts into rules.
- Check frontmatter, relative links, ignored-file status, and preservation of existing rules.
- Try representative prompts in a fresh session, such as a Rust bug fix, a private
  function's doc comment, and a documentation-only update. Check which skills are
  actually read. Structural validation alone does not prove automatic selection works.

The root map provides the current high-level description of the kernel, architecture
support, loaders, and supporting crates. Extend that description as needed before
introducing separate instructions for individual subsystems.
