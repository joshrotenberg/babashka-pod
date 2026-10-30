# Agent pod spike scripts

Scripts used to compare the pod with clajent for #41. They start real `claude` and `codex` turns, which cost money (a few cents per run with `haiku`).

| Script | Run with | Does |
|---|---|---|
| `live.bb` | `AGENT_POD=target/debug/pod-agent bb pods/agent/spike/live.bb claude haiku` (or `codex`) | ask, resumed ask, tell polled from a loop, events, output schema, cancel, timeout, busy agent |
| `live_clajent.clj` | `bb -cp <clajent>/src pods/agent/spike/live_clajent.clj claude haiku` (or `codex`) | the same through `clajent.core` |
| `exit.bb`, `sigint.bb` | as above, with `tell-then-exit` or `ask-block` | process cleanup when bb exits or gets a signal mid-turn |
| `exit_clajent.clj` | as above, with `start-then-exit` or `run-block` | the same through clajent |

Send signals from a process that resets SIGINT to the default: a non-interactive shell starts `&` jobs with SIGINT ignored, so `kill -INT` on them does nothing.
