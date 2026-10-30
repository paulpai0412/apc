import { useEffect, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import { Sidebar } from './components/Sidebar';
import { Autoresearch } from './components/Autoresearch';
import { RuntimeWorkspace } from './components/RuntimeWorkspace';
import { apiChangePassword, apiLogin, apiLogout, apiMe, hasRoleAtLeast } from './lib/api';
import type { UserRole } from './lib/api';
import { runtimeError, workspaceRequest } from './lib/runtimeWorkspace';
import { useWorkspaceScene } from './hooks/useWorkspaceScene';
import { modelingPages, runtimePages } from './types/Workspace';
import type { ModelingPage, RuntimePage, WorkspaceMode } from './types/Workspace';
import type { ResearchWorkspaceContext } from './types/Research';
import type { RuntimeWorkspaceContext, WorkspaceCapabilities } from './types/RuntimeWorkspace';

const control = 'min-h-10 w-full rounded border border-slate-700 bg-slate-950 px-3 py-2 text-sm text-slate-100';
const button = 'min-h-10 rounded border border-slate-700 bg-slate-900 px-3 py-2 text-sm hover:border-indigo-400 focus-visible:outline focus-visible:outline-2 focus-visible:outline-indigo-400 disabled:cursor-not-allowed disabled:opacity-50';

function App() {
  const [authReady, setAuthReady] = useState(false);
  const [authenticated, setAuthenticated] = useState(false);
  const [username, setUsername] = useState('');
  const [role, setRole] = useState<UserRole>('viewer');
  const [loginUser, setLoginUser] = useState('');
  const [loginPassword, setLoginPassword] = useState('');
  const [authError, setAuthError] = useState('');
  const [authBusy, setAuthBusy] = useState(false);
  const [forcePasswordChange, setForcePasswordChange] = useState(false);
  const [showPasswordDialog, setShowPasswordDialog] = useState(false);
  const [currentPassword, setCurrentPassword] = useState('');
  const [newPassword, setNewPassword] = useState('');
  const [passwordStatus, setPasswordStatus] = useState('');
  const [mode, setMode] = useState<WorkspaceMode>('runtime');
  const [runtimePage, setRuntimePage] = useState<RuntimePage>('overview');
  const [modelingPage, setModelingPage] = useState<ModelingPage>('overview');
  const [capabilities, setCapabilities] = useState<WorkspaceCapabilities | null>(null);
  const [capabilityError, setCapabilityError] = useState('');
  const [capabilityRevision, setCapabilityRevision] = useState(0);
  const [capabilityLoading, setCapabilityLoading] = useState(false);
  const [researchContext, setResearchContext] = useState<ResearchWorkspaceContext>({ datasetId: '', studyId: '', runId: '', sourceLabel: '尚未選取資料', versionLabel: '尚未選取研究版本', dirty: false });
  const [runtimeContext, setRuntimeContext] = useState<RuntimeWorkspaceContext>({ targetLabel: '尚未選取控制群組', sourceLabel: '尚未取得現場狀態', versionLabel: '活動版本未知', health: 'unknown', observedAt: null });
  const passwordDialog = useRef<HTMLDialogElement>(null);
  const canResearch = hasRoleAtLeast(role, 'engineer');
  const pages = mode === 'runtime' ? runtimePages : modelingPages;
  const pageId = mode === 'runtime' ? runtimePage : modelingPage;
  const page = pages.find(item => item.id === pageId)!;
  const scene = useWorkspaceScene(username, mode, pageId, page.showScene);

  useEffect(() => {
    let cancelled = false;
    void apiMe().then(me => {
      if (cancelled) return;
      if (me.authenticated && me.username && me.role) {
        setAuthenticated(true); setUsername(me.username); setRole(me.role); setForcePasswordChange(me.force_password_change);
      }
    }).catch(cause => { if (!cancelled) setAuthError(runtimeError(cause)); })
      .finally(() => { if (!cancelled) setAuthReady(true); });
    return () => { cancelled = true; };
  }, []);

  useEffect(() => {
    if (!authenticated) return;
    let cancelled = false;
    async function refreshSession() {
      try {
        const me = await apiMe();
        if (cancelled) return;
        if (!me.authenticated || !me.username || !me.role) {
          setAuthenticated(false); setUsername(''); setRole('viewer'); setCapabilities(null);
          setForcePasswordChange(false); setAuthError('登入已過期。重新登入後可查詢伺服器保存的任務與操作。');
          return;
        }
        setUsername(me.username); setRole(me.role); setForcePasswordChange(me.force_password_change);
      } catch { /* A failed refresh does not claim logout or change controller state. */ }
    }
    const onVisible = () => { if (document.visibilityState === 'visible') void refreshSession(); };
    const timer = window.setInterval(() => void refreshSession(), 5 * 60 * 1000);
    document.addEventListener('visibilitychange', onVisible);
    return () => { cancelled = true; window.clearInterval(timer); document.removeEventListener('visibilitychange', onVisible); };
  }, [authenticated]);

  useEffect(() => {
    if (!authenticated) return;
    const controller = new AbortController();
    setCapabilityLoading(true); setCapabilityError('');
    void workspaceRequest<WorkspaceCapabilities>('/api/workspace', undefined, controller.signal).then(value => {
      if (controller.signal.aborted) return;
      if (typeof value.runtime_enabled !== 'boolean' || typeof value.research_enabled !== 'boolean' || !Array.isArray(value.scopes)
        || !['isolated_research', 'local_test_runtime', 'field_runtime'].includes(value.deployment_kind)
        || !value.capabilities || value.user?.username !== username || !Array.isArray(value.limitations)) throw new Error('部署能力或登入身分不一致，不能啟用操作。');
      setCapabilities(value);
      setMode(current => !value.runtime_enabled && value.research_enabled && canResearch ? 'modeling' : current === 'modeling' && (!value.research_enabled || !canResearch) ? 'runtime' : current);
    }).catch(cause => { if (!controller.signal.aborted) { setCapabilities(null); setCapabilityError(runtimeError(cause)); } })
      .finally(() => { if (!controller.signal.aborted) setCapabilityLoading(false); });
    return () => controller.abort();
  }, [authenticated, username, role, canResearch, capabilityRevision]);

  useEffect(() => {
    const dialog = passwordDialog.current;
    if (authenticated && (showPasswordDialog || forcePasswordChange)) { if (dialog && !dialog.open) dialog.showModal(); }
    else if (dialog?.open) dialog.close();
  }, [authenticated, showPasswordDialog, forcePasswordChange]);

  async function onLogin(event: FormEvent<HTMLFormElement>) {
    event.preventDefault(); if (authBusy) return;
    setAuthBusy(true); setAuthError('');
    try {
      const result = await apiLogin(loginUser.trim(), loginPassword);
      setAuthenticated(true); setUsername(result.username); setRole(result.role);
      setForcePasswordChange(result.force_password_change); setLoginPassword('');
    } catch (cause) { setAuthError(runtimeError(cause)); }
    finally { setAuthBusy(false); }
  }
  async function onLogout() {
    if (researchContext.dirty && !window.confirm('有未儲存的建模草稿。登出會捨棄本分頁草稿；伺服器任務不會停止。仍要登出？')) return;
    try { await apiLogout(); }
    catch (cause) { setCapabilityError(`登出未確認：${runtimeError(cause)}`); return; }
    setAuthenticated(false); setUsername(''); setRole('viewer'); setCapabilities(null);
    setForcePasswordChange(false); setShowPasswordDialog(false); setResearchContext({ datasetId: '', studyId: '', runId: '', sourceLabel: '尚未選取資料', versionLabel: '尚未選取研究版本', dirty: false });
  }
  async function onChangePassword(event: FormEvent<HTMLFormElement>) {
    event.preventDefault(); if (authBusy) return;
    if ((!forcePasswordChange && !currentPassword) || newPassword.length < 8) { setPasswordStatus('請填寫目前密碼，且新密碼至少 8 字元。'); return; }
    setAuthBusy(true); setPasswordStatus('');
    try {
      await apiChangePassword(forcePasswordChange ? '' : currentPassword, newPassword);
      const me = await apiMe();
      if (!me.authenticated || !me.username || !me.role) { setAuthenticated(false); setCapabilities(null); setAuthError('密碼已變更，請重新登入。'); }
      else { setUsername(me.username); setRole(me.role); setForcePasswordChange(me.force_password_change); }
      setShowPasswordDialog(false); setCurrentPassword(''); setNewPassword('');
    } catch (cause) { setPasswordStatus(runtimeError(cause)); }
    finally { setAuthBusy(false); }
  }
  function selectPage(id: string) {
    if (mode === 'runtime' && runtimePages.some(item => item.id === id)) setRuntimePage(id as RuntimePage);
    if (mode === 'modeling' && modelingPages.some(item => item.id === id)) setModelingPage(id as ModelingPage);
  }

  if (!authReady) return <div role="status" className="flex h-dvh items-center justify-center bg-slate-950 text-slate-300">正在確認登入狀態…</div>;
  if (!authenticated) return <main className="flex min-h-dvh items-center justify-center bg-slate-950 p-5 text-slate-100"><form onSubmit={event => void onLogin(event)} className="w-full max-w-md space-y-4 rounded-xl border border-slate-700 bg-slate-900 p-6"><h1 className="text-xl font-semibold">AuTHRity · 水泥磨工作區</h1><p className="text-sm text-slate-400">登入後依部署能力與授權進入 Runtime 或隔離建模。</p><label className="block space-y-1 text-sm">帳號<input className={control} autoComplete="username" value={loginUser} onChange={event => setLoginUser(event.target.value)} required /></label><label className="block space-y-1 text-sm">密碼<input type="password" className={control} autoComplete="current-password" value={loginPassword} onChange={event => setLoginPassword(event.target.value)} required /></label>{authError && <p role="alert" className="break-words text-sm text-rose-300">{authError}</p>}<button type="submit" disabled={authBusy} className={`${button} w-full border-indigo-600 bg-indigo-700`}>{authBusy ? '登入中…' : '登入'}</button></form></main>;

  return <div className="flex h-dvh overflow-hidden bg-slate-950 font-sans text-slate-100">
    <Sidebar mode={mode} pages={pages} activePage={pageId} onSelect={selectPage} username={username} role={role} onLogout={() => void onLogout()} onChangePassword={() => { setPasswordStatus(''); setCurrentPassword(''); setNewPassword(''); setShowPasswordDialog(true); }} />
    <div className="flex min-w-0 flex-1 flex-col">
      <header className="shrink-0 border-b border-slate-700 bg-slate-950 px-3 py-3 sm:px-5">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex flex-wrap items-center gap-2"><strong className="mr-2">水泥磨</strong><button type="button" className={`${button} ${mode === 'runtime' ? 'border-indigo-500 text-indigo-200' : ''}`} aria-pressed={mode === 'runtime'} disabled={!capabilities?.runtime_enabled} onClick={() => setMode('runtime')}>Runtime · {capabilities?.deployment_kind === 'local_test_runtime' ? '隔離測試' : capabilities?.deployment_kind === 'field_runtime' ? '現場' : '來源待核對'}</button><button type="button" className={`${button} ${mode === 'modeling' ? 'border-indigo-500 text-indigo-200' : ''}`} aria-pressed={mode === 'modeling'} disabled={!canResearch || !capabilities?.research_enabled} onClick={() => setMode('modeling')}>建模 · 隔離研究</button></div>
          <span className="rounded border border-amber-800 px-2 py-1 text-xs text-amber-200">{mode === 'modeling' ? '無現場寫入通路' : capabilities?.deployment_kind === 'local_test_runtime' ? '隔離模擬環境 · 不是工廠投運證據' : '控制權由後端獨立確認'}</span>
        </div>
        <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-xs text-slate-400" aria-label="工作區來源與版本"><span>{mode === 'modeling' ? researchContext.sourceLabel : runtimeContext.sourceLabel}</span><span>{mode === 'modeling' ? researchContext.versionLabel : runtimeContext.versionLabel}</span><span>{mode === 'modeling' ? researchContext.dirty ? '有未儲存草稿' : '研究版本／資料明確選取' : runtimeContext.targetLabel}</span>{mode === 'runtime' && <span>{runtimeContext.observedAt ? `狀態確認：${runtimeContext.observedAt}` : '資料時刻未知'} · {runtimeContext.health}</span>}</div>
      </header>
      <main className="min-h-0 min-w-0 flex-1 overflow-y-auto pb-24 md:pb-6" aria-label="水泥磨工作區">
        <div className="flex flex-wrap items-start justify-between gap-3 px-3 pb-2 pt-4 sm:px-5"><div><h1 className="text-lg font-semibold">{page.label}</h1><p className="mt-1 text-xs text-slate-400">{page.purpose}</p></div><div className="flex flex-wrap gap-2"><button type="button" className={button} aria-expanded={scene.visible} onClick={scene.toggle}>{scene.visible ? '收合設備' : '顯示設備'}</button><button type="button" className={button} onClick={scene.restore}>恢復本頁預設</button></div></div>
        {capabilityLoading && <p role="status" className="px-5 py-2 text-sm text-slate-400">正在讀取部署能力…</p>}
        {capabilityError && <div role="alert" className="m-3 space-y-2 rounded border border-rose-800 p-3 text-sm text-rose-200"><p>{capabilityError}</p><button type="button" className={button} disabled={capabilityLoading} onClick={() => setCapabilityRevision(value => value + 1)}>重新查詢部署能力</button></div>}
        {capabilities && <>
          <div hidden={mode !== 'runtime'} inert={mode !== 'runtime'} style={mode !== 'runtime' ? { display: 'none' } : undefined}>
            <RuntimeWorkspace section={runtimePage} active={mode === 'runtime'} sceneVisible={mode === 'runtime' && scene.visible} capabilities={capabilities} onContextChange={setRuntimeContext} onNavigate={setRuntimePage} />
          </div>
          {canResearch && capabilities.research_enabled && <div hidden={mode !== 'modeling'} inert={mode !== 'modeling'} style={mode !== 'modeling' ? { display: 'none' } : undefined}><Autoresearch section={modelingPage} onSectionChange={setModelingPage} sceneVisible={mode === 'modeling' && scene.visible} active={mode === 'modeling'} onContextChange={setResearchContext} /></div>}
          {!capabilities.runtime_enabled && (!canResearch || !capabilities.research_enabled) && <p className="m-3 rounded border border-amber-700 p-4 text-sm text-amber-200">此部署未啟用現場 Runtime；目前帳號亦無可用的建模工作區。請由管理員確認部署能力與角色，不會自動建立現場連線。</p>}
        </>}
      </main>
    </div>
    <dialog ref={passwordDialog} onCancel={event => { if (forcePasswordChange) event.preventDefault(); else setShowPasswordDialog(false); }} className="m-auto w-[min(28rem,calc(100vw-2rem))] rounded-xl border border-slate-700 bg-slate-900 p-6 text-slate-100 backdrop:bg-black/70"><form onSubmit={event => void onChangePassword(event)} className="space-y-4"><h2 className="text-lg font-semibold">{forcePasswordChange ? '首次登入：請設定新密碼' : '更改密碼'}</h2>{!forcePasswordChange && <label className="block text-sm">目前密碼<input type="password" autoComplete="current-password" className={control} value={currentPassword} onChange={event => setCurrentPassword(event.target.value)} required /></label>}<label className="block text-sm">新密碼（至少 8 字元）<input type="password" autoComplete="new-password" className={control} value={newPassword} onChange={event => setNewPassword(event.target.value)} minLength={8} required /></label>{passwordStatus && <p role="alert" className="break-words text-sm text-amber-200">{passwordStatus}</p>}<div className="flex gap-2">{!forcePasswordChange && <button type="button" className={button} onClick={() => setShowPasswordDialog(false)}>取消</button>}<button className={`${button} flex-1 bg-indigo-700`} type="submit" disabled={authBusy}>{authBusy ? '更新中…' : '更新密碼'}</button></div></form></dialog>
  </div>;
}

export default App;
