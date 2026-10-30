import { useEffect, useMemo, useState } from 'react';
import { useTagStore } from '../store/tagStore';
import { hasGoodOpcQuality, observationFreshnessMs, OBSERVATION_FUTURE_TOLERANCE_MS, parseControlObservation } from '../lib/controlObservation';

interface Props { systemPrefix: string; wsConnected: boolean }
const numberFormat = new Intl.NumberFormat(undefined, { maximumSignificantDigits: 6 });

function Metric({ value, unit, squared = false }: { value: number | null; unit: string; squared?: boolean }) {
  if (value === null) return <span title="無可用統計值">—</span>;
  return <>{numberFormat.format(value)} <span className="text-slate-500">{unit.trim() ? (squared ? `(${unit})²` : unit) : '（未提供單位）'}</span></>;
}

export function ControlPerformance({ systemPrefix, wsConnected }: Props) {
  const tag = useTagStore(state => state.tags[`${systemPrefix}:ControlObservation`]);
  const parsed = useMemo(() => parseControlObservation(tag?.value, systemPrefix), [tag?.value, systemPrefix]);
  const observation = parsed.observation;
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);

  const ageMs = observation ? now - Date.parse(observation.observed_at) : null;
  const staleAfterMs = observation ? observationFreshnessMs(observation.sample_time_seconds) : null;
  const stale = ageMs !== null && staleAfterMs !== null && ageMs > staleAfterMs;
  let unavailable: string | null = null;
  if (!systemPrefix) unavailable = '尚未選定控制器。';
  else if (!wsConnected) unavailable = '連線已中斷，隱藏快取數值。';
  else if (!tag) unavailable = '尚未收到 ControlObservation 節點；請確認節點配置與引擎狀態。';
  else if (!hasGoodOpcQuality(tag.status)) unavailable = '觀測節點品質不良或不確定。';
  else if (parsed.error) unavailable = parsed.error;
  else if (ageMs !== null && ageMs < -OBSERVATION_FUTURE_TOLERANCE_MS) unavailable = '引擎時間超前瀏覽器，請檢查時鐘同步。';
  else if (stale) unavailable = '觀測已過期，等待新的有效掃描。';
  else if (observation?.samples === 0 || observation?.cv.length === 0) unavailable = '尚無有效 CV 觀測視窗。';
  const showSnapshot = unavailable === null && observation !== null;
  const showMetrics = showSnapshot && observation.complete && observation.samples >= 2;
  const missingMetrics = observation?.cv.some(cv => cv.variance === null || cv.tracking_error_variance === null || cv.bias === null || cv.rmse === null);
  const status = unavailable ? (stale ? '資料過期' : '觀測不可用')
    : !showMetrics ? '累積視窗中' : missingMetrics ? '部分指標不可用' : '完整實測視窗';

  return <section aria-label="實測控制表現" className="bg-slate-900 border border-slate-700 rounded-xl p-4 sm:p-5 mb-6">
    <div className="flex flex-wrap items-start justify-between gap-2 mb-3">
      <div><h2 className="text-base font-semibold text-slate-200">實測控制表現</h2><p className="text-xs text-slate-400">目前視窗的變異與偏差，不是模擬改善率。</p></div>
      <p role="status" className={`text-xs ${unavailable || !showMetrics || missingMetrics ? 'text-amber-300' : 'text-slate-300'}`}>{status}</p>
    </div>
    {unavailable && <p className="text-sm text-amber-300 mb-3 break-words">{unavailable}</p>}
    {observation && <div className="text-xs text-slate-400 flex flex-wrap gap-x-5 gap-y-1 mb-4">
      <span>視窗 {observation.samples} / {observation.window_samples} 筆</span>
      <span>引擎收件時間：<time dateTime={observation.observed_at}>{observation.observed_at}</time></span>
      <span>掃描週期 {numberFormat.format(observation.sample_time_seconds)} s</span>
      <span>當時請求：{['停止', '僅計算', '接管'][observation.requested_mode]}（不代表命令已落實）</span>
    </div>}
    {showSnapshot && <>
      {!showMetrics && <p className="text-xs text-amber-300 mb-3">目前只顯示量測快照；視窗累積完成後才呈現統計。</p>}
      <div className="overflow-x-auto mb-4">
        <table className="w-full text-xs text-right whitespace-nowrap">
          <caption className="text-left text-sm text-slate-300 font-medium mb-2">CV · 各變數保留自己的工程單位</caption>
          <thead className="text-slate-400 border-b border-slate-700"><tr>
            <th scope="col" className="p-2 text-left">CV</th><th scope="col" className="p-2">實際值</th><th scope="col" className="p-2">目標</th>
            <th scope="col" className="p-2">實際值變異</th><th scope="col" className="p-2">平均偏差</th><th scope="col" className="p-2">操作超限筆數</th>
          </tr></thead>
          <tbody className="text-slate-200">{observation.cv.map((cv, index) => <tr key={`${cv.name}:${index}`} className="border-b border-slate-800">
            <th scope="row" className="p-2 text-left font-medium">{cv.name}</th>
            <td className="p-2"><Metric value={cv.pv} unit={cv.unit} /></td><td className="p-2"><Metric value={cv.target} unit={cv.unit} /></td>
            <td className="p-2"><Metric value={showMetrics ? cv.variance : null} unit={cv.unit} squared /></td>
            <td className="p-2"><Metric value={showMetrics ? cv.bias : null} unit={cv.unit} /></td>
            <td className="p-2">{showMetrics ? `${cv.operating_violation_samples} / ${observation.samples}` : '—'}</td>
          </tr>)}</tbody>
        </table>
      </div>
      {observation.mv.length > 0 && <div className="overflow-x-auto">
        <table className="w-full text-xs text-right">
          <caption className="text-left text-sm text-slate-300 font-medium mb-2">MV · 已觀測 SP 的動作量，非求解器提案</caption>
          <thead className="text-slate-400 border-b border-slate-700"><tr><th scope="col" className="p-2 text-left">MV</th><th scope="col" className="p-2">回讀 SP</th><th scope="col" className="p-2">累積變動量 Σ|ΔSP|</th></tr></thead>
          <tbody className="text-slate-200">{observation.mv.map((mv, index) => <tr key={`${mv.name}:${index}`} className="border-b border-slate-800">
            <th scope="row" className="p-2 text-left font-medium">{mv.name}</th><td className="p-2"><Metric value={mv.applied_sp} unit={mv.unit} /></td>
            <td className="p-2"><Metric value={showMetrics ? mv.total_variation : null} unit={mv.unit} /></td>
          </tr>)}</tbody>
        </table>
      </div>}
      <details className="text-xs text-slate-400 mt-4">
        <summary className="cursor-pointer text-indigo-200">詳細統計與操作範圍</summary>
        <div className="overflow-x-auto mt-3"><table className="w-full text-right whitespace-nowrap">
          <thead><tr><th scope="col" className="p-2 text-left">CV</th><th scope="col" className="p-2">標準差</th><th scope="col" className="p-2">追蹤誤差變異</th><th scope="col" className="p-2">RMSE</th><th scope="col" className="p-2">操作下限／上限</th></tr></thead>
          <tbody>{observation.cv.map((cv, index) => <tr key={`${cv.name}:${index}`} className="border-t border-slate-800">
            <th scope="row" className="p-2 text-left">{cv.name}</th>
            <td className="p-2"><Metric value={showMetrics && cv.variance !== null ? Math.sqrt(cv.variance) : null} unit={cv.unit} /></td>
            <td className="p-2"><Metric value={showMetrics ? cv.tracking_error_variance : null} unit={cv.unit} squared /></td>
            <td className="p-2"><Metric value={showMetrics ? cv.rmse : null} unit={cv.unit} /></td>
            <td className="p-2"><Metric value={cv.low} unit={cv.unit} /> / <Metric value={cv.high} unit={cv.unit} /></td>
          </tr>)}</tbody>
        </table></div>
      </details>
    </>}
    <details className="text-xs text-slate-400 mt-4">
      <summary className="cursor-pointer">指標定義與資料有效性</summary>
      <div className="mt-2 space-y-2 max-w-4xl">
        <p>有效掃描等權重計算母體變異，不是時間加權。偏差為平均 PV−目標；RMSE 為均方根誤差；標準差為變異的平方根。目標固定時，PV 變異與追蹤誤差變異相同。破折號代表不可用，非零。</p>
        <p>超限以樣本計數，不是秒數，也不是產品規格判定。MV 動作量來自保留樣本間的 SP 回讀，不等於閥門行程或能源成本。此處沒有工廠改善率或因果效益判定。</p>
        <p>模式、目標或操作限制改變，及觀測故障／中斷，都會重置視窗。收件時間不是 PLC 採集時間。</p>
        <p>僅供顯示的過期門檻：max(10 秒，3×掃描週期){staleAfterMs !== null ? `，本筆為 ${numberFormat.format(staleAfterMs / 1000)} 秒` : ''}。時間超前瀏覽器逾 5 秒、斷線或品質不良時隱藏量測；這些 UI 判斷不授予控制權限。</p>
      </div>
    </details>
  </section>;
}
