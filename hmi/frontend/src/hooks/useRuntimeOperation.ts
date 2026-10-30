import { useCallback, useEffect, useRef, useState } from 'react';
import { runtimeError, workspaceRequest } from '../lib/runtimeWorkspace';
import type { RuntimeOperation } from '../types/RuntimeWorkspace';

type StoredIntent = { path: string; body: { operation_id: string; profile_hash: string } & Record<string, unknown> };
export type RuntimeOperationTracker = {
  intent: StoredIntent | null; record: RuntimeOperation | null; error: string; busy: boolean; unresolved: boolean;
  submit: (path: string, body: StoredIntent['body'], safetyRelease?: boolean) => Promise<void>;
  reconcile: () => Promise<void>;
  refresh: () => void;
};

export function useRuntimeOperation(userId: number, profileHash: string, active: boolean, onRefresh: () => void): RuntimeOperationTracker {
  const key = `authrity:runtime-intent:${userId}:${profileHash}`;
  const currentKey = useRef(key); currentKey.current = key;
  const submitting = useRef(false);
  const [intent, setIntent] = useState<StoredIntent | null>(null);
  const [record, setRecord] = useState<RuntimeOperation | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    setRecord(null); setError(''); setIntent(null);
    if (!profileHash) return;
    try {
      const stored = localStorage.getItem(key);
      if (!stored) return;
      const parsed: unknown = JSON.parse(stored);
      if (!parsed || typeof parsed !== 'object' || !('body' in parsed) || !('path' in parsed)) throw new Error('儲存的操作識別無效，請從伺服器操作紀錄查詢。');
      const value = parsed as StoredIntent;
      if (value.body?.profile_hash !== profileHash || typeof value.body.operation_id !== 'string'
        || !/^[A-Za-z0-9_-]{1,128}$/.test(value.body.operation_id) || typeof value.path !== 'string'
        || !/^\/api\/(runtime\/operations|control\/versions\/[a-f0-9]{64}\/install)$/.test(value.path)) throw new Error('儲存的操作識別無效，請從伺服器操作紀錄查詢。');
      setIntent(value);
    } catch (cause) { setError(runtimeError(cause)); }
  }, [key, profileHash]);
  useEffect(() => {
    if (!intent || !record || record.operation_id !== intent.body.operation_id || record.profile_hash !== profileHash
      || (record.state !== 'accepted' && record.state !== 'rejected' && !(record.state === 'unknown' && record.blocking === false))) return;
    try {
      const stored = localStorage.getItem(key);
      // A newer intent from another tab must not be removed by an older response.
      if (stored && JSON.parse(stored)?.body?.operation_id === record.operation_id) localStorage.removeItem(key);
    } catch { setError('操作回條已確認，但無法清除本機暫存。重新整理後請查詢原操作，不可重送。'); }
  }, [intent, record, profileHash, key]);
  useEffect(() => {
    if (!active || !intent) return;
    const controller = new AbortController();
    let timer: number | undefined;
    async function query() {
      try {
        const value = await workspaceRequest<RuntimeOperation>(`/api/runtime/operations/${encodeURIComponent(intent!.body.operation_id)}`, undefined, controller.signal);
        if (controller.signal.aborted) return;
        if (value.operation_id !== intent!.body.operation_id || value.profile_hash !== profileHash) throw new Error('操作回條身分不符。');
        setRecord(value); setError('');
        if (value.state === 'executing' || (value.state === 'unknown' && value.blocking !== false)) timer = window.setTimeout(() => void query(), 2000);
        else onRefresh();
      } catch (cause) {
        if (controller.signal.aborted) return;
        setError(`操作結果尚未確認：${runtimeError(cause)}`);
        timer = window.setTimeout(() => void query(), 5000);
      }
    }
    void query();
    return () => { controller.abort(); window.clearTimeout(timer); };
  }, [active, intent, profileHash, revision, onRefresh]);

  const submit = useCallback(async (path: string, body: StoredIntent['body'], safetyRelease = false) => {
    if (submitting.current || !profileHash || body.profile_hash !== profileHash) return;
    if (intent && (!record || record.state === 'executing' || (record.state === 'unknown' && record.blocking !== false)) && !safetyRelease) {
      setError('先查清原操作結果；不得以新 ID 重送相同意圖。安全退出另由原生引擎驗證。'); return;
    }
    const pending: StoredIntent = { path, body };
    try { localStorage.setItem(key, JSON.stringify(pending)); }
    catch { setError('無法保存操作識別；為避免重新整理後失去查詢能力，尚未送出。'); return; }
    submitting.current = true; setBusy(true); setIntent(pending); setRecord(null); setError('');
    try {
      const value = await workspaceRequest<RuntimeOperation>(path, body);
      if (value.operation_id !== body.operation_id || value.profile_hash !== profileHash) throw new Error('操作回條身分不符；查詢原 ID，不可重送。');
      if (currentKey.current === key) { setRecord(value); setRevision(number => number + 1); onRefresh(); }
    } catch (cause) {
      if (currentKey.current === key) setError(`${runtimeError(cause)} 已保留原操作 ID；系統只查詢，不自動重送。`);
    } finally {
      submitting.current = false;
      setBusy(false);
    }
  }, [key, profileHash, intent, record, onRefresh]);

  const reconcile = useCallback(async () => {
    if (!intent || submitting.current) return;
    submitting.current = true; setBusy(true); setError('');
    try {
      const native = intent.path === '/api/runtime/operations';
      const path = native ? `/api/runtime/operations/${encodeURIComponent(intent.body.operation_id)}/reconcile` : intent.path.replace(/\/install$/, '/reconcile');
      const value = await workspaceRequest<RuntimeOperation>(path, native ? { original_request: intent.body } : intent.body);
      if (value.operation_id !== intent.body.operation_id || value.profile_hash !== profileHash) throw new Error('核對回條身分不符。');
      if (currentKey.current === key) { setRecord(value); setRevision(number => number + 1); onRefresh(); }
    } catch (cause) { if (currentKey.current === key) setError(runtimeError(cause)); }
    finally { submitting.current = false; setBusy(false); }
  }, [intent, profileHash, key, onRefresh]);

  const refresh = useCallback(() => setRevision(value => value + 1), []);
  return { intent, record, error, busy, submit, reconcile, refresh,
    unresolved: !!intent && (!record || record.state === 'executing' || (record.state === 'unknown' && record.blocking !== false)) };
}

