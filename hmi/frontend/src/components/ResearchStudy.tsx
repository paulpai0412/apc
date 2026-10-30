import type { Campaign, Dataset, EngineeringVariable, ResearchBootstrap, ResearchSettings, ResearchValue, Study } from '../types/Research';
import { ResearchField, ResearchNumber, ResearchPanel, researchButton, researchControl } from './ResearchControls';
import { ResearchObjectiveControls } from './ResearchObjectiveControls';
import { objectValue, parameterLabels } from '../lib/studyValidation';

const variableLabels: Record<string, string> = { low: '操作下限', high: '操作上限', target: '目標值', weight: 'CV 權重', alpha: '參考軌跡 alpha', slack_weight: '操作限制 slack 權重', max_move: '每步最大變動', weight_r: 'MV 移動權重' };
const nodeFields = {
  cv: ['pv', 'target', 'prediction'],
  mv: ['pv', 'sp', 'applied_sp', 'op', 'mode', 'mode_target', 'future_plan', 'target'],
  dv: ['pv'],
} as const;
const nodeLabels: Record<string, string> = { pv: 'PV', target: '目標', prediction: '預測', sp: '命令 SP', applied_sp: '獨立已應用 SP（選填）', op: 'PID OP', mode: '實際模式', mode_target: '請求模式', future_plan: '未來命令計畫' };

function EngineeringFields({ value, onChange, path }: { value: ResearchValue; onChange: (value: ResearchValue) => void; path: string }) {
  if (Array.isArray(value)) return <div className="space-y-3">{value.map((item, index) => <EngineeringFields key={index} path={`${path} ${index + 1}`} value={item} onChange={next => onChange(value.map((current, i) => i === index ? next : current))} />)}</div>;
  if (value !== null && typeof value === 'object') return <div className="grid gap-4 sm:grid-cols-2">{Object.entries(value).map(([key, item]) => typeof item === 'object' && item !== null ? <details key={key} className="min-w-0 rounded border border-slate-700 p-3 sm:col-span-2"><summary className="cursor-pointer text-sm text-indigo-300">{key.replaceAll('_', ' ')}</summary><div className="mt-3"><EngineeringFields path={`${path} ${key}`} value={item} onChange={next => onChange({ ...value, [key]: next })} /></div></details> : <EngineeringFields key={key} path={`${path} ${key}`} value={item} onChange={next => onChange({ ...value, [key]: next })} />)}</div>;
  if (typeof value === 'number') return <ResearchNumber label={path.replaceAll('_', ' ')} value={value} onChange={next => onChange(next ?? NaN)} />;
  if (typeof value === 'boolean') return <label className="flex gap-2 text-sm"><input type="checkbox" checked={value} onChange={event => onChange(event.target.checked)} />{path.replaceAll('_', ' ')}</label>;
  if (typeof value === 'string') return <ResearchField label={path.replaceAll('_', ' ')}><input className={researchControl} value={value} onChange={event => onChange(event.target.value)} /></ResearchField>;
  return <span className="text-sm text-slate-400">{path}: 未設定</span>;
}


export function ResearchStudy({ study, datasets, capabilities, section, onChange, onSelectDataset }: { study: Study; datasets: Dataset[]; capabilities: ResearchBootstrap['capabilities']; section: 'engineering' | 'tuning'; onChange: (study: Study) => void; onSelectDataset: (id: string) => void }) {
  const updateResearch = (patch: Partial<ResearchSettings>) => onChange({ ...study, research: { ...study.research, ...patch } as ResearchSettings });
  const updateConfig = (patch: Partial<Campaign>) => onChange({ ...study, model_id: undefined, config: { ...study.config, ...patch } as Campaign });
  const updateVariable = (role: 'cv' | 'mv' | 'dv', index: number, patch: Partial<EngineeringVariable>) => {
    const variables = { ...study.config.variables, [role]: study.config.variables[role].map((variable, i) => i === index ? { ...variable, ...patch } : variable) };
    let objective = study.research.objective;
    if (objective && role === 'cv') objective = { ...objective, cv: objective.cv.map((gate, i) => {
      const variable = variables.cv[i];
      return i === index ? { ...gate, name: variable.name, quality_spec: variable.spec_low === undefined && variable.spec_high === undefined ? null : { low: variable.spec_low ?? null, high: variable.spec_high ?? null } } : gate;
    }) };
    if (objective && role === 'mv') objective = { ...objective, mv: objective.mv.map((gate, i) => i === index ? { ...gate, name: variables.mv[i].name } : gate) };
    onChange({ ...study, model_id: undefined, config: { ...study.config, variables, engineering_confirmed: false }, research: { ...study.research, objective } });
  };
  const bounds = objectValue(study.research.bounds);
  const initial = objectValue(study.research.initial);
  const bands = Array.isArray(study.research.bands) ? study.research.bands : [];
  const minimumTrials = study.research.methods.length * (1 + 2 * study.research.rounds);
  if (!study.research.objective || !study.config.plant_source) return <ResearchPanel title="3 · 舊版研究"><p className="text-sm text-amber-200">既有快照與結果保留不變。請用上方「建立新研究」重新設定資料來源、降變異門檻與多情境驗證。</p></ResearchPanel>;
  return <ResearchPanel title={section === 'engineering' ? '工程契約與研究版本' : '固定目標與有界 MPC 搜尋'}>
    <div className="space-y-6">
      <fieldset hidden={section !== 'engineering'} disabled={section !== 'engineering'} className="space-y-4">
      <div className="grid gap-4 sm:grid-cols-2">
        <ResearchField label="研究名稱"><input required maxLength={120} className={researchControl} value={study.name} onChange={event => onChange({ ...study, name: event.target.value })} /></ResearchField>
        <ResearchField label="研究使用的資料"><select className={researchControl} value={study.dataset_id || ''} onChange={event => onSelectDataset(event.target.value)}><option value="">產生本機三個月模擬資料（不使用匯入資料）</option>{datasets.filter(dataset => dataset.kind === 'clean').map(dataset => <option key={dataset.id} value={dataset.id} disabled={dataset.usable === false}>{dataset.name} · {dataset.row_count.toLocaleString()} 筆{dataset.usable === false ? ' · 無效' : ''}</option>)}</select></ResearchField>
      </div>
      <p className="rounded-lg border border-indigo-900 bg-indigo-950/30 p-3 text-sm text-indigo-200">{study.config.plant_source === 'identified_history' ? `從此資料辨識 ${String(study.config.model_family || 'fir').toUpperCase()} 控制模型與分開版本的 ${String(study.config.evaluation_model_family || 'sopdt').toUpperCase()} evaluation plant。訊號與切分沿用清理版本；同源模型不是獨立工廠真值。` : '使用明確宣告的獨立合成製程模型；不是實廠模型，也不授權生產寫入。'} 更換資料會建立新草稿，原已儲存版本不變。</p>
      <ResearchField label="製程區段"><input className={researchControl} value={String(study.config.process_area || '未確認')} readOnly /></ResearchField>
      <ResearchField label="控制引擎 metadata.name／工程 ID"><input className={researchControl} maxLength={120} value={typeof study.config.id === 'string' ? study.config.id : ''} onChange={event => updateConfig({ id: event.target.value || undefined, engineering_confirmed: false })} placeholder="未指定；不可當作 Runtime 目標身分" /></ResearchField>
      <ResearchNumber label="取樣時間 Ts（秒；變更新版本）" value={study.config.sample_time} min={0.001} onChange={sample_time => updateConfig({ sample_time: sample_time ?? NaN, engineering_confirmed: false })} />
      {study.dataset_id && <div className="overflow-x-auto"><table className="w-full text-left text-xs"><caption className="mb-2 text-left text-slate-400">清理版本的精確訊號來源（唯讀；變更需建立新資料版本）</caption><thead><tr>{['角色', '模型變數', '原始欄位', '單位', '品質欄位／格式'].map(label => <th key={label} className="p-2">{label}</th>)}</tr></thead><tbody>{(['cv', 'mv', 'dv'] as const).flatMap(role => study.config.variables[role].map(variable => { const mapping = datasets.find(item => item.id === study.dataset_id)?.suggested_signals?.find(item => item.name === variable.name && item.role === role); return <tr key={`${role}:${variable.name}`} className="border-t border-slate-700"><td className="p-2 uppercase">{role}</td><td className="p-2">{variable.name}</td><td className="p-2">{mapping?.column || '來源未記錄，不猜 Tag'}</td><td className="p-2">{variable.unit || '未確認'}</td><td className="p-2">{mapping?.quality_column ? `${mapping.quality_column} / ${mapping.quality_format || '未確認'}` : '未指定品質欄位'}</td></tr>; }))}</tbody></table></div>}
      </fieldset>
      <fieldset hidden={section !== 'tuning'} disabled={section !== 'tuning'} className="space-y-4">
      <p className="text-sm text-slate-300">固定 Ts：{study.config.sample_time} 秒。模型記憶與 evaluation plant 在本頁唯讀；改動須回工程／辨識建立新版本。</p>
      <ResearchField label="開發候選凍結方式"><select className={researchControl} value={typeof study.research.selection_mode === 'string' ? study.research.selection_mode : 'automatic'} onChange={event => updateResearch({ selection_mode: event.target.value })}><option value="manual">工程師比較開發折衷後凍結，再開封留出</option><option value="automatic">依預先固定目標自動選擇，再驗證</option></select></ResearchField>
      <p className="break-all text-xs text-slate-400">已選控制模型：{study.model_id || '未指定；研究將依凍結輸入辨識'}。使用 M5 凍結產物可明確固定模型與 evaluation plant。</p>
      <ResearchObjectiveControls objective={study.research.objective} scenarios={study.research.scenarios} variables={study.config.variables} onObjective={objective => updateResearch({ objective })} onScenarios={scenarios => updateResearch({ scenarios })} />
      <fieldset><legend className="mb-3 font-medium">比較控制方法</legend><div className="grid gap-3 lg:grid-cols-2">{capabilities.methods.map(method => <label key={method.id} className={`flex cursor-pointer items-start gap-3 rounded-lg border p-4 ${study.research.methods.includes(method.id) ? 'border-indigo-500 bg-indigo-950/30' : 'border-slate-700'}`}><input className="mt-1" type="checkbox" checked={study.research.methods.includes(method.id)} onChange={event => updateResearch({ methods: event.target.checked ? [...study.research.methods, method.id] : study.research.methods.filter(id => id !== method.id) })} /><span><span className="block font-medium">{method.label}</span><span className="mt-1 block text-sm text-slate-400">{method.description}</span></span></label>)}</div></fieldset>
      <div className="grid gap-4 sm:grid-cols-3">
        <ResearchNumber label="自適應輪數" value={study.research.rounds} min={3} max={10} step={1} onChange={value => updateResearch({ rounds: value ?? NaN })} />
        <ResearchNumber label={`試驗總上限（至少 ${Number.isFinite(minimumTrials) ? minimumTrials : '—'}）`} value={study.research.max_trials} min={minimumTrials} max={128} step={1} onChange={value => updateResearch({ max_trials: value ?? NaN })} />
        <ResearchNumber label="初始搜尋步長倍率" value={typeof study.research.step_factor === 'number' ? study.research.step_factor : undefined} min={1.0001} max={4} onChange={value => updateResearch({ step_factor: value ?? NaN })} />
      </div>
      <p className="text-sm text-slate-400">每輪雙方向搜尋；未改善時持續縮小步長。每個候選完成 {study.research.scenarios.length} 個情境，最後測試期只做凍結後驗證。</p>
      <fieldset className="space-y-3"><legend className="mb-3 font-medium">有界搜尋參數</legend>{Object.entries(bounds).map(([key, value]) => <div key={key} className="grid gap-3 rounded-lg border border-slate-700 p-3 sm:grid-cols-3"><ResearchNumber label={`${parameterLabels[key] || key} 初始值`} value={typeof initial[key] === 'number' ? initial[key] : undefined} min={Array.isArray(value) && typeof value[0] === 'number' ? value[0] : undefined} max={Array.isArray(value) && typeof value[1] === 'number' ? value[1] : undefined} onChange={next => updateResearch({ initial: { ...initial, [key]: next ?? NaN } })} /><ResearchNumber label={`${parameterLabels[key] || key} 搜尋下界`} value={Array.isArray(value) && typeof value[0] === 'number' ? value[0] : undefined} min={key === 'alpha' ? 0 : 0.000001} max={key === 'alpha' ? 0.999999 : undefined} onChange={next => updateResearch({ bounds: { ...bounds, [key]: [next ?? NaN, Array.isArray(value) ? value[1] : null] } })} /><ResearchNumber label={`${parameterLabels[key] || key} 搜尋上界`} value={Array.isArray(value) && typeof value[1] === 'number' ? value[1] : undefined} min={0.000001} max={key === 'alpha' ? 0.999999 : undefined} onChange={next => updateResearch({ bounds: { ...bounds, [key]: [Array.isArray(value) ? value[0] : null, next ?? NaN] } })} /></div>)}</fieldset>
      {study.research.methods.includes('soft_band') && <fieldset className="space-y-3"><legend className="mb-2 font-medium">Soft-band 品質帶（獨立於操作限制）</legend><p className="text-sm text-slate-400">品質帶以各 CV 目標為中心；採 Target 追蹤模式。操作限制與 MV 硬限制保持獨立。</p>{study.config.variables.cv.map((variable, index) => { const band = objectValue(bands[index]); return <div key={index} className="rounded-lg border border-indigo-900 p-4"><h3 className="mb-3 text-sm text-indigo-300">{variable.name} ({variable.unit})</h3><div className="grid gap-3 sm:grid-cols-3">{(['half_width', 'tracking_scale', 'quadratic_penalty'] as const).map(key => <ResearchNumber key={key} label={`${variable.name} ${{ half_width: '品質帶半寬', tracking_scale: '帶內追蹤倍率', quadratic_penalty: '品質 slack 二次懲罰' }[key]}`} value={typeof band[key] === 'number' ? band[key] : undefined} min={0.000001} max={key === 'tracking_scale' ? 1 : undefined} onChange={next => updateResearch({ bands: study.config.variables.cv.map((_, i) => i === index ? { ...objectValue(bands[i]), [key]: next ?? NaN } : objectValue(bands[i])) })} />)}</div></div>; })}</fieldset>}
      </fieldset>
      <fieldset hidden={section !== 'engineering'} disabled={section !== 'engineering'} className="space-y-4">
      <fieldset className="space-y-3"><legend className="mb-2 font-medium">CV 品質規格（選填 · 圖表與評估共用）</legend>
        <p className="text-sm text-slate-400">設定後會評估規格超限時間與代價，但不取代控制器操作界線或 Soft-band。留空表示未評估規格合格率。</p>
        {study.config.variables.cv.map((variable, index) => <div key={index} className="rounded-lg border border-slate-700 p-3"><h3 className="mb-3 text-sm">{variable.name} ({variable.unit || '單位未知'})</h3><div className="grid gap-3 sm:grid-cols-2">
          <ResearchNumber label={`${variable.name} 品質規格下限（空白 = 未設定）`} required={false} value={variable.spec_low} onChange={spec_low => updateVariable('cv', index, { spec_low })} />
          <ResearchNumber label={`${variable.name} 品質規格上限（空白 = 未設定）`} required={false} value={variable.spec_high} onChange={spec_high => updateVariable('cv', index, { spec_high })} />
        </div></div>)}
      </fieldset>
      <details open><summary className="cursor-pointer font-medium text-indigo-300">工程設定 · CV / MV / DV</summary>
        <p className="my-3 text-xs text-slate-400">操作限、產品規格與硬限分開。硬限留白時，離線研究沿用操作上下限；這不構成現場安全包絡核准。精確物理映射與安全包絡仍由 Runtime profile 擁有。</p>
        {study.dataset_id && <p className="my-3 text-sm text-amber-200">初始界線與目標來自訓練期觀測，只是待審草稿。請填入實際操作許可值；訊號名稱與單位若需改動，請回清理階段建立新版本。</p>}
        <div className="mt-4 space-y-4">{(['cv', 'mv', 'dv'] as const).map(role => <div key={role}><h3 className="mb-2 font-semibold uppercase text-slate-300">{role}</h3><div className="space-y-3">{study.config.variables[role].map((variable, index) => <fieldset key={index} className="min-w-0 rounded-lg border border-slate-700 p-4"><legend className="px-2 text-sm">{variable.name}</legend><div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
          <ResearchField label={`${role.toUpperCase()} ${index + 1} 名稱`}><input required readOnly={!!study.dataset_id} className={researchControl} value={variable.name} onChange={event => updateVariable(role, index, { name: event.target.value })} /></ResearchField>
          <ResearchField label={`${role.toUpperCase()} ${index + 1} 單位`}><input required readOnly={!!study.dataset_id} className={researchControl} value={variable.unit} onChange={event => updateVariable(role, index, { unit: event.target.value })} /></ResearchField>
          {Object.entries(variable).filter(([key, value]) => typeof value === 'number' && !['spec_low', 'spec_high', 'hard_low', 'hard_high', 'measurement_delay_seconds'].includes(key)).map(([key, value]) => <ResearchNumber key={key} label={`${variable.name} ${variableLabels[key] || key}`} value={value as number} min={['weight', 'slack_weight', 'max_move', 'weight_r'].includes(key) ? 0.000001 : key === 'alpha' ? 0 : undefined} max={key === 'alpha' ? 0.999999 : undefined} onChange={next => updateVariable(role, index, { [key]: next ?? NaN })} />)}
          <ResearchNumber label={`${variable.name} 硬下限（${variable.unit}；選填）`} required={false} value={variable.hard_low} onChange={hard_low => updateVariable(role, index, { hard_low })} />
          <ResearchNumber label={`${variable.name} 硬上限（${variable.unit}；選填）`} required={false} value={variable.hard_high} onChange={hard_high => updateVariable(role, index, { hard_high })} />
          <ResearchField label={`${variable.name} 精確原始來源欄位（選填）`}><input className={researchControl} value={variable.source_column || ''} onChange={event => updateVariable(role, index, { source_column: event.target.value || undefined })} /></ResearchField>
          <ResearchField label={`${variable.name} 量測契約`}><select className={researchControl} value={variable.measurement_kind || ''} onChange={event => updateVariable(role, index, { measurement_kind: event.target.value ? event.target.value as EngineeringVariable['measurement_kind'] : undefined })}><option value="">未明訂（不可推定為即時量測）</option><option value="fast">快速量測</option><option value="delayed_lab">延遲化驗</option><option value="estimated">估測值</option></select></ResearchField>
          <ResearchNumber label={`${variable.name} 已知量測延遲（秒；選填）`} required={false} value={variable.measurement_delay_seconds} min={0} onChange={measurement_delay_seconds => updateVariable(role, index, { measurement_delay_seconds })} />
        </div>
        {role === 'cv' && variable.measurement_kind && variable.measurement_kind !== 'fast' && <p className="mt-3 text-xs text-amber-200">此 CV 不是直接快掃描回授。沒有合格估測器與延遲契約時只能保存草稿；不可用 forward-fill 冒充新鮮回授。</p>}
        <details className="mt-3"><summary className="cursor-pointer text-sm text-indigo-300">凍結前精確點位契約 · {variable.node_ids ? '已建立研究映射草稿' : '尚無映射，僅離線研究'}</summary>
          <p className="my-3 text-xs text-slate-400">只保存研究工程對照，不修改 Runtime profile。未輸入的點位保持缺失；已應用 SP 必須與命令 SP 獨立，OP 不當作同座標回授。提供映射後需完整通過後端檢查才能凍結。</p>
          <div className="grid gap-3 sm:grid-cols-2">{nodeFields[role].map(key => <ResearchField key={key} label={`${variable.name} ${nodeLabels[key]} Node ID`}><input className={researchControl} maxLength={512} value={variable.node_ids?.[key] || ''} onChange={event => updateVariable(role, index, { node_ids: { ...variable.node_ids, [key]: event.target.value || undefined } })} /></ResearchField>)}
            {(role === 'dv' ? ['low', 'high'] as const : ['low', 'high', 'll', 'hh'] as const).map(key => <ResearchField key={key} label={`${variable.name} limits.${key} Node ID${key === 'll' || key === 'hh' ? '（選填）' : ''}`}><input className={researchControl} maxLength={512} value={variable.node_ids?.limits?.[key] || ''} onChange={event => updateVariable(role, index, { node_ids: { ...variable.node_ids, limits: { ...variable.node_ids?.limits, [key]: event.target.value || undefined } } })} /></ResearchField>)}
          </div>
          {variable.node_ids && <button type="button" className={`${researchButton} mt-3`} onClick={() => updateVariable(role, index, { node_ids: undefined })}>移除整組研究映射，改為僅離線研究</button>}
        </details>
        </fieldset>)}</div></div>)}</div>
      </details>
      <label className="flex items-start gap-3 rounded-lg border border-amber-900 bg-amber-950/20 p-4 text-sm text-amber-100"><input type="checkbox" className="mt-1" checked={study.config.engineering_confirmed === true} onChange={event => updateConfig({ engineering_confirmed: event.target.checked })} /><span>已確認工程單位、目標、操作界線及 MV 最大步幅；MV 歷史是下層 PID 已生效的 SP，不是 PV 或 OP。此確認只允許本機研究，不是現場接管授權。</span></label>
      <details><summary className="cursor-pointer font-medium text-indigo-300">進階 · 控制器、模型適用性與模擬設定</summary><div className="mt-4 space-y-4">{Object.entries(study.config).filter(([key]) => !['variables', 'schema_version', 'id', 'candidates', 'engineering_basis', 'engineering_confirmed', 'process_area', 'plant_source', 'mv_coordinate', 'data_window'].includes(key)).map(([key, value]) => <details key={key} className="rounded-lg border border-slate-700 p-3"><summary className="cursor-pointer text-sm text-indigo-300">{key.replaceAll('_', ' ')}</summary><div className="mt-3"><EngineeringFields path={key} value={value as ResearchValue} onChange={next => updateConfig({ [key]: next, engineering_confirmed: false })} /></div></details>)}</div></details>
      </fieldset>
    </div>
  </ResearchPanel>;
}
