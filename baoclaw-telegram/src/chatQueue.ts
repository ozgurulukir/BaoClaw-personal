/**
 * Per-chat message queue (one message at a time per chat).
 */
export class ChatQueue {
  private queues = new Map<number, string[]>();
  private processing = new Set<number>();

  enqueue(chatId: number, text: string): void {
    const q = this.queues.get(chatId) ?? [];
    q.push(text);
    this.queues.set(chatId, q);
  }

  dequeue(chatId: number): string | undefined {
    const q = this.queues.get(chatId);
    if (!q || q.length === 0) return undefined;
    return q.shift();
  }

  hasQueued(chatId: number): boolean {
    const q = this.queues.get(chatId);
    return !!q && q.length > 0;
  }

  isProcessing(chatId: number): boolean {
    return this.processing.has(chatId);
  }

  startProcessing(chatId: number): void {
    this.processing.add(chatId);
  }

  finishProcessing(chatId: number): void {
    this.processing.delete(chatId);
  }
}
