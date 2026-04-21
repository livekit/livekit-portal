# RTC example

Request-driven real-time chunking on top of Portal.

The robot fires an RPC when its action queue is running low. The policy
runs inference and pushes the new chunk back as a typed `ActionChunk` over
a reserved byte-stream topic. The robot splices the chunk into its queue
using the `real_delay = current_step - chunk_start_step` rule.

Portal is pure transport here. All RTC state (queue, splicing, step
indexing) lives in `robot.py`.

## Run it

One-time:

```bash
cp .env.example .env   # fill in LIVEKIT_* credentials
uv sync
```

Two terminals:

```bash
uv run robot.py
uv run policy.py
```

Expected output:
- `[robot]` prints a tick summary and logs RPC round-trip times every few chunks
- `[policy]` prints "served N chunks" as it emits

## Knobs

All env-var driven. Defaults in `.env.example`.

| Var | Default | What it changes |
|---|---|---|
| `RTC_CONTROL_HZ` | 30 | Robot control rate |
| `RTC_HORIZON` | 32 | Chunk length in control ticks |
| `RTC_INFERENCE_MS` | 40 | Simulated GPU latency on the policy side |
| `RTC_DURATION_SECONDS` | 20 | How long the robot runs |
| `RTC_TRIGGER_REMAINING` | 12 | Queue threshold for firing the next RPC |

## Run the policy on Modal

Everything stays the same on the robot side. Only where the policy runs
moves.

```bash
pip install modal
modal setup
modal run policy_modal.py
```

`policy_modal.py` mounts the worktree's editable `livekit-portal` into a
Modal image and runs the same `policy.py` inside. Credentials come from
the local shell via `Secret.from_local_environ`.

## What to read

- `robot.py` — the control loop, the queue splice, the RPC trigger
- `policy.py` — the RPC handler that emits chunks
- `policy_modal.py` — optional Modal wrapper
