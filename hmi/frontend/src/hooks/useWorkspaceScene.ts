import { useState, useSyncExternalStore } from 'react';
import type { WorkspaceMode } from '../types/Workspace';

const storageKey = 'authrity.workspace.scene.v1';
function subscribe(notify: () => void) {
  const query = window.matchMedia('(max-width: 767px)');
  query.addEventListener('change', notify);
  return () => query.removeEventListener('change', notify);
}
const smallScreen = () => window.matchMedia('(max-width: 767px)').matches;
function load(): Record<string, boolean> {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(storageKey) || '{}');
    return value && typeof value === 'object' && !Array.isArray(value)
      ? Object.fromEntries(Object.entries(value).filter(([, enabled]) => typeof enabled === 'boolean')) : {};
  } catch { return {}; }
}

export function useWorkspaceScene(user: string, mode: WorkspaceMode, page: string, defaultVisible: boolean) {
  const mobile = useSyncExternalStore(subscribe, smallScreen, () => true);
  const [preferences, setPreferences] = useState(load);
  const key = JSON.stringify([user, mode, page, mobile ? 'mobile' : 'desktop']);
  const visible = preferences[key] ?? (!mobile && defaultVisible);
  function save(next: Record<string, boolean>) {
    setPreferences(next);
    try { localStorage.setItem(storageKey, JSON.stringify(next)); } catch { /* Display-only preference remains in memory. */ }
  }
  return {
    visible,
    toggle: () => save({ ...preferences, [key]: !visible }),
    restore: () => { const next = { ...preferences }; delete next[key]; save(next); },
  };
}
