export type WorkspaceMode = 'runtime' | 'modeling';

export type RuntimePage = 'overview' | 'trends' | 'control' | 'events' | 'performance' | 'versions' | 'administration';
export type ModelingPage = 'overview' | 'sources' | 'engineering' | 'cleaning' | 'identification' | 'tuning' | 'simulation' | 'candidates' | 'policy';

export type WorkspacePage = { id: string; label: string; purpose: string; showScene: boolean };

export const runtimePages: readonly (WorkspacePage & { id: RuntimePage })[] = [
  { id: 'overview', label: '設備總覽', purpose: '即時設備、資料品質與需要處理的事項', showScene: true },
  { id: 'trends', label: '趨勢與診斷', purpose: '同時間軸的 CV／MV／DV 與完整預測', showScene: false },
  { id: 'control', label: '控制操作', purpose: '檢查接管條件、預覽操作與追蹤交接', showScene: false },
  { id: 'events', label: '事件與資料健康', purpose: '區分告警、資料異常與操作結果', showScene: false },
  { id: 'performance', label: '績效與漂移', purpose: '比較相容工況下的 CV 品質與 MV 代價', showScene: false },
  { id: 'versions', label: '版本與交接', purpose: '審查、發布及核對安全交接證據', showScene: false },
  { id: 'administration', label: '連線、權限與稽核', purpose: '檢查來源、授權與可追溯操作', showScene: false },
];

export const modelingPages: readonly (WorkspacePage & { id: ModelingPage })[] = [
  { id: 'overview', label: '研究總覽與任務', purpose: '查看產物、工作進度與下一步', showScene: false },
  { id: 'sources', label: '資料來源與快照', purpose: '匯入 CSV、PostgreSQL 或唯讀採集資料', showScene: false },
  { id: 'engineering', label: '設備、點位與工程契約', purpose: '確認 CV／MV／DV、單位、限制與設備綁定', showScene: true },
  { id: 'cleaning', label: '清洗與工況整理', purpose: '預覽規則、檢查差異與保存清洗版本', showScene: false },
  { id: 'identification', label: '模型辨識與資格', purpose: '辨識動態並檢查模型適用證據', showScene: false },
  { id: 'tuning', label: 'MPC 設定與自動調參', purpose: '在固定模型與界線內比較控制折衷', showScene: false },
  { id: 'simulation', label: '閉環模擬與壓力驗證', purpose: '執行原生閉環並觀察新 MV 產生的 CV', showScene: true },
  { id: 'candidates', label: '比較、候選與送審', purpose: '核對改善、代價及不可變驗證證據', showScene: false },
  { id: 'policy', label: '持續研究政策', purpose: '配置有界研究觸發與停止條件，不授予現場寫入權', showScene: false },
];
