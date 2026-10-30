import type { Metrics, ResearchRun } from '../types/Research';
import { ResearchDetails, ResearchPanel } from './ResearchControls';

function metric(metrics: Metrics, field: string, index: number): number | null {
  const values = metrics[field];
  const value = Array.isArray(values) ? values[index] : undefined;
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function number(value: number | null): string {
  return value === null ? '未記錄' : value.toLocaleString('zh-TW', { maximumSignificantDigits: 6 });
}

function change(baseline: number | null, selected: number | null): string {
  if (baseline === null || selected === null) return '資料不足，無法比較';
  if (baseline === 0) return '基準為 0，百分比不可比較';
  const percent = (selected - baseline) / Math.abs(baseline) * 100;
  if (!Number.isFinite(percent)) return '百分比不可比較';
  if (percent === 0) return '與基準相同';
  return `${percent < 0 ? '減少' : '增加'} ${number(Math.abs(percent))}%`;
}

function Comparison({ label, baseline, selected, unit, relative = false, missing = '未記錄' }: {
  label: string; baseline: number | null; selected: number | null; unit: string; relative?: boolean; missing?: string;
}) {
  return <div className="min-w-0 rounded-lg bg-slate-950/60 p-3">
    <dt className="text-xs text-slate-400">{label} · {unit}</dt>
    <dd className="mt-1 break-words text-sm text-slate-100"><span className="text-slate-400">{baseline === null ? missing : number(baseline)}</span> → {selected === null ? missing : number(selected)}</dd>
    {relative && <dd className="mt-1 text-xs text-indigo-200">{change(baseline, selected)}</dd>}
  </div>;
}

export function ResearchOutcomeSummary({ run }: { run: ResearchRun }) {
  if (!run.results) return null;
  const results = run.results;
  const candidates = results.methods.filter(method => method.recommendation === 'candidate_for_review');
  const developmentScores = results.methods.map(method => ({
    method: method.method,
    score: run.trials.find(item => item.id === method.selected_trial && item.method === method.method)?.score ?? null,
  }));
  return <ResearchPanel title="凍結候選驗證 · 變異與動作代價">
    <p className="mb-2 text-sm text-amber-200">{run.status !== 'completed' ? '尚未完成，不可送審' : candidates.length ? `${candidates.length} 種已凍結方法可送審；留出結果不重新排序或改選勝者` : '無可送審候選，請查看拒絕原因'}</p>
    <p id="research-comparison-scope" className="mb-2 text-xs text-slate-400">配對 native 閉環模擬：自身基準 → 凍結候選。不是歷史實測 CV 的反事實改善，也不是工廠投運證據；獨立留出資格以來源曝光紀錄為準。</p>
    {results.methods.length > 1 && <p className="mb-2 text-xs text-indigo-300 sm:hidden">左右滑動，比較 {results.methods.length} 種方法。</p>}
    <div className="overflow-x-auto">
      <table aria-describedby="research-comparison-scope" className="w-full text-left text-sm">
        <thead><tr className="border-b border-slate-700"><th scope="col" className="py-2 pr-3 font-medium text-slate-400">工程指標</th>{results.methods.map(method => <th scope="col" key={method.method} className="px-3 py-2 font-medium text-indigo-200">{method.method}<span className={`block text-xs font-normal ${method.recommendation === 'candidate_for_review' ? 'text-emerald-300' : 'text-amber-300'}`}>{method.recommendation === 'candidate_for_review' ? '可送審' : '不可選版'}</span></th>)}</tr></thead>
        <tbody>{(['cv', 'mv'] as const).flatMap(role => results.variables[role].map((variable, index) => {
          const field = role === 'cv' ? 'cv_variance' : 'mv_total_variation';
          const unit = variable.unit || '單位未知';
          return <tr key={`${role}:${index}`} className="border-b border-slate-800 last:border-0">
            <th scope="row" className="py-2 pr-3 font-normal"><span className="block break-words">{role.toUpperCase()} · {variable.name}</span><span className="text-xs text-slate-400">{role === 'cv' ? `實際變異 (${unit})²` : `累積 |Δ命令| (${unit})`}</span></th>
            {results.methods.map(method => {
              const baseline = metric(method.baseline.test, field, index);
              const selected = metric(method.optimized.test, field, index);
              return <td key={method.method} className="px-3 py-2 align-top"><span className="block whitespace-nowrap text-xs text-slate-400">{number(baseline)} → {number(selected)}</span><span className={baseline === null || selected === null ? 'text-slate-400' : selected > baseline ? 'text-amber-300' : 'text-emerald-300'}>{change(baseline, selected)}</span></td>;
            })}
          </tr>;
        }))}</tbody>
      </table>
    </div>
    <details className="mt-3">
      <summary className="cursor-pointer text-xs text-indigo-300">偏差、越限、選參依據與拒絕原因</summary>
      <p className="my-3 text-xs text-slate-400">開發情境以固定目標與硬門檻選擇；上表是實體 Var(CV)，與 Var(CV − 目標) 不可混用。動作量不是能耗。此處的最終驗證不能用來重挑方法。</p>
      <div className="grid min-w-0 gap-4 xl:grid-cols-2">{results.methods.map(method => <article key={method.method} className="min-w-0 space-y-3 rounded-lg border border-slate-700 p-3">
        <header><h3 className="font-semibold text-indigo-200">{method.method}</h3><p className="mt-1 break-all text-xs text-slate-400">凍結試驗 {method.selected_trial || '未記錄'} · 全情境門檻：{method.assessment?.eligible === true ? '通過' : method.assessment?.eligible === false ? '未通過' : '未評估'}</p><p className="text-xs text-slate-400">凍結前開發分數：{number(developmentScores.find(item => item.method === method.method)?.score ?? null)}</p></header>
        {method.selection_recommendation === 'retain_baseline' && <p className="text-sm text-amber-200">沒有合格改善候選，保留基準。</p>}
        {method.assessment?.reasons?.length ? <ul className="list-inside list-disc space-y-1 text-xs text-amber-200">{method.assessment.reasons.map((reason, index) => <li key={index}>{reason}</li>)}</ul> : method.assessment?.eligible === false ? <p className="text-xs text-amber-200">拒絕原因未記錄。</p> : null}
        {results.variables.cv.map((variable, index) => {
          const unit = variable.unit || '單位未知';
          const baseline = method.baseline.test;
          const selected = method.optimized.test;
          const quality = selected.quality_assessed;
          return <section key={`${index}:${variable.name}`}><h4 className="mb-2 text-sm">CV · {variable.name}</h4><dl className="grid gap-2 sm:grid-cols-2">
            <Comparison label="追蹤偏差 mean(CV − 目標)" unit={unit} baseline={metric(baseline, 'cv_tracking_bias', index)} selected={metric(selected, 'cv_tracking_bias', index)} />
            <Comparison label="目標誤差變異 Var(CV − 目標)" unit={`(${unit})²`} baseline={metric(baseline, 'cv_tracking_error_variance', index)} selected={metric(selected, 'cv_tracking_error_variance', index)} relative />
            <Comparison label="追蹤 RMSE" unit={unit} baseline={metric(baseline, 'cv_rmse', index)} selected={metric(selected, 'cv_rmse', index)} />
            <Comparison label="操作違限時間" unit="秒" baseline={metric(baseline, 'cv_violation_seconds', index)} selected={metric(selected, 'cv_violation_seconds', index)} />
            <Comparison label="品質規格違限時間" unit="秒" baseline={metric(baseline, 'cv_spec_violation_seconds', index)} selected={metric(selected, 'cv_spec_violation_seconds', index)} missing={Array.isArray(quality) && quality[index] === false ? '未評估規格' : '未記錄'} />
          </dl></section>;
        })}
        <details><summary className="cursor-pointer text-xs text-indigo-300">全部情境門檻、參數與原始指標</summary><ResearchDetails value={{ assessment: method.assessment ?? null, baseline: method.baseline, optimized: method.optimized, transport: method.transport, parameters: method.parameters }} /></details>
      </article>)}</div>
      <p className="mt-3 text-sm text-amber-200">模型留出預測：{results.model_test?.passed === true ? '通過' : results.model_test?.passed === false ? '未通過，不能選版' : '未記錄，不可推定通過'}</p>
      <details className="mt-2"><summary className="cursor-pointer text-xs text-indigo-300">證據範圍與測試區間</summary><p className="my-3 text-xs text-amber-200">{results.scope}</p><ResearchDetails value={{ model_test: results.model_test ?? null, splits: results.splits }} /></details>
    </details>
  </ResearchPanel>;
}
