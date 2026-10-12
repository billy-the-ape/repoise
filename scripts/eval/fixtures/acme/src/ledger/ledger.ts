export interface LedgerEntry {
  batchId: string;
  amount: number;
}

export function reconcileLedger(entries: LedgerEntry[]): number {
  let total = 0;
  for (const entry of entries) {
    total += entry.amount;
  }
  return total;
}
