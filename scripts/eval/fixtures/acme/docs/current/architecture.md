# Current Architecture

The relay writes batches through a two-phase commit: a durable journal
accepts the batch, then the mirror confirms. The journal file is
journal.tsv inside the state directory.

The ledger reconciles batches nightly using reconcileLedger over the
published batch records.
