import { apiFetch } from './api';

export async function researchRequest<T>(path = '', payload?: unknown, signal?: AbortSignal): Promise<T> {
  const response = await apiFetch(`/api/research${path}`, {
    method: payload === undefined ? 'GET' : 'POST',
    headers: payload === undefined ? undefined : { 'Content-Type': 'application/json' },
    body: payload === undefined ? undefined : JSON.stringify(payload),
    signal,
  }, payload !== undefined);
  const text = await response.text();
  let data: unknown;
  try { data = text ? JSON.parse(text) : null; } catch { throw new Error(`HTTP ${response.status}: ${text.slice(0, 500) || '伺服器未回傳 JSON'}`); }
  if (!response.ok) {
    const detail = data && typeof data === 'object' && 'error' in data ? String(data.error) : text.slice(0, 500);
    const advice = response.status === 401 ? '登入已過期，請重新登入。' : response.status === 403 ? '此操作未獲授權；請確認明確的建模權限與目標範圍。' : '';
    throw new Error(`${advice} ${detail || `HTTP ${response.status}`}`.trim());
  }
  return data as T;
}

export function researchError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
