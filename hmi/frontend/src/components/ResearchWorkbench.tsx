import type { ModelingPage } from '../types/Workspace';
import type { Dataset, ResearchRun, Study } from '../types/Research';
import { modelingPages } from '../types/Workspace';

const groups: { label: string; pages: ModelingPage[] }[] = [
  { label: '資料準備', pages: ['sources', 'cleaning', 'engineering'] },
  { label: '模型辨識與驗證', pages: ['identification'] },
  { label: '控制器自動調參', pages: ['tuning'] },
  { label: '模擬比較', pages: ['simulation', 'candidates'] },
];

export function ResearchWorkbench({ section, onNavigate, study, datasets, savedId, run }: {
  section: ModelingPage; onNavigate: (page: ModelingPage) => void; study: Study;
  datasets: Dataset[]; savedId: string; run: ResearchRun | null;
}) {
  const dataset = datasets.find(item => item.id === study.dataset_id);
  const matchingRun = run?.study_id === savedId && !!savedId ? run : null;
  const summaries = [
    dataset ? `${dataset.name} · ${dataset.kind === 'clean' ? '清理版本' : '原始資料'}` : study.config.plant_source === 'independent_synthetic' ? '獨立合成來源 · 非現場資料' : '尚未選取清理資料',
    study.model_id ? `固定模型 ${study.model_id}` : '尚未指定凍結模型',
    matchingRun ? `執行 ${matchingRun.status} · ${matchingRun.progress?.completed ?? '未知'} / ${matchingRun.progress?.total ?? '未知'}` : '尚無目前研究版本的執行紀錄',
    run ? `${run.id} · ${run.status}（依所選 run 檢視）` : '尚未選取試驗 run',
  ];
  const active = groups.find(group => group.pages.includes(section));
  return <section className="workbench" aria-label="建立方案工作台">
    <nav className="workbench-stages" aria-label="方案工作區">
      {groups.map((group, index) => <button key={group.label} type="button" aria-current={group === active ? 'true' : undefined} onClick={() => onNavigate(group.pages[0])}>
        <strong>{group.label}</strong><span>{summaries[index]}</span>
      </button>)}
    </nav>
    {active && active.pages.length > 1 && <nav className="workbench-sections" aria-label={`${active.label}功能`}>
      {active.pages.map(id => <button key={id} type="button" aria-current={id === section ? 'page' : undefined} onClick={() => onNavigate(id)}>{modelingPages.find(page => page.id === id)?.label}</button>)}
    </nav>}
  </section>;
}
