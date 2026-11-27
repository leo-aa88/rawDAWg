# rawDAWg — Claude Code instructions

The repository's instructions for coding agents live in `AGENTS.md`, shared with other tools so
there is one copy to keep current. Edit the rules there, not here.

@AGENTS.md

## Claude Code specifics

- Keep the session's task list current and end multi-step turns with the wrap-up, as the app
  expects; the user follows progress there.
- For broad reviews ("double check everything", adversarial review), independent reviewer agents
  that get the code rather than your conclusions find more. Verify each finding yourself before
  reporting it.
- Commits end with the attribution trailer the harness specifies.
