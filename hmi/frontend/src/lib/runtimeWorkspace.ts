import type { RuntimeOperationAction } from '../types/RuntimeWorkspace';
import { apiFetch } from './api';

export function runtimeError(cause: unknown): string {
  return cause instanceof Error ? cause.message : '無法確認伺服器結果。請查詢原操作紀錄，不要重複提交。';
}

export async function workspaceRequest<T>(path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
  const response = await apiFetch(path, {
    method: body === undefined ? 'GET' : 'POST', signal,
    ...(body === undefined ? {} : { headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) }),
  }, body !== undefined);
  const text = await response.text();
  let value: unknown;
  try { value = JSON.parse(text); }
  catch { throw new Error(`伺服器回覆不是有效 JSON（HTTP ${response.status}），操作結果需重新查詢。`); }
  if (!response.ok) {
    const message = value && typeof value === 'object' && 'error' in value && typeof value.error === 'string' ? value.error : `HTTP ${response.status}`;
    throw new Error(response.status === 401 ? '登入已過期，請重新登入後查詢原操作。' : message);
  }
  return value as T;
}

export function targetLabel(target: { plant: string; line: string; area: string; group: string }): string {
  return [target.plant, target.line, target.area, target.group].join(' ／ ');
}

export function runtimeReadinessAllows(action: RuntimeOperationAction['kind'], healthy: boolean, unresolved: boolean): boolean {
  if (action === 'hold' || action === 'exit' || action === 'stop' || action === 'recover_monitor') return true;
  // Monitor can recover cold history, but must not bypass an unresolved operation.
  return !unresolved && (healthy || action === 'monitor' || action === 'start' || action === 'start_candidate');
}
