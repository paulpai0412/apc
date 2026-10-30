import { useState } from 'react';
import type { TwinManifest, TwinSession, TwinTrial, TwinVariableFocus } from '../types/DigitalTwin';
import { twinQuality } from '../lib/digitalTwin';
import { ResearchDetails, ResearchField, ResearchPanel, researchControl } from './ResearchControls';

const number = (value: number | undefined) => value === undefined || !Number.isFinite(value) ? '—' : value.toLocaleString(undefined, { maximumSignificantDigits: 7 });

export function DigitalTwinIdentity({ session }: { session: TwinSession }) {
  return <details className="min-w-0 rounded-lg border border-slate-700 p-3">
    <summary className="cursor-pointer text-sm text-indigo-300">不可變模型與情境 SHA-256</summary>
    <dl className="mt-3 grid min-w-0 gap-3 text-xs sm:grid-cols-2">{([
      ['控制器', session.controller_sha256], ['被控模型', session.plant_sha256], ['情境', session.scenario_sha256], ['執行檔', session.binary_sha256],
      ['工作階段', session.id], ['建立時間', session.created_at], ['情境起點', session.start], ['試驗', session.trial_id],
      ['時間基準', session.time_basis], ['原始資料偏移 (s)', session.source_offset_seconds === undefined ? '未記錄' : String(session.source_offset_seconds)],
    ] as const).map(([label, value]) => <div key={label} className="min-w-0"><dt className="text-slate-400">{label}</dt><dd className="break-all font-mono text-slate-200">{value || '未記錄'}</dd></div>)}</dl>
  </details>;
}

export function DigitalTwinModel({ manifest, trial }: { manifest: TwinManifest; trial?: TwinTrial }) {
  const inputs = [...manifest.variables.mv.map(variable => ({ ...variable, role: 'MV' })), ...manifest.variables.dv.map(variable => ({ ...variable, role: 'DV' }))];
  return <ResearchPanel title="模型與試驗來源">
    <div className="space-y-3">
      <div className="flex flex-wrap gap-2 text-xs"><span className="rounded bg-amber-950 px-2 py-1 text-amber-200">隔離模擬 · 非工廠實測</span><span className="rounded bg-slate-800 px-2 py-1 text-slate-300">{manifest.scope}</span><span className="rounded bg-slate-800 px-2 py-1 text-slate-300">Ts {manifest.sample_time} s</span></div>
      <p className="text-xs text-slate-400">{manifest.scope === 'identified_history' ? '歷史資料辨識模型：可信範圍以持久化的資格證據為準，模擬成功不等於現場驗證。' : manifest.scope === 'synthetic' ? '合成模型與情境：可驗證演算法行為，不提供現場設備的實證可信度。' : '來源範圍以上方紀錄為準；未明示的現場可信度不作推論。'}</p>
      {trial && <div className="rounded-lg border border-slate-700 p-3 text-sm"><div className="flex flex-wrap justify-between gap-2"><strong className="break-all text-slate-100">{trial.method} · {trial.id} · 第 {trial.round} 輪</strong><span className={trial.eligible === true && !trial.diagnostic_only ? 'text-emerald-300' : 'text-amber-200'}>{trial.eligible === true && !trial.diagnostic_only ? '符合選參資格 · 仍非部署核准' : '僅供診斷 · 不具升版資格'}</span></div>{trial.reason && <p className="mt-2 break-words text-slate-400">{trial.reason}</p>}<details className="mt-3"><summary className="cursor-pointer text-indigo-300">試驗參數與控制器識別</summary><div className="mt-3"><ResearchDetails value={trial.parameters} /></div><p className="mt-3 break-all font-mono text-xs text-slate-400">SHA-256: {trial.controller_sha256 || '未記錄'}</p></details></div>}
      <details className="rounded-lg border border-slate-700 p-3"><summary className="cursor-pointer text-sm text-indigo-300">SOPDT 被控模型路徑 · FIR 控制器區別</summary>
        <p className="my-3 text-xs text-slate-400">被控模型以 SOPDT 路徑（增益、兩個時間常數、延遲）推進；控制器以學得的 FIR／步階係數預測。兩者不是同一組模型，也不代表工廠真實物理。</p>
        <div className="max-w-full overflow-x-auto"><table className="w-full text-left text-xs"><caption className="sr-only">SOPDT 模型路徑參數</caption><thead className="text-slate-400"><tr>{['輸入 → 輸出', 'Gain', 'τ1 (s)', 'τ2 (s)', 'Delay (s)'].map(label => <th key={label} className="whitespace-nowrap p-2">{label}</th>)}</tr></thead><tbody>{manifest.plant_paths.flatMap((row, output) => row.map((path, input) => <tr key={`${output}:${input}`} className="border-t border-slate-800 text-slate-200"><th className="p-2 font-normal">{inputs[input]?.role} {inputs[input]?.name ?? `#${input + 1}`} → {manifest.variables.cv[output]?.name ?? `CV #${output + 1}`}</th><td className="p-2">{number(path.gain)}</td><td className="p-2">{number(path.tau1)}</td><td className="p-2">{number(path.tau2)}</td><td className="p-2">{number(path.delay_seconds)}</td></tr>))}</tbody></table></div>
        {!manifest.plant_paths.length && <p className="text-sm text-amber-200">尚無持久化的路徑模型。</p>}
      </details>
      <div className="grid gap-3 sm:grid-cols-3">{([
        ['來源', manifest.plant_source], ['辨識紀錄', manifest.identification], ['模型資格證據', manifest.qualification],
      ] as const).map(([label, value]) => <details key={label} className="min-w-0 rounded-lg border border-slate-700 p-3"><summary className="cursor-pointer text-sm text-indigo-300">{label}</summary><div className="mt-3"><ResearchDetails value={value} /></div></details>)}</div>
    </div>
  </ResearchPanel>;
}

export function DigitalTwinProcess({ manifest, session, advancing, onFocus }: { manifest: TwinManifest; session: TwinSession; advancing: boolean; onFocus: (variable: TwinVariableFocus) => void }) {
  const [selected, setSelected] = useState({ cv: 0, mv: 0, dv: 0 });
  const frame = session.frame;
  const frozenVariables = session.variables ?? manifest.variables;
  function card(role: 'cv' | 'mv' | 'dv') {
    const variables = frozenVariables[role];
    if (!variables.length) return <div className="rounded-lg border border-dashed border-slate-700 p-3 text-sm text-slate-500">未配置 {role.toUpperCase()}</div>;
    const index = selected[role] < variables.length ? selected[role] : 0;
    const variable = variables[index];
    const offset = role === 'cv' ? 0 : frozenVariables.cv.length + (role === 'dv' ? frozenVariables.mv.length : 0);
    const quality = frame ? twinQuality(frame.quality[offset + index]) : 'unknown';
    return <div className="min-w-0 rounded-lg border border-slate-700 bg-slate-950 p-3"><ResearchField label={`${role.toUpperCase()} 顯示變數`}><select className={researchControl} value={index} onChange={event => setSelected(current => ({ ...current, [role]: Number(event.target.value) }))}>{variables.map((item, i) => <option value={i} key={i}>{item.name} [{item.unit}]</option>)}</select></ResearchField><button type="button" aria-label={`聚焦 ${role.toUpperCase()} ${variable.name} 曲線`} className="mt-3 w-full rounded border border-transparent p-2 text-left hover:border-indigo-500 focus-visible:outline focus-visible:outline-indigo-400" onClick={() => onFocus({ role: role.toUpperCase() as TwinVariableFocus['role'], name: variable.name, unit: variable.unit })}><span className="block break-words text-2xl tabular-nums text-slate-100">{number(frame?.[role][index])} <span className="text-xs text-slate-400">{variable.unit}</span></span><span className="mt-1 block text-xs text-slate-400">品質 {quality}{frame ? ` · t = ${number(frame.time_seconds)} s` : ' · 尚無完成掃描'}</span><span className="mt-2 block text-xs text-indigo-300">點選聚焦曲線</span></button>{role === 'cv' && frame && <p className="mt-2 text-xs text-slate-400">目標 {number(frame.targets[index])} · 一步預測 {number(frame.predicted_cv[index])}（t = {number(frame.time_seconds + session.sample_time)} s）</p>}{role === 'mv' && frame && <p className="mt-2 text-xs text-slate-400">施加指令 {number(frame.command_mv[index])}</p>}</div>;
  }
  return <ResearchPanel title="模擬程序與變數">
    <p className="mb-3 text-xs text-slate-400">MV / DV → 可配置被控模型 → CV。這是模型拓撲，不是工廠 P&amp;ID；數值僅取自伺服器完成的掃描。</p>
    <div className="grid min-w-0 items-center gap-3 md:grid-cols-3"><div className="space-y-3">{card('mv')}{card('dv')}</div><div className="rounded-xl border border-indigo-700 bg-indigo-950/30 p-5 text-center"><p className="text-xs text-indigo-300">MV + DV →</p><h3 className="my-2 font-semibold text-indigo-100">SOPDT 被控模型</h3><p role="status" className="text-sm text-slate-300">{advancing && frame ? '正在推進 · 已收到實際掃描' : '靜止 · 無推進動畫'}</p><p className="mt-2 text-xs text-slate-400">→ CV</p></div>{card('cv')}</div>
    <div className="mt-3 rounded-lg border border-indigo-900 bg-indigo-950/20 p-3 text-center text-xs text-indigo-200">CV 回授 + 目標 → 凍結 FIR MPC 控制器 → MV 施加指令<br /><span className="text-slate-400">指令與實際 MV 分開顯示；DV 是外生擾動，不由 MPC 操作。</span></div>
    <p className="mt-3 text-xs text-slate-400">{frame ? `最近完成掃描 t = ${number(frame.time_seconds)} s · ${frame.solver_status}` : '尚未執行掃描；快照不會推進模型。'} · 情境起點 {session.start}</p>
  </ResearchPanel>;
}
