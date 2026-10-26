// Usage accounting: folds declared usage events and the final total into one
// normalized result without double counting. Missing measurements stay null;
// estimates are tagged and are not interchangeable with provider counts.

import { USAGE_FIELDS } from "./protocol.js";

export function emptyUsage() {
  const usage = { estimated: false };
  for (const field of USAGE_FIELDS) usage[field] = null;
  return usage;
}

function copyValues(values) {
  const out = {};
  for (const field of USAGE_FIELDS) {
    const value = values?.[field];
    out[field] =
      typeof value === "number" && Number.isFinite(value) ? value : null;
  }
  return out;
}

export class UsageLedger {
  /** @param {"cumulative"|"delta"} semantics declared in the hello event. */
  constructor(semantics = "cumulative") {
    this.semantics = semantics;
    this.ledger = emptyUsage();
    this.final = null;
    this.sawEstimated = false;
  }

  /** Record one `usage` event. */
  recordUsage(event) {
    if (event?.source === "estimate") this.sawEstimated = true;
    const values = copyValues(event?.values ?? {});
    for (const field of USAGE_FIELDS) {
      if (values[field] === null) continue;
      if (this.semantics === "delta") {
        this.ledger[field] = (this.ledger[field] ?? 0) + values[field];
      } else {
        // Cumulative snapshots: the largest observed value wins, so an
        // out-of-order or repeated snapshot cannot inflate the total.
        this.ledger[field] =
          this.ledger[field] === null ? values[field] : Math.max(this.ledger[field], values[field]);
      }
    }
  }

  /** Record the final event's usage total (authoritative when present). */
  recordFinal(finalUsage) {
    if (typeof finalUsage !== "object" || finalUsage === null) return;
    this.final = copyValues(finalUsage);
    if (finalUsage.estimated === true) this.sawEstimated = true;
  }

  /** Resolve the single normalized total for the result.
   * - cumulative: per-field max of ledger and final.
   * - delta: the final total is authoritative when present (it already
   *   includes the deltas; adding both would double count); otherwise the
   *   summed deltas are used. */
  resolve() {
    const result = emptyUsage();
    result.estimated = this.sawEstimated;
    for (const field of USAGE_FIELDS) {
      const ledgerValue = this.ledger[field];
      const finalValue = this.final ? this.final[field] : null;
      if (this.semantics === "cumulative") {
        result[field] =
          ledgerValue === null ? finalValue : finalValue === null ? ledgerValue : Math.max(ledgerValue, finalValue);
      } else {
        result[field] = finalValue ?? ledgerValue;
      }
    }
    return result;
  }
}
