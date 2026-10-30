import { useState } from 'react';
import { researchError, researchRequest } from '../lib/research';
import { researchSelectionIssue } from '../lib/researchSelection';
import type { ResearchRun } from '../types/Research';
import { ResearchField, ResearchNotice, ResearchPanel, researchButton, researchControl, researchPrimary } from './ResearchControls';

export function ResearchTrialSelection({ run }: { run: ResearchRun }) {
  const [selections, setSelections] = useState<Record<string, string>>({});
  const [reason, setReason] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [submitted, setSubmitted] = useState(false);
  const development = run.development;
  const methods = [...new Set(run.trials.map(item => item.method))];
  const awaiting = run.stage === 'awaiting_selection' && run.status === 'running' && !run.results && !submitted;
  async function freeze() {
    const issue = researchSelectionIssue(run, selections, reason);
    if (!awaiting || issue) { setError(issue || '選擇已提交。'); return; }
    setBusy(true); setError('');
    try {
      await researchRequest(`/runs/${encodeURIComponent(run.id)}/selection`, { selections, reason: reason.trim() });
      setSubmitted(true);
    } catch (failure) { setError(`選擇結果未確認，請更新原 run 狀態後再操作：${researchError(failure)}`); }
    finally { setBusy(false); }
  }
  return <ResearchPanel title="開發情境折衷與候選凍結">
    {!development ? <p className="text-sm text-slate-400">伺服器尚未回傳開發情境多目標證據，不能由最終測試結果建立折衷排名。</p> : <>
      <p className="mb-3 text-xs text-slate-400">{development.scope}</p>
      <div className="overflow-x-auto"><table className="w-full text-left text-xs"><caption className="sr-only">伺服器回報的多目標向量與非支配集合</caption><thead><tr><th className="p-2">Trial</th><th className="p-2">資格／非支配</th>{development.objectives.map(label => <th className="p-2" key={label}>{label}</th>)}</tr></thead><tbody>{run.trials.map(trial => <tr key={trial.id} className="border-t border-slate-700"><td className="p-2">{trial.id}{trial.round === 0 && ' · 基準'}</td><td className="p-2">{trial.eligible === true ? development.nondominated_trial_ids.includes(trial.id) ? '合格／非支配' : '合格' : trial.valid ? '不合格／未評估' : '失敗'}</td>{development.objectives.map((label, index) => <td key={label} className="p-2">{Number.isFinite(development.vectors[trial.id]?.[index]) ? development.vectors[trial.id][index].toPrecision(5) : '未記錄'}</td>)}</tr>)}</tbody></table></div>
    </>}
    {error && <ResearchNotice error={error} />}
    {awaiting && <form className="mt-4 space-y-3" onSubmit={event => { event.preventDefault(); void freeze(); }}><fieldset disabled={busy} className="space-y-3">
      {methods.map(method => <ResearchField key={method} label={`${method} 最終留出前選擇`}><select required className={researchControl} value={selections[method] || ''} onChange={event => setSelections({ ...selections, [method]: event.target.value })}><option value="">選擇候選或保留基準</option>{run.trials.filter(item => item.method === method && item.valid && (item.round === 0 || item.eligible === true)).map(item => <option key={item.id} value={item.id}>{item.id} · {item.round === 0 ? '保留基準' : '開發候選'}</option>)}</select></ResearchField>)}
      <ResearchField label="選擇理由（開封前凍結）"><textarea required maxLength={2000} rows={3} className={researchControl} value={reason} onChange={event => setReason(event.target.value)} /></ResearchField>
      <button type="submit" className={researchPrimary} disabled={!!researchSelectionIssue(run, selections, reason)}>{busy ? '提交中…' : '凍結選擇並開封最終驗證'}</button>
    </fieldset></form>}
    {submitted && <p role="status" className="mt-3 text-sm text-indigo-200">選擇已提交；等待 run 回報最終資格進度。不可根據留出結果改選下一名。</p>}
    {!awaiting && !submitted && <p className="mt-3 text-xs text-slate-400">{run.results ? '此 run 已開封最終結果，選擇鎖定。' : '等待伺服器完成開發試驗；自動選擇模式不提供手動重選。'}</p>}
    <details className="mt-3"><summary className={`${researchButton} cursor-pointer`}>閱讀折衷方式</summary><p className="mt-2 text-xs text-slate-400">只比較相同 plant、固定目標及開發情境的真實閉環向量。硬限不由低分抵銷；基準可勝出。留出失敗需另建研究與未使用保留證據。</p></details>
  </ResearchPanel>;
}
