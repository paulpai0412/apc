import { useState } from 'react';
import { Bar, BarChart, CartesianGrid, Legend, Line, LineChart, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts';
import type { Metrics, ResearchRun } from '../types/Research';
import { ResearchTrendComparison } from './ResearchTrendComparison';
import { ResearchDetails, ResearchField, ResearchNotice, ResearchPanel, researchButton, researchControl } from './ResearchControls';
import { ResearchOutcomeSummary } from './ResearchOutcomeSummary';
import { ResearchVersionSelection } from './ResearchVersionSelection';
import { ResearchTrialSelection } from './ResearchTrialSelection';

const colors = ['#a5b4fc', '#34d399', '#fbbf24', '#f472b6', '#38bdf8'];
const tooltipStyle = { background: '#0f172a', borderColor: '#475569', color: '#e2e8f0' };
const statusLabels: Record<ResearchRun['status'], string> = { queued: '已排入佇列', running: '執行中', cancel_requested: '已要求取消，正在安全停止', cancelled: '已取消', failed: '執行失敗', completed: '已完成' };

export function ResearchResults({ run, onCancel, busy, section }: { run: ResearchRun; onCancel: () => void; busy: boolean; section: 'tuning' | 'candidates' }) {
  const [comparisonMetric, setComparisonMetric] = useState('score');
  const trials = run.trials || [];
  const scoreTraces = Array.from(new Set(trials.map(trial => trial.method))).map(method => ({
    method,
    points: trials.filter(trial => trial.method === method).map((trial, evaluation) => ({
      evaluation, trial: trial.id, round: trial.round,
      score: trial.valid && trial.score !== null && Number.isFinite(trial.score) ? trial.score : null,
    })),
  }));
  const scores = scoreTraces.flatMap(item => item.points);
  const metricOptions = [
    { id: 'score', label: '固定綜合評分（無因次）' },
    ...(run.results?.variables.cv.flatMap((item, i) => [
      { id: `cv_variance:${i}`, label: `${item.name} · 實體 CV 變異 (${item.unit || '單位未知'})²` },
      { id: `cv_tracking_error_variance:${i}`, label: `${item.name} · 目標誤差變異 (${item.unit || '單位未知'})²` },
      { id: `cv_tracking_bias:${i}`, label: `${item.name} · 追蹤偏差 (${item.unit || '單位未知'})` },
      { id: `cv_spec_violation_seconds:${i}`, label: `${item.name} · 品質規格違限時間（秒）` },
      { id: `cv_rmse:${i}`, label: `${item.name} · RMSE (${item.unit})` },
      { id: `cv_violation_seconds:${i}`, label: `${item.name} · 操作違限時間（秒）` },
    ]) || []),
    ...(run.results?.variables.mv.map((item, i) => ({ id: `mv_total_variation:${i}`, label: `${item.name} · 累積命令動作量 (${item.unit})` })) || []),
  ];
  const metricValue = (metrics: Metrics) => {
    const [field, column] = comparisonMetric.split(':');
    const values = metrics[field];
    const value = column === undefined ? values : Array.isArray(values) ? values[Number(column)] : null;
    return typeof value === 'number' && Number.isFinite(value) ? value : null;
  };
  const metricRows = run.results?.methods.map(method => ({
    method: method.method, baseline: metricValue(method.baseline.test),
    optimized: metricValue(method.optimized.test), transport: metricValue(method.transport.test),
  })) || [];
  return <div className="space-y-3">
    <div className="flex flex-wrap items-center justify-between gap-2 text-xs">
      <p role="status" aria-live="polite" className={run.status === 'failed' ? 'text-rose-300' : run.status === 'completed' ? 'text-emerald-300' : 'text-indigo-300'}>{statusLabels[run.status]} · 工作 {run.progress?.completed ?? '未記錄'} / {run.progress?.total ?? '未記錄'}{run.status !== 'completed' && ` · ${run.stage || '等待工作程序'}`}</p>
      <details className="min-w-0 text-slate-400"><summary className="cursor-pointer text-indigo-300">執行身分與進度</summary><p className="mt-2 break-all">{run.name || run.id} · Run {run.id}<br />{run.created_at} · {run.stage || '等待工作程序'}</p></details>
      {['queued', 'running', 'cancel_requested'].includes(run.status) && <button type="button" className={researchButton} disabled={busy || run.status === 'cancel_requested'} onClick={onCancel}>取消研究</button>}
    </div>
    {run.error && <ResearchNotice error={run.error} />}
    <div hidden={section !== 'candidates'}>
    <div id="research-summary"><ResearchOutcomeSummary run={run} /></div>
    {run.policy_candidate && <ResearchPanel title="政策候選封存結果">
      {run.policy_candidate.run_id !== run.id || run.policy_candidate.authority !== 'review_only' ? <ResearchNotice error="政策候選身分或權威不相符，不能推定此 run 已產生可送審候選。" /> : <div className="space-y-2 text-sm">
        <p>{({ sealed: '已封存審查候選（未核准、未安裝）', retain_baseline: '保留基準，不強迫產生新版本', manual_only_unbound: '未綁定發布範本，只能人工選版', failed_qualification: '未通過候選資格', run_not_completed: '研究尚未完整完成' } as const)[run.policy_candidate.status]}</p>
        {run.policy_candidate.candidate_id && <p className="break-all text-xs">候選 ID：{run.policy_candidate.candidate_id}</p>}
        {run.policy_candidate.reason && <p className="text-amber-200">{run.policy_candidate.reason}</p>}
        <p className="break-all text-xs text-slate-400">政策：{run.policy_candidate.policy_id} · 僅 review_only；Runtime 獨立核准與交接權限不由研究取得。</p>
      </div>}
    </ResearchPanel>}
    {run.results && <>
      <div id="research-trends"><ResearchTrendComparison key={run.id} run={run} /></div>
      <ResearchPanel title="更多指標比較">
        <details><summary className="mb-3 cursor-pointer text-sm text-indigo-300">查看單一指標比較圖與測試區間</summary>
        <p className="mb-3 text-sm text-amber-200">{run.results.scope}</p>
        <div className="mb-4"><ResearchField label="凍結後測試比較指標"><select className={researchControl} value={comparisonMetric} onChange={event => setComparisonMetric(event.target.value)}>{metricOptions.map(option => <option key={option.id} value={option.id}>{option.label}</option>)}</select></ResearchField><p className="mt-2 text-xs text-slate-400">以下是凍結後驗證，不可用來重挑勝者。是否仍具獨立留出資格以證據記錄為準；命令動作量不是能耗。</p></div>
        {metricRows.length > 0 && <div className="h-72" role="img" aria-label={`凍結後測試比較：${metricOptions.find(option => option.id === comparisonMetric)?.label}`}><ResponsiveContainer width="100%" height="100%"><BarChart data={metricRows}><CartesianGrid stroke="#334155" strokeDasharray="3 3" /><XAxis dataKey="method" tick={{ fill: '#94a3b8', fontSize: 11 }} /><YAxis tick={{ fill: '#94a3b8', fontSize: 11 }} /><Tooltip contentStyle={tooltipStyle} /><Legend /><Bar dataKey="baseline" name="基準 native" fill="#94a3b8" isAnimationActive={false} /><Bar dataKey="optimized" name="凍結候選 native" fill="#a5b4fc" isAnimationActive={false} /><Bar dataKey="transport" name="封存傳輸試驗（見檢查證據）" fill="#34d399" isAnimationActive={false} /></BarChart></ResponsiveContainer></div>}
        <div className="mt-4 flex flex-wrap gap-3">{run.results.splits.map(split => <div key={split.name} className="rounded border border-slate-700 p-3 text-xs"><strong>{split.name}</strong><p>{split.start} → {split.end}</p><p>{split.samples.toLocaleString()} 筆 · 終點不含</p></div>)}</div>
        </details>
      </ResearchPanel>
      <ResearchPanel title="辨識證據"><details><summary className="cursor-pointer text-sm text-indigo-300">查看模型辨識與留出驗證資料</summary><div className="mt-3"><ResearchDetails value={run.results.identification} /></div></details></ResearchPanel>
    </>}
    </div>
    <div id="research-trials" hidden={section !== 'tuning'}>
    <ResearchTrialSelection key={run.id} run={run} />
    <ResearchPanel title="自適應試驗 · 即時驗證分數">
      <details open={['queued', 'running', 'cancel_requested'].includes(run.status)}>
      <summary className="mb-3 cursor-pointer text-sm text-indigo-300">查看 {trials.length} 筆試驗、分數與拒絕原因</summary>
      <p className="mb-3 text-sm text-slate-400">分數只供排序；候選還須通過所有情境的變異、偏差、規格與 MV 動作門檻。</p>
      {scores.some(point => point.score !== null) ? <div className="h-64" role="img" aria-label="各控制方法已完成試驗的實際驗證分數，橫軸為方法內試驗序號"><ResponsiveContainer width="100%" height="100%"><LineChart data={scores}><CartesianGrid stroke="#334155" strokeDasharray="3 3" /><XAxis dataKey="evaluation" type="number" allowDecimals={false} domain={['dataMin', 'dataMax']} tick={{ fill: '#94a3b8', fontSize: 11 }} /><YAxis tick={{ fill: '#94a3b8', fontSize: 11 }} domain={['auto', 'auto']} /><Tooltip contentStyle={tooltipStyle} /><Legend />{scoreTraces.map((item, index) => <Line key={item.method} data={item.points} dataKey="score" name={`${item.method} · 驗證分數（越低越好）`} stroke={colors[index % colors.length]} dot={{ r: 3 }} connectNulls={false} isAnimationActive={false} />)}</LineChart></ResponsiveContainer></div> : <p className="text-sm text-slate-400">尚無已完成的有效試驗。工作程序回傳分數後會自動顯示，未使用模擬進度或預設結果。</p>}
      {trials.length > 0 && <div className="mt-4 overflow-x-auto">
        <table className="w-full text-left text-sm">
          <caption className="sr-only">實際試驗紀錄（失敗與未通過門檻的候選也保留）</caption>
          <thead className="text-slate-400"><tr>{['試驗 / 父候選', '方法', '輪次', '門檻評估', '分數', '詳細資料'].map(label => <th key={label} className="p-2">{label}</th>)}</tr></thead>
          <tbody>{trials.map(trial => <tr key={trial.id} className="border-t border-slate-800 align-top">
            <td className="p-2">{trial.id}<span className="block text-xs text-slate-500">{trial.parent_trial}</span></td>
            <td className="p-2">{trial.method}</td><td className="p-2">{trial.round}</td>
            <td className={`whitespace-nowrap p-2 ${!trial.valid ? 'text-rose-300' : trial.eligible ? 'text-emerald-300' : 'text-amber-300'}`}>
              {!trial.valid ? '執行失敗' : trial.eligible === true ? trial.round === 0 ? '基準通過' : '通過門檻' : trial.eligible === false ? '未過門檻' : '門檻未評估'}
            </td>
            <td className="p-2">{trial.score == null || !Number.isFinite(trial.score) ? '未記錄' : trial.score.toPrecision(6)}</td>
            <td className="min-w-48 p-2"><details>
              <summary className="cursor-pointer text-indigo-300">{trial.reason ? '查看拒絕原因與參數' : '查看參數'}</summary>
              {trial.reason && <p className="my-2 text-amber-200">{trial.reason}</p>}
              <ResearchDetails value={trial.parameters} />
              <details className="mt-3"><summary className="cursor-pointer text-indigo-300">實際完整 tuning／控制策略／各 CV 與 MV 參數</summary>{trial.effective_tuning ? <ResearchDetails value={trial.effective_tuning} /> : <p className="mt-2 text-xs text-slate-400">伺服器未提供此 trial 的完整有效參數；上方搜尋倍率不等同完整控制器設定。</p>}</details>
            </details></td>
          </tr>)}</tbody>
        </table>
      </div>}
      </details>
    </ResearchPanel>
    </div>
    <ResearchPanel title="安全與傳輸檢查">{run.checks?.length ? <details><summary className="cursor-pointer text-sm text-indigo-300">查看 {run.checks.length} 筆原始檢查證據</summary><div className="mt-3"><ResearchDetails value={run.checks} /></div></details> : <p className="text-sm text-amber-200">尚未回傳檢查結果；不可推定通過。</p>}</ResearchPanel>
    <div id="research-selection" hidden={section !== 'candidates'}><ResearchVersionSelection key={run.id} run={run} /></div>
  </div>;
}
