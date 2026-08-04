# ACP Strict Resume Design

Issue: https://github.com/smelt-ai/smelt/issues/1

## Goal

Preserve ACP conversation context across smeltd restarts and upgrades. Starting
a replacement agent process is expected after daemon state is lost, but that
must not silently create a new conversation when restoration fails
transiently.

## Persisted identity

`workspace.json` remains the source of the two identities needed during cold
restore:

- `sid` identifies a daemon-hosted ACP session and supports cheap attachment
  while the smeltd slot is still alive.
- `resume_session_id` identifies the conversation owned by the agent and
  supports protocol-level restoration after the daemon slot is gone.

The process ID is not persisted and does not determine whether a conversation
should be resumed or created.

## Restore flow

The GUI restores an ACP placeholder with the persisted identities, then sends
`acp_open` when the session becomes active.

smeltd handles the request in this order:

1. If the registry contains a live slot for `sid`, attach to it without
   spawning an agent.
2. If the slot is absent or ended, spawn a replacement agent process with the
   persisted `resume_session_id`.
3. The connection attempts `session/load` with the persisted `resume_session_id`
   (cold restore). `session/resume` does not replay history and is **not** used
   for cold restore — it only supports attach-style resume while the agent
   process is alive, so restoration after a dead daemon slot always goes
   through `session/load`.
4. Create a new conversation only when restoration is explicitly unsupported
   or the historical session is confirmed missing.
5. Return a retryable restoration failure for timeouts, transport failures,
   malformed responses, and other protocol errors. Do not call `session/new`.

The existing successful resume modes remain unchanged: resume keeps the local
history, while load clears the local snapshot and accepts replayed history.

## Failure classification

The ACP connection layer will classify restoration outcomes rather than
collapsing every error into the current `session/new` fallback:

- `Resumed`: resume or load succeeded.
- `Unavailable`: the agent does not support either restoration method, or
  reports that the requested historical session does not exist.
- `Retryable`: the restoration attempt could not establish whether the
  historical session exists because of an I/O, timeout, process, or protocol
  failure.

Only `Unavailable` may continue to `session/new`. Error classification will use
typed ACP/JSON-RPC error information where available. It will not infer
"missing" from arbitrary display strings.

## Persistence and retry

A retryable failure ends the current replacement process and leaves the view in
an ended state with a clear "recovery failed, retry" message. The persisted
`resume_session_id`, local entries, `sid`, launch specification, and profile
identity remain unchanged.

Retrying repeats the same attach-or-resume flow with the original
`resume_session_id`. A failed restoration must not persist a newly allocated
agent session ID or replace the saved conversation with an empty snapshot.

When `Unavailable` leads to `session/new`, the UI receives an explicit status
that the historical conversation could not be restored and a new conversation
was created. The newly returned session ID becomes the persisted identity only
after the new session handshake succeeds.

## UI behavior

The existing automatic cold-resume trigger remains. A retryable failure uses
the current ended-state restart action, with copy that distinguishes recovery
failure from a completed conversation. No new modal or settings are added.

## Tests

Regression coverage will verify:

- A live daemon slot uses attach and does not launch another agent.
- A missing daemon slot restores through `session/resume`.
- An adapter that directs the client to load restores through `session/load`.
- Explicit unsupported or not-found outcomes may call `session/new` and emit
  an explanatory status.
- Timeout, transport, and protocol failures never call `session/new`.
- A retryable failure preserves the original `resume_session_id` and local
  entries for the next retry.
- A successful new-session fallback persists its new ID only after the
  handshake succeeds.

Targeted tests for `smelt-core`, `smeltd`, and `smelt-acp-view` will run before
the workspace check.
