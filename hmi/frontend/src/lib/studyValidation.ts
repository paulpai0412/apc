import type { Dataset, ResearchValue, Study } from '../types/Research';

export const parameterLabels: Record<string, string> = { cv_weight_scale: 'CV 權重倍率', mv_weight_scale: 'MV 權重倍率', alpha: '參考軌跡 alpha' };
export const objectValue = (value: ResearchValue | undefined): Record<string, ResearchValue> => value && !Array.isArray(value) && typeof value === 'object' ? value : {};

export function validateStudy(study: Study, datasets: Dataset[]): string | null {
  if (!study.name.trim()) return '請輸入研究名稱。';
  if (!study.research.objective || !study.config.plant_source) return '此為舊版研究快照。請建立新研究；既有結果仍可檢視。';
  if (study.config.engineering_confirmed !== true) return '請確認工程單位、目標、操作界線及 MV 最大步幅；觀測範圍不是已核准的安全限。';
  if (!study.research.methods.length) return '請至少選擇一種控制方法。';
  const minimum = study.research.methods.length * (1 + 2 * study.research.rounds);
  if (!Number.isInteger(study.research.rounds) || study.research.rounds < 3 || study.research.rounds > 10) return '自適應輪數必須為 3–10 的整數，至少覆蓋三種搜尋參數。';
  if (!Number.isInteger(study.research.max_trials) || study.research.max_trials < minimum || study.research.max_trials > 128) return `試驗上限必須為 ${minimum}–128，包含各方法基準與每輪雙方向候選。`;
  if (study.dataset_id && !datasets.some(dataset => dataset.id === study.dataset_id && dataset.kind === 'clean' && dataset.usable !== false)) return '請選擇可用的清理資料集。';
  const bounds = objectValue(study.research.bounds);
  const initial = objectValue(study.research.initial);
  for (const [key, value] of Object.entries(bounds)) {
    if (!Array.isArray(value) || value.length !== 2 || typeof value[0] !== 'number' || typeof value[1] !== 'number' || value[0] >= value[1]) return `${parameterLabels[key] || key} 的下界必須小於上界。`;
    if (typeof initial[key] !== 'number' || initial[key] < value[0] || initial[key] > value[1]) return `${parameterLabels[key] || key} 的初始值必須位於搜尋界限內。`;
  }
  for (const [role, variables] of Object.entries(study.config.variables)) for (const variable of variables) {
    if (!variable.unit.trim() || !Number.isFinite(variable.low) || !Number.isFinite(variable.high)) return `${variable.name} 請填寫工程單位與有限操作界線。`;
    if (variable.low >= variable.high) return `${variable.name} 的操作下限必須小於上限。`;
    if (variable.hard_low !== undefined && (!Number.isFinite(variable.hard_low) || variable.hard_low > variable.low)) return `${variable.name} 的硬下限必須有限且不高於操作下限。`;
    if (variable.hard_high !== undefined && (!Number.isFinite(variable.hard_high) || variable.hard_high < variable.high)) return `${variable.name} 的硬上限必須有限且不低於操作上限。`;
    if (variable.measurement_delay_seconds !== undefined && (!Number.isFinite(variable.measurement_delay_seconds) || variable.measurement_delay_seconds < 0)) return `${variable.name} 的已知量測延遲必須是非負有限秒數。`;
    if (role === 'cv' && variable.target !== undefined && (variable.target < variable.low || variable.target > variable.high)) return `${variable.name} 的目標值必須位於操作範圍內。`;
    if (role === 'cv') {
      if ([variable.spec_low, variable.spec_high].some(value => value !== undefined && !Number.isFinite(value))) return `${variable.name} 的品質規格須為有限數值，或留空表示未設定。`;
      if (variable.spec_low !== undefined && variable.spec_high !== undefined && variable.spec_low >= variable.spec_high) return `${variable.name} 的品質規格下限必須小於上限。`;
    }
  }
  return null;
}
