export interface RelayBatch {
  id: string;
  records: string[];
}

export function computeBatchId(records: string[]): string {
  let digest = 0;
  for (const record of records) {
    digest = (digest * 31 + record.charCodeAt(0)) >>> 0;
  }
  return batchIdHex(digest);
}

export function batchIdHex(digest: number): string {
  return `batch-${digest.toString(16)}`;
}

export class RelayQueue {
  private pending: RelayBatch[] = [];

  enqueue(records: string[]): RelayBatch {
    const batch: RelayBatch = { id: computeBatchId(records), records };
    this.pending.push(batch);
    return batch;
  }

  flush(): RelayBatch[] {
    const drained = this.pending;
    this.pending = [];
    return drained;
  }
}
