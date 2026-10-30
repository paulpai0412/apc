import { apiFetch } from './api';
import { useTagStore } from '../store/tagStore';

/** Server acceptance only. PID application remains the engine's separate confirmation gate. */
export async function writeOpcNode(nodeId: string, value: number | string | boolean, timeoutMs = 10000): Promise<void> {
    let ownsPending = false;
    let timer: number | undefined;
    try {
        if (!nodeId || (typeof value === 'number' && !Number.isFinite(value))) throw new Error('節點或寫入值無效。');
        if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) throw new Error('寫入等待時間無效。');
        if (useTagStore.getState().isPending(nodeId)) throw new Error('此節點仍有未確認的寫入，請先確認結果。');
        useTagStore.getState().setPending(nodeId);
        ownsPending = true;
        const abort = new AbortController();
        timer = window.setTimeout(() => abort.abort(), timeoutMs);
        const response = await apiFetch('/api/runtime/write', {
            method: 'POST', headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ node_id: nodeId, value }), signal: abort.signal,
        }, true);
        const receipt: unknown = await response.json().catch(() => null);
        if (!response.ok) {
            const message = receipt !== null && typeof receipt === 'object' && 'error' in receipt
                && typeof receipt.error === 'string' ? receipt.error : `寫入未確認（HTTP ${response.status}）。`;
            throw new Error(message);
        }
        if (receipt === null || typeof receipt !== 'object' || Array.isArray(receipt)
            || !('accepted' in receipt) || receipt.accepted !== true) {
            throw new Error('未收到明確的 OPC 接受結果；請先查看回讀，不要直接重送。');
        }
    } catch (cause) {
        const message = cause instanceof Error && cause.name !== 'AbortError'
            ? cause.message : '寫入逾時或連線中斷；實際結果未知，請先查看回讀，不要直接重送。';
        const error = new Error(`${nodeId}：${message}`);
        useTagStore.getState().setWriteError(error.message);
        throw error;
    } finally {
        window.clearTimeout(timer);
        if (ownsPending) useTagStore.getState().clearPending(nodeId);
    }
}
