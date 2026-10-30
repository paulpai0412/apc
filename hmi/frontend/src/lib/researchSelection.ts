import type { ResearchRun } from '../types/Research';

/** Only development evidence may authorize a pre-holdout selection. */
export function researchSelectionIssue(run: ResearchRun, selections: Record<string, string>, reason: string): string | null {
  if (run.status !== 'running' || run.stage !== 'awaiting_selection' || run.results) return '此 run 不在開封前選擇階段，不能重選候選。';
  if (!run.development) return '缺少開發情境證據，不能凍結候選。';
  if (!reason.trim() || reason.length > 2000) return '請填寫 1–2000 字的選擇理由。';
  const methods = new Set(run.trials.map(trial => trial.method));
  if (!methods.size || Object.keys(selections).length !== methods.size) return '每一個實際方法都必須選擇一個候選或保留基準。';
  for (const method of methods) {
    const trial = run.trials.find(item => item.id === selections[method] && item.method === method);
    if (!trial || !trial.valid || (trial.round !== 0 && trial.eligible !== true)) return `${method} 的選擇不屬於有效且通過資格的開發候選。`;
  }
  return null;
}
