# ADR 001: Relay commit protocol

Decision: the relay uses a two-phase commit against the durable journal.
The in-memory event queue proposal (docs/plans/relay-plan.md) is superseded
for v1; revisit after the drain-time budget is measured.
