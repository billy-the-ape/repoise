import { computeBatchId, RelayQueue } from "../src/relay/relay";

export function testBatchIdIsStable() {
  const first = computeBatchId(["a", "b"]);
  const second = computeBatchId(["a", "b"]);
  return first === second && first.startsWith("batch-");
}

export function testQueueDrainsOnFlush() {
  const queue = new RelayQueue();
  queue.enqueue(["x"]);
  return queue.flush().length === 1;
}
