import type { ReactNode } from 'react';
import type { ResearchValue } from '../types/Research';

export const researchControl = 'mt-1 min-h-11 w-full min-w-0 rounded border border-slate-600 bg-slate-950 px-3 py-2 text-base text-slate-100 focus:outline-none focus:ring-2 focus:ring-indigo-400 disabled:opacity-50';
export const researchButton = 'min-h-11 rounded border border-slate-600 bg-slate-800 px-4 py-2 text-sm font-medium text-slate-100 hover:bg-slate-700 focus-visible:outline focus-visible:outline-2 focus-visible:outline-indigo-400 disabled:cursor-not-allowed disabled:opacity-40';
export const researchPrimary = `${researchButton} border-indigo-500 bg-indigo-600 hover:bg-indigo-500`;
export function ResearchPanel({ title, children }: { title: string; children: ReactNode }) {
  return <section className="research-panel min-w-0 border-t border-slate-700 py-5"><h2 className="mb-4 text-lg font-semibold text-slate-100">{title}</h2>{children}</section>;
}
export function ResearchField({ label, children }: { label: string; children: ReactNode }) {
  return <label className="block min-w-0 text-sm text-slate-300"><span>{label}</span>{children}</label>;
}
export function ResearchNumber({ label, value, onChange, min, max, step = 'any', required = true }: { label: string; value: number | undefined; onChange: (value: number | undefined) => void; min?: number; max?: number; step?: number | 'any'; required?: boolean }) {
  return <ResearchField label={label}><input className={researchControl} type="number" value={value === undefined || !Number.isFinite(value) ? '' : value} min={min} max={max} step={step} required={required} onChange={event => onChange(event.target.value === '' ? undefined : event.target.valueAsNumber)} /></ResearchField>;
}
export function ResearchNotice({ error }: { error: string }) {
  return <div role="alert" className="break-words rounded border border-rose-800 bg-rose-950/40 p-4 text-sm text-rose-200"><p>{error.length > 180 ? `${error.slice(0, 160)}…` : error}</p>{error.length > 180 && <details className="mt-3"><summary>完整錯誤與定位資訊</summary><p className="mt-2 whitespace-pre-wrap">{error}</p></details>}</div>;
}
export function ResearchDetails({ value }: { value: ResearchValue | undefined }) {
  if (value == null) return <span className="text-slate-400">未記錄</span>;
  if (typeof value !== 'object') return <span className="break-words">{typeof value === 'number' ? value.toLocaleString(undefined, { maximumFractionDigits: 5 }) : String(value)}</span>;
  const entries = Array.isArray(value) ? value.map((item, index) => [String(index + 1), item] as const) : Object.entries(value);
  return <dl className="space-y-2 text-sm">{entries.map(([key, item]) => <div key={key} className="border-l border-slate-700 pl-3"><dt className="text-slate-400">{key.replaceAll('_', ' ')}</dt><dd>{typeof item === 'object' && item !== null ? <details><summary className="cursor-pointer text-indigo-300">查看內容</summary><ResearchDetails value={item} /></details> : <ResearchDetails value={item} />}</dd></div>)}</dl>;
}
