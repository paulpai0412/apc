import { historianTrendData } from './historianTrends';
import type { HistoryEnvelope, HistoryField, HistoryPoint } from './historianTrends';
import { decodeModelBinding } from './modelBinding';
import type { TwinControllerVariables } from '../types/EquipmentView';
import type { DeploymentProfile, RuntimeProfileSnapshot, RuntimeSignal } from '../types/RuntimeWorkspace';
import type { ProcessTrendData, TrendPoint } from '../types/ProcessTrend';
import type { OpcUpdate } from '../store/tagStore';

export function runtimeControllerVariables(profile: DeploymentProfile): TwinControllerVariables {
  const binding = decodeModelBinding(profile.model_binding);
  const variables: TwinControllerVariables = { cvs: [], mvs: [], dvs: [] };
  for (const role of ['cv', 'mv', 'dv'] as const) {
    for (const variable of binding[`${role}s`]) {
      const authority = profile.variables.find(item => item.role === role.toUpperCase() && item.id === variable.name);
      if (!authority || authority.unit !== variable.units || authority.pv_tag !== variable.node_ids.pv) throw new Error(`核准 profile 與模型映射不一致：${variable.name}`);
      variables[`${role}s`].push({ name: variable.name, units: variable.units, node_ids: { ...variable.node_ids, limits: { ...variable.node_ids.limits } }, limits: { low: authority.hard_low, high: authority.hard_high } });
    }
  }
  return variables;
}

export function runtimeTags(signals: RuntimeSignal[]): Record<string, OpcUpdate> {
  const tags: Record<string, OpcUpdate> = {};
  for (const signal of signals) {
    const value = signal.value;
    if (typeof value === 'string' || typeof value === 'boolean' || (typeof value === 'number' && Number.isFinite(value))
      || (Array.isArray(value) && value.every(item => typeof item === 'number' && Number.isFinite(item)))) {
      tags[signal.node_id] = { node_id: signal.node_id, value, timestamp: signal.timestamp, status: signal.status };
    }
  }
  return tags;
}

export function appendRuntimeSamples(previous: ReadonlyMap<string, HistoryPoint[]>, signals: RuntimeSignal[], capacity: number): Map<string, HistoryPoint[]> {
  const next = new Map(previous);
  for (const signal of signals) {
    if (!Number.isFinite(Date.parse(signal.timestamp)) || typeof signal.value !== 'number') continue;
    const point: HistoryPoint = { node_id: signal.node_id, timestamp: signal.timestamp, value: Number.isFinite(signal.value) ? signal.value : null, status: signal.status };
    const points = next.get(signal.node_id) ?? [];
    const last = points.at(-1);
    const timestamp = Date.parse(signal.timestamp);
    if (last && timestamp < Date.parse(last.timestamp)) continue;
    if (last && timestamp === Date.parse(last.timestamp)) {
      if (last.value !== point.value || last.status !== point.status) {
        // One source instant cannot prove two different observations. Only a new instant clears the gap.
        next.set(signal.node_id, [...points.slice(0, -1), { ...last, value: null, status: 0x80000000 }]);
      }
    } else next.set(signal.node_id, [...(capacity > 1 ? points.slice(1 - capacity) : []), point]);
  }
  return next;
}

/** Profile-scoped engineering coordinates shared by recorded and live observation views. */
export function runtimeHistoryData(profile: DeploymentProfile, history: HistoryEnvelope,
  range: { start: number; end: number }, sampledTags: Readonly<Record<string, string>> = {}): ProcessTrendData {
  const fields: HistoryField[] = [];
  for (const variable of profile.variables) {
    const channels = variable.role === 'CV'
      ? [['PV', variable.pv_tag, '實際'], ['Target', variable.target_tag, '目標'], ['LowLimit', variable.low_tag, '操作下限'], ['HighLimit', variable.high_tag, '操作上限']]
      : variable.role === 'MV'
        ? [['PV', variable.pv_tag, '製程量測'], ['AppliedSP', variable.applied_tag, '已應用 SP'], ['SP', variable.command_tag, 'SP 命令暫存器'], ['OP', variable.op_tag, 'PID OP'], ['LowLimit', variable.low_tag, '操作下限'], ['HighLimit', variable.high_tag, '操作上限']]
        : [['PV', variable.pv_tag, '實際'], ['LowLimit', variable.low_tag, '有效下限'], ['HighLimit', variable.high_tag, '有效上限']];
    for (const [field, key, label] of channels) {
      if (!key || !field) continue;
      const sourceKey = ['Target', 'LowLimit', 'HighLimit'].includes(field) && Object.hasOwn(sampledTags, key) ? sampledTags[key] : key;
      fields.push({ key: sourceKey, base: variable.id, field, role: variable.role, alias: `${variable.id} · ${label}`, unit: field === 'OP' ? '' : variable.unit, enabled: true, pinned: false });
    }
  }
  const data = historianTrendData(history, fields, range);
  data.provenance = '核准 profile 的指定來源封存；使用原始來源時間，不填補實際值。顯示縮減不作為正式績效統計。';
  for (const series of data.series) series.source = 'QuestDB · 核准 OPC 來源與原始來源時間';
  return data;
}

/** Advisory display only. This data never authorizes or acknowledges a physical command. */
export function runtimeTrendData(profile: DeploymentProfile, snapshot: RuntimeProfileSnapshot | undefined,
  history: ReadonlyMap<string, HistoryPoint[]>, now: number, connected: boolean): ProcessTrendData {
  const binding = decodeModelBinding(snapshot?.active_model_binding ?? profile.model_binding);
  const points = [...history.values()].flat();
  let first = now;
  for (const point of points) first = Math.min(first, Date.parse(point.timestamp));
  const data = runtimeHistoryData(profile, { points, rawSamples: points.length, gapThresholdMs: profile.maximum_age_ms,
    warnings: connected ? [] : ['現場觀測連線中斷；最後值保留但不視為即時。'] },
    { start: Math.min(first, now - binding.sample_time * 1000), end: now });
  data.provenance = '已核准 profile 的現場 OPC 觀測；本分頁有界觀察緩衝，不是完整歷史封存或正式績效統計。';
  data.sampleTimeMs = binding.sample_time * 1000;
  data.currentTime = now;
  for (const series of data.series) series.source = '現場 OPC · 原始來源時間；非寫入／應用確認';
  const signal = snapshot?.signals.find(item => item.node_id === profile.forecast_tag);
  let forecast: Record<string, unknown> | null = null;
  try { const parsed: unknown = typeof signal?.value === 'string' ? JSON.parse(signal.value) : null; if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) forecast = parsed as Record<string, unknown>; } catch { /* Missing or invalid forecast stays unavailable. */ }
  const issuedAt = typeof forecast?.issued_at === 'string' ? Date.parse(forecast.issued_at) : NaN;
  const native = snapshot?.native_status;
  const acquiredAt = typeof native?.acquired_at === 'string' ? Date.parse(native.acquired_at) : NaN;
  const maximumSolveAge = native?.maximum_solve_age_ms;
  const vector = (value: unknown, size: number): value is number[] => Array.isArray(value) && value.length === size && value.every(item => typeof item === 'number' && Number.isFinite(item));
  const valid = connected && snapshot?.health === 'good' && snapshot.active_model_binding
    && native?.healthy === true && Number.isSafeInteger(maximumSolveAge) && Number(maximumSolveAge) > 0
    && Number.isFinite(acquiredAt) && acquiredAt <= now + profile.maximum_skew_ms && now - acquiredAt <= profile.maximum_age_ms
    && native.observed_at === forecast?.issued_at && native.forecast_sequence === forecast?.sequence
    && signal && Number.isInteger(signal.status) && signal.status >= 0 && signal.status <= 0xffffffff && (signal.status >>> 30) === 0
    && forecast?.schema_version === 1 && forecast.source === 'engine_solver' && forecast.time_basis === 'utc'
    && forecast.engine_id === profile.engine_id && forecast.controller_sha256 === snapshot?.version
    && typeof forecast.instance_id === 'string' && forecast.instance_id === snapshot.instance_id && Number.isSafeInteger(forecast.sequence) && Number(forecast.sequence) > 0
    && forecast.layout === 'step-major' && forecast.cv_first_offset_steps === 1 && forecast.mv_first_offset_steps === 0
    && forecast.sample_time === binding.sample_time && forecast.cv_count === binding.cvs.length && forecast.mv_count === binding.mvs.length
    && forecast.prediction_horizon === binding.prediction_horizon && forecast.control_horizon === binding.control_horizon
    && Number.isFinite(issuedAt) && issuedAt <= now + profile.maximum_skew_ms && now - issuedAt <= Number(maximumSolveAge)
    && vector(forecast.cv, binding.cvs.length * binding.prediction_horizon)
    && vector(forecast.mv_base, binding.mvs.length) && vector(forecast.mv_delta, binding.mvs.length * binding.control_horizon);
  for (const role of ['CV', 'MV'] as const) {
    const variables = role === 'CV' ? binding.cvs : binding.mvs;
    variables.forEach((variable, index) => {
      const target = data.variables.find(item => item.role === role && item.name === variable.name && item.unit === variable.units);
      if (!target) return;
      const forecastPoints: TrendPoint[] = [];
      if (valid && forecast) {
        let command = (forecast.mv_base as number[])[index];
        const count = role === 'CV' ? binding.prediction_horizon : binding.control_horizon;
        for (let step = 0; step < count; step++) {
          if (role === 'MV') command += (forecast.mv_delta as number[])[step * binding.mvs.length + index];
          forecastPoints.push({ time: issuedAt + (step + (role === 'CV' ? 1 : 0)) * binding.sample_time * 1000,
            value: role === 'CV' ? (forecast.cv as number[])[step * binding.cvs.length + index] : command, quality: 'good', issuedAt });
        }
      }
      data.series.push({ id: `${profile.profile_hash}:forecast:${role}:${variable.name}`, variableId: target.id,
        label: role === 'CV' ? `${variable.name} · 完整預測` : `${variable.name} · 未來 MV 計畫`,
        kind: role === 'CV' ? 'prediction' : 'plan', points: forecastPoints, source: '原生引擎完整求解時域；不是已派送命令',
        interpolation: role === 'CV' ? 'linear' : 'step', available: !!valid, forecastScope: 'latest',
        ...(!valid ? { unavailableReason: '未取得同模型、有效時刻與品質的完整原生預測。' } : {}),
      });
      if (forecastPoints.length) data.range.end = Math.max(data.range.end, forecastPoints[forecastPoints.length - 1].time);
    });
  }
  data.displaySamples = data.series.reduce((sum, series) => sum + series.points.length, 0);
  return data;
}
