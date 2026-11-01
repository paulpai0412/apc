import type { ProcessTrendData, TrendKind, TrendPoint, TrendSeries, TrendVariable } from '../types/ProcessTrend';
import { finitePoint, trendKindLabels } from '../lib/processTrend';

const quality = { good: '良好', bad: '不良', uncertain: '不確定', unknown: '品質未知', missing: '缺值' };
type Props = {
  pinnedVariableId?: string | null; data: ProcessTrendData; selected: string[]; mobile: boolean; cursorLabel: string;
  formatTime: (time: number) => string; cursorTime: number | null; pointIndex: Map<string, Map<number, TrendPoint>>;
  colors: Record<string, string>; dashes: Record<TrendKind, string | undefined>;
  enabled: (series: TrendSeries) => boolean; focusedId: string | null;
  onVariableToggle: (variable: TrendVariable, checked: boolean) => void;
  onSeriesToggle: (id: string, checked: boolean) => void;
  onFocus: (series: TrendSeries) => void; onHover: (id: string | null) => void;
  onReset: () => void; onDefaults: () => void;
  onVariableLines: (id: string, choice: boolean | 'default') => void;
};

/** One key and one exact-time reading per signal, shared by every aligned plot. */
export function TrendSignalList({ pinnedVariableId, data, selected, mobile, cursorLabel, formatTime, cursorTime, pointIndex, colors, dashes, enabled, focusedId, onVariableToggle, onSeriesToggle, onFocus, onHover, onReset, onDefaults, onVariableLines }: Props) {
  const focused = data.series.find(series => series.id === focusedId && selected.includes(series.variableId) && series.available && enabled(series));
  const count = data.series.filter(series => selected.includes(series.variableId) && series.available && enabled(series)).length;
  return <details open={!mobile} className="trend-signal-list" aria-label="訊號清單與同時間值">
    <summary>訊號清單 · 已顯示 {count} / {data.series.length} 條{focused ? ' · 已聚焦' : ''}</summary>
    <div className="trend-signal-tools"><button type="button" onClick={onReset}>恢復全部線強調</button><button type="button" onClick={onDefaults}>恢復預設選線</button></div>
    <p role="status" className="text-sm text-indigo-200">{focused ? `聚焦：${data.variables.find(variable => variable.id === focused.variableId)?.name} · ${trendKindLabels[focused.kind]}` : '全部線正常強調'}</p>
    <p className="trend-reading-time">同時間值 · {cursorLabel}</p>
    <p className="text-sm text-slate-400">只讀完全相同時間的記錄；缺值不內插。選線可聚焦，其餘淡化。</p>
    {(['CV', 'MV', 'DV', 'OTHER'] as const).filter(role => data.variables.some(variable => variable.role === role)).map(role => <section key={role} aria-label={`${role} 訊號清單`} className="trend-signal-role"><h3>{role}</h3>
      {data.variables.filter(variable => variable.role === role).map(variable => <fieldset key={variable.id} className="trend-signal-variable"><legend><label><input type="checkbox" aria-label={`顯示 ${variable.name}`} disabled={variable.id === pinnedVariableId} checked={selected.includes(variable.id)} onChange={event => onVariableToggle(variable, event.target.checked)} />{variable.name} <span>({variable.unit || '單位未知'})</span></label>{variable.id === pinnedVariableId && <span className="text-sm text-indigo-200">與所選設備點位同步；仍可逐條選線</span>}</legend>
        <details className="text-sm"><summary>此變數選線操作</summary><div className="trend-signal-tools">{([['default', '恢復預設'], [true, '全部線'], [false, '清空']] as const).map(([choice, label]) => <button key={label} type="button" disabled={!selected.includes(variable.id)} aria-label={`${variable.name} ${label}`} onClick={() => onVariableLines(variable.id, choice)}>{label}</button>)}</div></details>
        {data.series.filter(series => series.variableId === variable.id).map(series => {
          const shown = selected.includes(variable.id) && series.available && enabled(series);
          const point = cursorTime === null ? undefined : pointIndex.get(series.id)?.get(cursorTime);
          const group = data.groups?.find(item => item.id === (series.groupId ?? 'live'))?.label ?? series.groupId ?? '即時／歷史';
          return <div key={series.id} className={`trend-signal-row${focusedId === series.id ? ' is-focused' : ''}`} data-signal-id={series.id} onMouseEnter={() => { if (shown) onHover(series.id); }} onMouseLeave={() => onHover(null)}>
            <label className="trend-signal-toggle"><input type="checkbox" aria-label={`顯示線 ${variable.name} · ${group} · ${series.label} · ${trendKindLabels[series.kind]}`} checked={shown} disabled={!series.available || !selected.includes(variable.id)} onChange={event => onSeriesToggle(series.id, event.target.checked)} /></label>
            <div className="min-w-0"><button type="button" className="trend-signal-focus" disabled={!shown} aria-label={`聚焦線 ${variable.name} · ${group} · ${series.label} · ${trendKindLabels[series.kind]}`} aria-pressed={focusedId === series.id} onMouseEnter={() => { if (shown) onHover(series.id); }} onFocus={() => { if (shown) onHover(series.id); }} onBlur={() => onHover(null)} onClick={() => onFocus(series)}>
              <svg width="28" height="10" aria-hidden="true"><line x1="0" y1="5" x2="28" y2="5" stroke={colors[series.id]} strokeWidth="2" strokeDasharray={dashes[series.kind]} /></svg><strong>{trendKindLabels[series.kind]}</strong>
            </button><p className="text-sm">{series.label}</p><p className="text-sm text-slate-400">{group}{series.leadSeconds !== undefined ? ` · 提前 ${series.leadSeconds} 秒` : ''} · {shown ? '顯示中' : series.available ? '未顯示' : '不可用'}</p>
            <p className="trend-signal-value">{series.available && point && finitePoint(point) ? `${point.value!.toLocaleString('zh-TW', { maximumSignificantDigits: 6 })} ${variable.unit || '單位未知'} · ${quality[point.quality]}` : '此時無可用記錄'}</p>
            {!series.available && <p className="text-sm text-amber-200">{series.unavailableReason || '來源未提供'}</p>}
            <details className="text-sm text-slate-400"><summary>來源與時刻</summary><p className="break-all">{series.source || '來源未提供'}</p>{point && <p>有效：{formatTime(point.time)}{point.issuedAt !== undefined && <><br />發佈：{formatTime(point.issuedAt)}</>}</p>}</details></div>
          </div>;
        })}
      </fieldset>)}
    </section>)}
  </details>;
}
