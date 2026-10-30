import { useCallback, useEffect, useRef } from 'react';
import { useTagStore } from '../store/tagStore';
import { writeOpcNode } from '../lib/opcWrite';

interface WriteNodeOptions {
  timeout?: number;
  onSuccess?: () => void;
  onError?: (error: Error) => void;
}

export const useWriteNode = (
  nodeId: string,
  wsRef: React.RefObject<WebSocket | null>,
  options: WriteNodeOptions = {}
) => {
  const { timeout = 10000, onSuccess, onError } = options;
  const isWriting = useTagStore(state => state.pendingWrites.has(nodeId));
  const activeNode = useRef(nodeId);
  const mounted = useRef(true);
  activeNode.current = nodeId;
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  const write = useCallback(async (value: number | string | boolean) => {
    try {
      // The stream is needed for observing the result, not for acknowledging the request.
      if (!wsRef.current || wsRef.current.readyState !== WebSocket.OPEN) {
        throw new Error('觀測連線尚未建立；不發送寫入。');
      }
      await writeOpcNode(nodeId, value, timeout);
      if (mounted.current && activeNode.current === nodeId) onSuccess?.();
    } catch (cause) {
      const error = cause instanceof Error ? cause : new Error('寫入未確認。');
      useTagStore.getState().setWriteError(error.message);
      if (mounted.current && activeNode.current === nodeId) onError?.(error);
    }
  }, [nodeId, wsRef, timeout, onSuccess, onError]);

  return { isWriting, write };
};
