import type { ProcessTrendData, TrendEvent, TrendKind, TrendPoint, TrendQuality, TrendSeries, TrendVariable } from '../types/ProcessTrend';

export type HistoryPoint = { timestamp: string; node_id: string; value: number | null; status: number | null };
export type HistoryEnvelope = { points: HistoryPoint[]; rawSamples: number; gapThresholdMs: number; warnings: string[] };
export type HistoryField = { key: string; base: string; field: string; role: 'CV' | 'MV' | 'DV'; alias: string; unit: string; enabled: boolean; pinned: boolean };
const bounds: Record<string, true> = { LowLimit: true, HighLimit: true, SpecLow: true, SpecHigh: true };
const diagnostic: Record<string, true> = { Error: true, ConstraintDistance: true, SaturationFlag: true };
export const isDiagnosticField = (field: string) => Object.hasOwn(diagnostic, field);
export const isLimitField = (field: string) => Object.hasOwn(bounds, field);

export function opcQuality(status: number | null): TrendQuality {
  if (status === null || !Number.isInteger(status) || status < 0 || status > 0xffffffff) return 'unknown';
  const severity = status >>> 30;
  return severity === 0 ? 'good' : severity === 1 ? 'uncertain' : 'bad';
}

function causal(points: TrendPoint[], time: number, maxAge: number): TrendPoint | undefined {
  let lo = 0, hi = points.length;
  while (lo < hi) { const mid = (lo + hi) >>> 1; if (points[mid].time <= time) lo = mid + 1; else hi = mid; }
  const point = points[lo - 1];
  return point && time - point.time <= maxAge ? point : undefined;
}

function gaps(points: TrendPoint[], start: number, end: number, maxGap: number): TrendPoint[] {
  const result: TrendPoint[] = [];
  let previous: TrendPoint | undefined;
  for (const point of points) {
    if (point.time < start || point.time > end) continue;
    result.push(point); previous = point;
  }
  if (previous && end - previous.time > maxGap) result.push({ time: previous.time + maxGap, value: null, quality: 'missing' });
  return result;
}

export function historianTrendData(history: HistoryEnvelope, fields: HistoryField[], range: { start: number; end: number }): ProcessTrendData {
  const grouped = new Map<string, TrendPoint[]>();
  for (const raw of history.points) {
    const time = Date.parse(raw.timestamp);
    if (!Number.isFinite(time)) continue;
    const key = raw.node_id;
    const points = grouped.get(key) || [];
    const value = typeof raw.value === 'number' && Number.isFinite(raw.value) ? raw.value : null;
    points.push({ time, value, quality: value === null ? 'missing' : opcQuality(raw.status) });
    grouped.set(key, points);
  }
  for (const points of grouped.values()) points.sort((a, b) => a.time - b.time);
  const configs = new Map(fields.map(field => [field.key, field]));
  const semantic = new Map(fields.map(field => [JSON.stringify([field.role, field.base, field.field]), field]));
  const sourceField = (field: HistoryField, name: string) => semantic.get(JSON.stringify([field.role, field.base, name]));
  const series: TrendSeries[] = [], variables = new Map<string, TrendVariable>(), events: TrendEvent[] = [];
  const maxGap = Number.isFinite(history.gapThresholdMs) && history.gapThresholdMs > 0 ? history.gapThresholdMs : 1;
  const warnings = [...history.warnings];
  for (const field of [...fields].sort((a, b) => Number(b.pinned) - Number(a.pinned))) {
    if (!field.enabled) continue;
    const mode = field.field === 'Mode' || field.field === 'ModeTarget';
    const derived = isDiagnosticField(field.field);
    const separate = field.field === 'OP' || mode || derived;
    const reference = sourceField(field, 'PV');
    const unit = mode || field.field === 'SaturationFlag' ? '代碼' : field.unit || (field.field !== 'OP' ? reference?.unit : '') || '';
    const variableId = JSON.stringify([field.role, field.base, separate ? (mode ? 'Mode' : field.field) : null, unit]);
    const variable: TrendVariable = { id: variableId, role: mode || derived || field.field === 'OP' ? 'OTHER' : field.role,
      name: separate ? `${field.base} · ${mode ? 'Mode' : field.field}` : field.base, unit, description: separate ? field.alias : reference?.alias };
    variables.set(variableId, variable);
    let kind: TrendKind = 'actual';
    if (field.field === 'Prediction') kind = 'prediction';
    else if (field.field === 'FuturePlan') kind = 'plan';
    else if (field.field === 'Target') kind = 'target';
    else if (field.field === 'SP' || field.field === 'ModeTarget') kind = 'command';
    else if (field.field === 'LowLimit') kind = field.role === 'DV' ? 'valid_low' : 'operating_low';
    else if (field.field === 'HighLimit') kind = field.role === 'DV' ? 'valid_high' : 'operating_high';
    else if (field.field === 'SpecLow') kind = 'spec_low';
    else if (field.field === 'SpecHigh') kind = 'spec_high';
    let points = grouped.get(field.key) || [];
    let unavailableReason: string | undefined;
    if (kind === 'prediction' || kind === 'plan') {
      points = [];
      unavailableReason = '舊 historian 陣列未保存求解產生時間與取樣週期，無法可靠還原有效時間。請使用新研究執行的預測快照；不以接收時間冒充求解時間。';
    } else if (derived) {
      const sourceConfig = sourceField(field, field.field === 'SaturationFlag' ? 'SP' : 'PV');
      const targetConfig = sourceField(field, 'Target');
      const lowerConfig = sourceField(field, 'LowLimit');
      const upperConfig = sourceField(field, 'HighLimit');
      const source = sourceConfig ? grouped.get(sourceConfig.key) || [] : [];
      const target = targetConfig ? grouped.get(targetConfig.key) || [] : [];
      const lower = lowerConfig ? grouped.get(lowerConfig.key) || [] : [];
      const upper = upperConfig ? grouped.get(upperConfig.key) || [] : [];
      const dependencyFields = field.field === 'Error' ? [sourceConfig, targetConfig] : [sourceConfig, lowerConfig, upperConfig];
      const dependencyUnits = dependencyFields.map(config => config ? configs.get(config.key)?.unit || reference?.unit || '' : undefined);
      const compatible = dependencyUnits.every(value => value === dependencyUnits[0])
        && (field.field === 'SaturationFlag' || unit === dependencyUnits[0]);
      if (!compatible) unavailableReason = '診斷值的來源工程單位不一致；不進行未經換算的加減或比較。';
      points = source.map(point => {
        const a = causal(field.field === 'Error' ? target : lower, point.time, maxGap);
        const b = field.field === 'Error' ? undefined : causal(upper, point.time, maxGap);
        if (!compatible || point.quality !== 'good' || point.value === null || !a || a.quality !== 'good' || a.value === null
          || (field.field !== 'Error' && (!b || b.quality !== 'good' || b.value === null || b.value <= a.value))) {
          return { time: point.time, value: null, quality: 'missing' as const };
        }
        const value = field.field === 'Error' ? point.value - a.value : field.field === 'ConstraintDistance'
          ? Math.min(point.value - a.value, b!.value! - point.value)
          : Number((point.value - a.value) / (b!.value! - a.value) <= 0.02 || (point.value - a.value) / (b!.value! - a.value) >= 0.98);
        return { time: point.time, value, quality: 'good' as const };
      });
    }
    const displayed = gaps(points, range.start, range.end, maxGap);
    if (!displayed.length && !unavailableReason) unavailableReason = '此時間範圍沒有已記錄的數值。';
    series.push({ id: field.key, variableId, label: field.alias, kind, points: displayed, source: derived ? '由歷史有效樣本因果配對推導；不是獨立量測' : 'QuestDB · OPC UA 接收／記錄時間',
      interpolation: isLimitField(field.field) || ['Target', 'SP', 'Mode', 'ModeTarget'].includes(field.field) ? 'step' : 'linear',
      available: !unavailableReason, unavailableReason });
    for (let i = 0; i < displayed.length; i++) {
      const point = displayed[i], previous = displayed[i - 1];
      if ((point.quality === 'missing' || point.quality === 'bad') && point.quality !== previous?.quality) {
        events.push({ time: point.time, kind: 'gap', variableId, label: `${field.alias} · ${point.quality === 'bad' ? '品質不良' : '缺值／逾時'}` });
      } else if (previous && point.value !== previous.value && point.quality === 'good' && previous.quality === 'good') {
        const eventKind = mode ? 'mode' : isLimitField(field.field) ? 'limit' : field.field === 'Target' ? 'target' : null;
        if (eventKind) events.push({ time: point.time, kind: eventKind, variableId, label: `${field.alias}：${previous.value} → ${point.value}` });
      }
    }
  }
  if ([...variables.values()].some(variable => !variable.unit)) warnings.push('部分工程單位尚未設定；請在訊號設定填入來源單位。此設定只標示，不換算量測值。');
  if (series.some(item => item.source.startsWith('由歷史'))) warnings.push('診斷值使用已記錄樣本的因果配對，超過資料有效期限即保留缺口；不作為正式品質統計。');
  return { schemaVersion: 1, originTime: range.start, timeBasis: 'utc', variables: [...variables.values()], series, events: events.sort((a,b) => a.time-b.time), range,
    sampleTimeMs: null, rawSamples: history.rawSamples, displaySamples: series.reduce((count, item) => count + item.points.length, 0), warnings,
    provenance: 'QuestDB 已記錄的 OPC UA 資料；每個變數獨立尺度，不填補實際值；預測缺少時間中繼資料時標示未記錄。' };
}
