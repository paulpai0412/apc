import { useEffect, useState } from 'react';
import { apiFetch } from '../lib/api';
import type { UserRole } from '../lib/api';
import { runtimeError, workspaceRequest } from '../lib/runtimeWorkspace';

type ManagedUser = { id: number; username: string; role: UserRole; disabled: boolean; created_at: string };
type AuditEvent = { id: number; actor_username: string | null; actor_role: string | null; action: string; target: string | null; result: string; detail: string | null; created_at: string };
const roles: UserRole[] = ['viewer', 'operator', 'engineer', 'admin'];
const input = 'min-h-10 w-full rounded border border-slate-700 bg-slate-950 px-3 py-2 text-sm';
const button = 'min-h-10 rounded border border-slate-600 bg-slate-900 px-3 py-2 text-sm hover:border-indigo-400 disabled:opacity-40';

export function SettingsView({ isAdmin, active }: { isAdmin: boolean; active: boolean }) {
  const [tab, setTab] = useState<'users' | 'audit'>('users');
  const [users, setUsers] = useState<ManagedUser[]>([]); const [audit, setAudit] = useState<AuditEvent[]>([]);
  const [username, setUsername] = useState(''); const [password, setPassword] = useState(''); const [role, setRole] = useState<UserRole>('viewer');
  const [resetPasswords, setResetPasswords] = useState<Record<number, string>>({});
  const [actor, setActor] = useState(''); const [action, setAction] = useState(''); const [result, setResult] = useState('');
  const [error, setError] = useState(''); const [notice, setNotice] = useState(''); const [busy, setBusy] = useState(false); const [revision, setRevision] = useState(0);
  useEffect(() => {
    if (!isAdmin || !active) return;
    const controller = new AbortController();
    void workspaceRequest<ManagedUser[]>('/api/admin/users', undefined, controller.signal).then(value => { if (!controller.signal.aborted) setUsers(value); })
      .catch(cause => { if (!controller.signal.aborted) setError(runtimeError(cause)); });
    return () => controller.abort();
  }, [isAdmin, active, revision]);
  async function mutate(path: string, body: unknown, message: string) {
    if (busy || !isAdmin) return false;
    setBusy(true); setError(''); setNotice('');
    try {
      const response = await apiFetch(path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) }, true);
      if (!response.ok) throw new Error((await response.text()).slice(0, 500) || `HTTP ${response.status}`);
      setNotice(message); setRevision(value => value + 1); return true;
    } catch (cause) { setError(`${runtimeError(cause)} 請重新查詢帳號狀態；系統不自動重送。`); return false; }
    finally { setBusy(false); }
  }
  async function create() {
    if (!username.trim() || password.length < 8) { setError('請輸入帳號及至少 8 字元的密碼。測試啟動密碼例外不適用一般新增帳號。'); return; }
    if (!window.confirm(`建立帳號 ${username.trim()}，角色 ${role}？這不會自動授予任何設備 profile 的操作權。`)) return;
    if (await mutate('/api/admin/users', { username: username.trim(), password, role }, '帳號已建立；請另行核准設備範圍。')) { setUsername(''); setPassword(''); setRole('viewer'); }
  }
  async function loadAudit() {
    if (busy || !isAdmin) return; setBusy(true); setError('');
    const query = new URLSearchParams({ limit: '500' });
    if (actor.trim()) query.set('actor', actor.trim()); if (action.trim()) query.set('action', action.trim()); if (result.trim()) query.set('result', result.trim());
    try { setAudit(await workspaceRequest<AuditEvent[]>(`/api/admin/audit?${query}`)); }
    catch (cause) { setError(runtimeError(cause)); } finally { setBusy(false); }
  }
  function exportAudit() {
    const url = URL.createObjectURL(new Blob([JSON.stringify(audit, null, 2)], { type: 'application/json' }));
    const anchor = document.createElement('a'); anchor.href = url; anchor.download = `hmi-audit-${new Date().toISOString().replace(/[:.]/g, '-')}.json`; anchor.click();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  if (!isAdmin) return <p className="rounded border border-slate-700 p-3 text-sm text-slate-400">帳號管理與全域稽核僅限管理員。設備範圍權限以目前 profile 授權為準。</p>;
  return <section className="space-y-4 rounded-xl border border-slate-700 p-4" aria-label="帳號與全域稽核">
    <div className="flex gap-2"><button type="button" className={button} aria-pressed={tab === 'users'} onClick={() => setTab('users')}>帳號管理</button><button type="button" className={button} aria-pressed={tab === 'audit'} onClick={() => setTab('audit')}>全域稽核</button></div>
    {error && <p role="alert" className="text-sm text-rose-300">{error}</p>}{notice && <p role="status" className="text-sm text-emerald-300">{notice}</p>}
    <div hidden={tab !== 'users'} className="space-y-4"><form onSubmit={event => { event.preventDefault(); void create(); }} className="grid items-end gap-3 sm:grid-cols-2 lg:grid-cols-4"><label className="space-y-1 text-sm">新帳號<input className={input} autoComplete="off" value={username} onChange={event => setUsername(event.target.value)} required /></label><label className="space-y-1 text-sm">初始密碼<input className={input} autoComplete="new-password" type="password" minLength={8} value={password} onChange={event => setPassword(event.target.value)} required /></label><label className="space-y-1 text-sm">角色<select className={input} value={role} onChange={event => setRole(event.target.value as UserRole)}>{roles.map(value => <option key={value}>{value}</option>)}</select></label><button type="submit" className={button} disabled={busy}>確認新增帳號</button></form><p className="text-xs text-slate-400">角色與設備範圍是兩層授權。修改角色不會增加 profile 權限，也不會接管設備；既有安全政策會再次核對授權。</p><button type="button" className={button} disabled={busy} onClick={() => setRevision(value => value + 1)}>重新查詢帳號</button><div className="space-y-3">{users.map(user => <article key={user.id} className="grid gap-3 rounded border border-slate-800 p-3 md:grid-cols-[1fr_1fr_1fr_2fr]"><div className="text-sm"><strong>{user.username}</strong><p className="text-xs text-slate-400">ID {user.id} · {user.disabled ? '已停用' : '啟用中'}</p></div><label className="text-xs">角色<select className={input} value={user.role} disabled={busy} onChange={event => { const next = event.target.value as UserRole; if (window.confirm(`將 ${user.username} 的角色由 ${user.role} 改為 ${next}？`)) void mutate(`/api/admin/users/${user.id}/role`, { role: next }, '角色已更新。'); }}>{roles.map(value => <option key={value}>{value}</option>)}</select></label><button type="button" className={button} disabled={busy} onClick={() => { if (window.confirm(`${user.disabled ? '啟用' : '停用'}帳號 ${user.username}？此動作不會自行停止現場引擎。`)) void mutate(`/api/admin/users/${user.id}/disable`, { disabled: !user.disabled }, '帳號狀態已更新。'); }}>{user.disabled ? '啟用帳號' : '停用帳號'}</button><div className="flex flex-wrap gap-2"><input aria-label={`${user.username} 的重設密碼`} type="password" autoComplete="new-password" className={`${input} min-w-0 flex-1`} value={resetPasswords[user.id] ?? ''} onChange={event => setResetPasswords(current => ({ ...current, [user.id]: event.target.value }))} placeholder="至少 8 字元" /><button type="button" className={button} disabled={busy || (resetPasswords[user.id]?.length ?? 0) < 8} onClick={async () => { if (!window.confirm(`重設 ${user.username} 的密碼並使其重新驗證？`)) return; if (await mutate(`/api/admin/users/${user.id}/password`, { new_password: resetPasswords[user.id] }, '密碼已重設。')) setResetPasswords(current => ({ ...current, [user.id]: '' })); }}>重設密碼</button></div></article>)}</div></div>
    <div hidden={tab !== 'audit'} className="space-y-3"><form onSubmit={event => { event.preventDefault(); void loadAudit(); }} className="grid items-end gap-3 sm:grid-cols-2 lg:grid-cols-4"><label className="space-y-1 text-sm">操作者<input className={input} value={actor} onChange={event => setActor(event.target.value)} /></label><label className="space-y-1 text-sm">動作<input className={input} value={action} onChange={event => setAction(event.target.value)} /></label><label className="space-y-1 text-sm">結果<input className={input} value={result} onChange={event => setResult(event.target.value)} /></label><button type="submit" className={button} disabled={busy}>查詢最新 500 筆</button></form><button type="button" className={button} disabled={!audit.length} onClick={exportAudit}>匯出本次稽核查詢</button><div className="overflow-x-auto"><table className="w-full min-w-[650px] text-left text-xs"><thead><tr>{['時間', '操作者', '動作', '目標', '結果／詳細'].map(label => <th key={label} className="p-2 font-normal text-slate-400">{label}</th>)}</tr></thead><tbody>{audit.map(event => <tr key={event.id} className="border-t border-slate-800"><td className="p-2">{event.created_at}</td><td className="p-2">{event.actor_username ?? '系統'} · {event.actor_role}</td><td className="p-2">{event.action}</td><td className="max-w-64 break-all p-2">{event.target}</td><td className="max-w-96 break-all p-2">{event.result}{event.detail && <details><summary className="cursor-pointer text-slate-400">詳細</summary>{event.detail}</details>}</td></tr>)}</tbody></table></div></div>
  </section>;
}
