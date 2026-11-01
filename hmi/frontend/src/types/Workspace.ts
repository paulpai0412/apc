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
  { id: 'overview', label: '方案摘要與任務', purpose: '查看已保存產物、目前缺項與下一可做操作', showScene: false },
  { id: 'sources', label: '資料匯入', purpose: '匯入 CSV、PostgreSQL 或唯讀採集資料', showScene: false },
  { id: 'engineering', label: '點位與工程界線', purpose: '選取單一 CV／MV／DV，確認單位、界線與工程契約', showScene: true },
  { id: 'cleaning', label: '清理與切分', purpose: '預覽規則、工況與訓練／驗證切分，保存清理版本', showScene: false },
  { id: 'identification', label: '模型辨識與驗證', purpose: '辨識動態、凍結控制模型並檢查資格證據', showScene: false },
  { id: 'tuning', label: '控制器自動調參', purpose: '固定模型、目標與工程界線，搜尋控制器參數', showScene: false },
  { id: 'simulation', label: '模擬比較', purpose: '比較閉環試驗，或建立凍結模型工作階段再驗證', showScene: true },
  { id: 'candidates', label: '候選與送審', purpose: '核對改善、代價及不可變驗證證據', showScene: false },
  { id: 'policy', label: '持續研究政策', purpose: '配置有界研究觸發與停止條件，不授予現場寫入權', showScene: false },
];
