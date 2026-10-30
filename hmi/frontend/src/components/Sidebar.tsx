import { useEffect, useRef } from 'react';
import { Activity, Menu, X } from 'lucide-react';
import type { WorkspaceMode, WorkspacePage } from '../types/Workspace';

interface SidebarProps {
  mode: WorkspaceMode;
  pages: readonly WorkspacePage[];
  activePage: string;
  onSelect: (page: string) => void;
  username: string;
  role: string;
  onChangePassword: () => void;
  onLogout: () => void;
}

export function Sidebar({ mode, pages, activePage, onSelect, username, role, onChangePassword, onLogout }: SidebarProps) {
  const drawer = useRef<HTMLDialogElement>(null);
  const opener = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    const media = window.matchMedia('(min-width: 768px)');
    const closeOnDesktop = () => { if (media.matches) drawer.current?.close(); };
    media.addEventListener('change', closeOnDesktop);
    return () => media.removeEventListener('change', closeOnDesktop);
  }, []);
  function close() { drawer.current?.close(); opener.current?.focus(); }
  function select(page: string) { onSelect(page); close(); }
  const contents = <>
    <div className="flex items-center gap-3 border-b border-slate-800 px-4 py-4"><Activity size={22} className="text-indigo-400" /><span className="text-lg font-bold">AuTHRity</span><span className="ml-auto text-xs text-slate-400">水泥磨</span></div>
    <div className="px-4 pb-2 pt-4 text-xs font-semibold text-slate-400">{mode === 'runtime' ? 'RUNTIME · 現場運行' : 'MODELING · 隔離建模'}</div>
    <nav className="min-h-0 flex-1 space-y-1 overflow-y-auto px-2 pb-4" aria-label={mode === 'runtime' ? 'Runtime 功能頁面' : '建模功能頁面'}>
      {pages.map((page, index) => <button type="button" key={page.id} aria-current={activePage === page.id ? 'page' : undefined} onClick={() => select(page.id)} className={`flex w-full items-start gap-2 rounded-lg border px-3 py-3 text-left text-sm focus-visible:outline focus-visible:outline-2 focus-visible:outline-indigo-400 ${activePage === page.id ? 'border-indigo-700 bg-indigo-950/60 text-indigo-100' : 'border-transparent text-slate-300 hover:border-slate-700 hover:bg-slate-800'}`}><span className="mt-0.5 w-5 shrink-0 text-xs text-slate-500">{mode === 'runtime' ? 'R' : 'M'}{index + 1}</span><span>{page.label}</span></button>)}
    </nav>
    <footer className="space-y-3 border-t border-slate-800 p-4 text-xs">
      <div className="flex items-center justify-between gap-2"><span className="truncate text-slate-200">{username}</span><span className="text-indigo-300">{role}</span></div>
      <div className="flex gap-2"><button type="button" className="min-h-10 flex-1 rounded border border-slate-700 px-2 hover:border-indigo-400" onClick={() => { close(); onChangePassword(); }}>更改密碼</button><button type="button" className="min-h-10 flex-1 rounded border border-slate-700 px-2 hover:border-indigo-400" onClick={() => { close(); onLogout(); }}>登出</button></div>
      <p className="text-slate-500">換頁不啟停控制；模式與資料品質以頁首狀態為準。</p>
    </footer>
  </>;
  return <>
    <aside className="hidden h-dvh w-60 shrink-0 flex-col border-r border-slate-800 bg-slate-900 md:flex">{contents}</aside>
    <button ref={opener} type="button" aria-label="開啟功能導覽" aria-haspopup="dialog" onClick={() => drawer.current?.showModal()} className="fixed bottom-4 left-4 z-30 flex min-h-11 items-center gap-2 rounded-lg border border-indigo-400 bg-indigo-700 px-4 py-3 text-sm text-white shadow-lg md:hidden"><Menu size={18} />功能</button>
    <dialog ref={drawer} aria-label="水泥磨功能導覽" className="fixed inset-y-0 left-0 m-0 h-dvh max-h-none w-72 max-w-[90vw] border-r border-slate-700 bg-slate-900 p-0 text-slate-100 backdrop:bg-black/70" onClick={event => { if (event.target === event.currentTarget && event.clientX > event.currentTarget.getBoundingClientRect().right) close(); }}>
      <div className="flex h-full flex-col"><button type="button" aria-label="關閉功能導覽" onClick={close} className="m-2 flex min-h-10 items-center justify-end gap-2 rounded px-3 text-sm text-slate-300"><X size={18} />關閉</button>{contents}</div>
    </dialog>
  </>;
}
