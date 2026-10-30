import type { Campaign, ResearchObjective, ResearchScenario } from '../types/Research';
import { ResearchNumber } from './ResearchControls';

type Props = {
  objective: ResearchObjective;
  scenarios: ResearchScenario[];
  variables: Campaign['variables'];
  onObjective: (objective: ResearchObjective) => void;
  onScenarios: (scenarios: ResearchScenario[]) => void;
};

export function ResearchObjectiveControls({ objective, scenarios, variables, onObjective, onScenarios }: Props) {
  const updateCv = (index: number, patch: Partial<ResearchObjective['cv'][number]>) => onObjective({ ...objective, cv: objective.cv.map((row, i) => i === index ? { ...row, ...patch } : row) });
  const updateMv = (index: number, patch: Partial<ResearchObjective['mv'][number]>) => onObjective({ ...objective, mv: objective.mv.map((row, i) => i === index ? { ...row, ...patch } : row) });
  return <section className="space-y-4" aria-label="降變異目標與接受門檻">
    <div><h3 className="font-semibold text-indigo-200">改善什麼？先固定判定標準</h3><p className="mt-2 text-sm text-slate-400">降低對目標的變異，同時限制偏差、違限與 MV 動作。未通過全部情境，就不推薦採用。</p></div>
    {objective.cv.map((row, index) => <fieldset key={index} className="min-w-0 rounded-lg border border-slate-700 p-4">
      <legend className="px-2 text-sm">{row.name} · {variables.cv[index]?.unit}</legend>
      <div className="grid gap-3 sm:grid-cols-3">
        <ResearchNumber label={`${row.name} 最大目標偏差`} value={row.max_abs_bias} min={0} onChange={value => updateCv(index, { max_abs_bias: value ?? NaN })} />
        <ResearchNumber label={`${row.name} 至少降低變異 (%)`} value={row.min_variance_improvement_fraction * 100} min={0} max={100} onChange={value => updateCv(index, { min_variance_improvement_fraction: value === undefined ? NaN : value / 100 })} />
        {row.quality_spec ? <ResearchNumber label={`${row.name} 規格超限時間上限 (%)`} value={row.max_spec_violation_fraction * 100} min={0} max={100} onChange={value => updateCv(index, { max_spec_violation_fraction: value === undefined ? NaN : value / 100 })} /> : <p className="self-center text-sm text-amber-200">品質規格未設定；不宣稱規格合格率。</p>}
      </div>
      <details className="mt-3"><summary className="cursor-pointer text-sm text-slate-400">操作界線與比較尺度</summary><div className="mt-3 grid gap-3 sm:grid-cols-3">
        <ResearchNumber label={`${row.name} 操作超限時間上限 (%)`} value={row.max_operating_violation_fraction * 100} min={0} max={100} onChange={value => updateCv(index, { max_operating_violation_fraction: value === undefined ? NaN : value / 100 })} />
        <ResearchNumber label={`${row.name} 變異增加上限 (%)`} value={row.max_variance_increase_fraction * 100} min={0} onChange={value => updateCv(index, { max_variance_increase_fraction: value === undefined ? NaN : value / 100 })} />
        <ResearchNumber label={`${row.name} 評分正規化尺度`} value={row.scale} min={0.000001} onChange={value => updateCv(index, { scale: value ?? NaN })} />
      </div></details>
    </fieldset>)}
    <div className="grid gap-3 sm:grid-cols-2">{objective.mv.map((row, index) => <ResearchNumber key={index} label={`${row.name} 動作總量增加上限 (%)`} value={row.max_movement_increase_fraction * 100} min={0} onChange={value => updateMv(index, { max_movement_increase_fraction: value === undefined ? NaN : value / 100 })} />)}</div>
    <details><summary className="cursor-pointer text-sm text-indigo-300">多情境驗證與固定評分權重</summary>
      <p className="my-3 text-sm text-slate-400">每個候選完成所有增益情境；最後測試期不參與搜尋。這些倍率是事先宣告的模型不確定性，不是量測出的信賴區間。</p>
      <div className="grid gap-3 sm:grid-cols-3">{scenarios.map((scenario, index) => <ResearchNumber key={scenario.id} label={`情境 ${scenario.id} 增益倍率`} value={scenario.gain_multiplier} min={scenario.id === 'nominal' ? 1 : 0.25} max={scenario.id === 'nominal' ? 1 : 2} onChange={value => onScenarios(scenarios.map((current, i) => i === index ? { ...current, gain_multiplier: value ?? NaN } : current))} />)}</div>
      <div className="mt-4 grid gap-3 sm:grid-cols-2">{(['variance', 'bias', 'quality', 'movement'] as const).map(key => <ResearchNumber key={key} label={`${{ variance: '變異', bias: '偏差', quality: '規格超限', movement: 'MV 動作' }[key]} 評分權重`} value={objective.coefficients[key]} min={key === 'variance' || key === 'bias' ? 0.000001 : 0} onChange={value => onObjective({ ...objective, coefficients: { ...objective.coefficients, [key]: value ?? NaN } })} />)}</div>
      <div className="mt-4 grid gap-3 sm:grid-cols-2">{objective.mv.map((row, index) => <ResearchNumber key={index} label={`${row.name} 動作正規化尺度`} value={row.scale} min={0.000001} onChange={value => updateMv(index, { scale: value ?? NaN })} />)}</div>
    </details>
  </section>;
}
