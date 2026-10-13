# Relay Plan (proposed)

Status: proposed. This plan is not the current behavior.

We plan to move the relay from a durable journal to an in-memory event
queue with a single-phase commit. The plan assumes the queue drains
before shutdown.

Open risks: loss on crash, drain-time budget.
