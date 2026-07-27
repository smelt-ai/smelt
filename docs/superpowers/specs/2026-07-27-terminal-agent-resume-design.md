# Terminal Agent Resume Design

Issue: https://github.com/smelt-ai/smelt/issues/4

## Goal

Restore non-ACP Claude Code, Codex, Copilot, and Grok conversations after
smeltd loses the original terminal process. A missing daemon process must not
silently turn a saved agent tab into a new conversation.

## Boundaries

The generic terminal layer continues to own shells, PTYs, attachment, layout,
and protocol-level state only. It does not parse agent history or contain
Claude-specific assumptions.

Agent-specific behavior lives behind terminal-session adapters in
`session_history.rs`, which is already the only GUI module that understands
the four private history formats and extracts their resume IDs.

## Launch metadata

`LaunchEntry` gains an optional terminal agent kind. The built-in Claude Code,
Codex, Copilot, and Grok entries set it explicitly. Custom entries remain
ordinary terminal launches unless the user selects one of the supported agent
kinds.

Each persisted terminal leaf may contain:

- The terminal agent kind.
- The agent-owned session ID.
- The original launch command.

Older workspace and launch configuration files omit the new fields and retain
their current behavior.

## Session identity binding

Claude Code, Copilot, and Grok accept a caller-provided UUID for a new
conversation. Before first launch, smelt generates an ID and asks the adapter
to add the agent's session-ID argument to the original command. The same ID is
stored on the terminal view immediately, so simultaneous tabs cannot be
confused.

Codex assigns its own ID. Before launch, smelt snapshots the Codex session IDs
for the project. It polls the existing history reader after launch and binds
the tab only when exactly one new project-matching session appears. Zero
matches remain pending until the bounded observation period ends. Multiple
matches are ambiguous and leave that tab without a bound ID; smelt does not
guess between them.

The adapter preserves the user's original executable and flags when producing
both initial and resume commands. It shell-quotes generated IDs and does not
interpret arbitrary custom shell pipelines as supported agent launches.

## Restore flow

Workspace cold restore distinguishes an initial terminal launch from a saved
agent restoration:

1. Send the saved smeltd `sid` as today.
2. If the daemon still owns that session, attach and ignore all launch
   metadata.
3. If the session is absent and the leaf is an ordinary shell, recreate it as
   today.
4. If the session is absent and the leaf is a supported agent, require a
   persisted agent session ID and verify that its history still exists.
5. Ask the adapter to build the agent-specific resume command and launch that
   command under the same terminal `sid`.

The terminal open request carries distinct initial and restore intent so
smeltd never has to infer intent from process IDs or command text. A saved
agent restoration without a valid resume command returns an explicit error
instead of executing the original command.

Daemon handoff and upgrade keep their existing behavior: a live PTY is
transferred and attached, so no CLI resume command runs.

## Agent commands

Adapters generate commands according to each CLI:

- Claude Code: append `--resume <session-id>` while retaining original flags.
- Copilot: append `--resume=<session-id>` while retaining original flags.
- Grok: append `--resume <session-id>` while retaining original flags.
- Codex: insert `resume <session-id>` after the executable and retain supported
  original global flags.

Command construction is covered by tests and remains centralized in the
adapter rather than duplicated in workspace restoration.

## Error handling

The following conditions are restoration failures and never fall back to the
original launch command:

- No bound session ID.
- The persisted history no longer exists.
- Codex history matching is ambiguous.
- The adapter cannot safely transform the launch command.
- The resume process fails to start.

The restored tab remains visible with an actionable "unable to restore"
terminal error. Users may close it or manually start another conversation, but
smelt does not make that destructive choice automatically.

## Persistence

The agent kind and session ID are written with the terminal leaf as soon as
they become known. The existing workspace save subscription must be notified
when asynchronous Codex discovery binds an ID.

Attaching a live daemon session does not replace persisted identity. Failed
restoration also keeps the saved metadata so a later retry can succeed after a
temporary filesystem or executable problem is fixed.

## Tests

Regression coverage will verify:

- Initial command construction and resume command construction for all four
  agents.
- Shell quoting and preservation of original launch flags.
- Deterministic IDs for Claude Code, Copilot, and Grok.
- Unique Codex history-delta binding and ambiguous-delta rejection.
- Serialization compatibility for old and new launch/workspace files.
- Live daemon sessions attach without running a resume command.
- Missing daemon sessions resume the persisted agent conversation.
- Ordinary shell sessions still recreate normally.
- Missing IDs, missing history, unsafe commands, and spawn failures never run
  the original agent launch command.
- Daemon handoff keeps the live PTY and does not invoke CLI resume.

Targeted tests for `smelt`, `smelt-core`, and `smeltd` will run before the
workspace check.
