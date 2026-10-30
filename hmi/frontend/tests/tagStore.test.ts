import { test } from 'node:test';
import assert from 'node:assert/strict';
import { useTagStore } from '../src/store/tagStore.ts';

test('subscriptions, even matching Good values, never acknowledge a pending command', () => {
    const node = 'TestLoop:OperatingMode';
    const store = useTagStore.getState();
    store.setPending(node);
    try {
        store.updateTag({ node_id: node, value: 2, status: 0, timestamp: '2026-01-01T00:00:00Z' });
        assert.equal(useTagStore.getState().isPending(node), true);
        store.updateTags([
            { node_id: node, value: 1, status: 0x80000000, timestamp: '2026-01-01T00:00:01Z' },
            { node_id: 'Another:PV', value: 10, status: 0, timestamp: '2026-01-01T00:00:01Z' },
        ]);
        assert.equal(useTagStore.getState().isPending(node), true);
        assert.equal(useTagStore.getState().getTagValue(node), 1);
    } finally {
        store.clearPending(node);
    }
});
