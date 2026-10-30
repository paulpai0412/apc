import type { TwinArchiveFrame, TwinForecast, TwinManifest, TwinSession } from '../types/DigitalTwin';
import type { ProcessTrendData, TrendKind, TrendQuality, TrendSeries } from '../types/ProcessTrend';

type TwinTrendSnapshot = Pick<TwinSession, 'id' | 'run_id' | 'trial_id' | 'samples' | 'sample_time' | 'start' | 'time_basis' | 'frame' | 'recent_trace' | 'latest_forecast' | 'settings_version_id' | 'variables' | 'controller_sha256' | 'method'>;

export function twinQuality(value: number | undefined): TrendQuality {
  if (value === undefined || !Number.isInteger(value) || value < 0 || value > 0xffff) return 'unknown';
  const major = value & 0xc0;
  return major === 0xc0 ? 'good' : major === 0x40 ? 'uncertain' : 'bad';
}

export function sessionCompatibility(a: TwinSession, b: TwinSession): string | null {
  if (a.status !== 'completed' || b.status !== 'completed') return '僅完成的工作階段可比較完整封存。';
  if (a.run_id !== b.run_id) return '不同研究的變數契約無法確認一致。';
  if (!a.plant_sha256 || a.plant_sha256 !== b.plant_sha256) return '被控模型雜湊不同。';
  if (!a.scenario_sha256 || a.scenario_sha256 !== b.scenario_sha256) return '情境雜湊不同（初始狀態、擾動或資料區間不同）。';
  if (a.time_basis !== b.time_basis || a.start !== b.start || a.sample_time !== b.sample_time || a.requested_samples !== b.requested_samples || a.source_offset_seconds !== b.source_offset_seconds) return '時間起點、取樣週期或情境長度不同。';
  return null;
}

/** Reject late/cross-source snapshots; equal sequence is a read-only refresh. */
export function acceptsTwinSnapshot(previous: TwinSession | null, next: TwinSession, id: string, runId: string): boolean {
  if (next.id !== id || next.run_id !== runId || !Number.isSafeInteger(next.samples) || next.samples < 0) return false;
  if (next.sequence !== undefined && (!Number.isSafeInteger(next.sequence) || next.sequence < 0)) return false;
  if (next.native_sequence !== undefined && (!Number.isSafeInteger(next.native_sequence) || next.native_sequence < 0)) return false;
  if (next.frame && next.recent_trace?.time_seconds.length) {
    if (next.frame.time_seconds !== next.recent_trace.time_seconds.at(-1)) return false;
    for (const field of ['cv', 'mv', 'dv', 'targets', 'command_mv', 'predicted_cv', 'quality'] as const) {
      const row = next.recent_trace[field]?.at(-1), values = next.frame[field];
      if (!row || row.length !== values.length || row.some((value, index) => value !== values[index])) return false;
    }
  }
  if (!previous) return true;
  if (next.trial_id !== previous.trial_id || next.controller_sha256 !== previous.controller_sha256
    || next.plant_sha256 !== previous.plant_sha256 || next.scenario_sha256 !== previous.scenario_sha256 || next.binary_sha256 !== previous.binary_sha256
    || next.sample_time !== previous.sample_time || next.start !== previous.start || next.time_basis !== previous.time_basis
    || next.source_offset_seconds !== previous.source_offset_seconds || next.requested_samples !== previous.requested_samples
    || next.equipment_view?.id !== previous.equipment_view?.id || next.settings_version_id !== previous.settings_version_id
    || next.samples < previous.samples
    || (previous.native_sequence !== undefined && (next.native_sequence === undefined || next.native_sequence < previous.native_sequence))) return false;
  return previous.sequence === undefined || (next.sequence !== undefined && next.sequence >= previous.sequence
    && (next.sequence !== previous.sequence || (next.samples === previous.samples && next.native_sequence === previous.native_sequence)));
}

// Native arrays are step-major. A horizon belongs to exactly one completed scan,
// never to receipt time, source-window offset, or a later snapshot request.
function latestForecast(manifest: TwinManifest, session: TwinTrendSnapshot): TwinForecast | null {
  const forecast = session.latest_forecast;
  const variables = session.variables ?? manifest.variables;
  const trial = manifest.trials.find(item => item.id === session.trial_id);
  if (!forecast || !session.controller_sha256 || session.run_id !== manifest.run_id || !trial ||
      (!session.settings_version_id && trial.controller_sha256 && trial.controller_sha256 !== session.controller_sha256)) return null;
  if ((['cv', 'mv', 'dv'] as const).some(role =>
    variables[role].length !== manifest.variables[role].length || variables[role].some((variable, index) =>
      variable.name !== manifest.variables[role][index].name || variable.unit !== manifest.variables[role][index].unit))) return null;
  if (forecast.schema_version !== 1 || forecast.layout !== 'step-major' ||
      forecast.cv_first_offset_steps !== 1 || forecast.mv_first_offset_steps !== 0 ||
      forecast.cv_count !== variables.cv.length || forecast.mv_count !== variables.mv.length ||
      !Number.isSafeInteger(forecast.prediction_horizon) || forecast.prediction_horizon < 1 ||
      !Number.isSafeInteger(forecast.control_horizon) || forecast.control_horizon < 1 ||
      forecast.control_horizon > forecast.prediction_horizon ||
      !Number.isSafeInteger(forecast.sample) || forecast.sample < 0 || forecast.sample !== session.samples - 1 ||
      !Number.isFinite(forecast.sample_time) || forecast.sample_time <= 0 || forecast.sample_time !== session.sample_time ||
      !Number.isFinite(forecast.issued_at_seconds) || forecast.issued_at_seconds < 0 ||
      forecast.issued_at_seconds !== session.frame?.time_seconds ||
      forecast.issued_at_seconds !== session.recent_trace?.time_seconds.at(-1)) return null;
  const finiteVector = (values: number[], length: number) => Array.isArray(values) && values.length === length && values.every(Number.isFinite);
  if (!finiteVector(forecast.cv, forecast.prediction_horizon * forecast.cv_count) ||
      !finiteVector(forecast.mv_base, forecast.mv_count) ||
      !finiteVector(forecast.mv_delta, forecast.control_horizon * forecast.mv_count)) return null;
  if (!Number.isFinite((forecast.issued_at_seconds + forecast.prediction_horizon * forecast.sample_time) * 1000)) return null;
  for (let variable = 0; variable < forecast.mv_count; variable += 1) {
    let command = forecast.mv_base[variable];
    for (let step = 0; step < forecast.control_horizon; step += 1) {
      command += forecast.mv_delta[step * forecast.mv_count + variable];
      if (!Number.isFinite(command)) return null;
    }
  }
  return forecast;
}

// Native traces contain observations at t and one-step forecasts issued at t, valid at t + Ts.
// No interpolation, synthetic observations, or invented quality is added here.
export function recentTwinTrend(manifest: TwinManifest, session: TwinTrendSnapshot): ProcessTrendData | null {
  const trace = session.recent_trace;
  if (!trace?.time_seconds.length || !Number.isFinite(session.sample_time) || session.sample_time <= 0) return null;
  const originTime = session.time_basis === 'relative' ? 0 : Date.parse(session.start);
  if (!Number.isFinite(originTime)) return null;
  const sampleTimeMs = session.sample_time * 1000;
  const settings = session.variables ?? manifest.variables;
  const forecast = latestForecast(manifest, session);
  const currentTime = originTime + trace.time_seconds[trace.time_seconds.length - 1] * 1000;
  if (!Number.isFinite(currentTime)) return null;
  const forecastEnd = forecast ? originTime + (forecast.issued_at_seconds + forecast.prediction_horizon * forecast.sample_time) * 1000 : currentTime;
  const rangeEnd = Math.max(currentTime + sampleTimeMs, forecastEnd);
  const variables = (['cv', 'mv', 'dv'] as const).flatMap(role => settings[role].map((variable, index) => ({
    id: `${role}:${index}`, name: variable.name, unit: variable.unit, role: role.toUpperCase() as 'CV' | 'MV' | 'DV',
  })));
  const groupId = session.id;
  const series: TrendSeries[] = [];
  let qualityOffset = 0;
  for (const role of ['cv', 'mv', 'dv'] as const) {
    settings[role].forEach((variable, index) => {
      const variableId = `${role}:${index}`;
      const fields: { field: 'cv' | 'mv' | 'dv' | 'targets' | 'command_mv' | 'predicted_cv'; kind: TrendKind; label: string }[] = [{ field: role, kind: 'actual', label: '實際' }];
      if (role === 'cv') fields.push({ field: 'targets', kind: 'target', label: '目標' }, { field: 'predicted_cv', kind: 'prediction', label: '一步預測' });
      if (role === 'mv') fields.push({ field: 'command_mv', kind: 'command', label: '施加指令' });
      for (const { field, kind, label } of fields) {
        const forecast = kind === 'prediction';
        series.push({
          id: `${groupId}:${variableId}:${kind}`, variableId, groupId, kind, label: `${variable.name} · ${label}`,
          source: `模擬工作階段 ${session.id}`, interpolation: role === 'cv' && kind === 'actual' ? 'linear' : 'step',
          available: trace[field]?.some(row => Number.isFinite(row?.[index])) ?? false,
          ...(forecast ? { leadSeconds: session.sample_time, forecastScope: 'historical' as const } : {}),
          points: trace.time_seconds.map((seconds, row) => {
            const issuedAt = originTime + seconds * 1000;
            const value = trace[field]?.[row]?.[index];
            return {
              time: issuedAt + (forecast ? sampleTimeMs : 0), value: Number.isFinite(value) ? value : null,
              quality: Number.isFinite(value) ? twinQuality(trace.quality?.[row]?.[qualityOffset + index]) : 'missing',
              ...(forecast ? { issuedAt } : {}),
            };
          }),
        });
      }
      if (forecast && (role === 'cv' || role === 'mv')) {
        const issuedAt = originTime + forecast.issued_at_seconds * 1000;
        let command = forecast.mv_base[index];
        const points = Array.from({ length: role === 'cv' ? forecast.prediction_horizon : forecast.control_horizon }, (_, step) => {
          if (role === 'mv') command += forecast.mv_delta[step * forecast.mv_count + index];
          return {
            time: issuedAt + (step + (role === 'cv' ? forecast.cv_first_offset_steps : forecast.mv_first_offset_steps)) * sampleTimeMs,
            value: role === 'cv' ? forecast.cv[step * forecast.cv_count + index] : command,
            quality: 'good' as const, issuedAt,
          };
        });
        series.push({
          id: `${groupId}:${variableId}:latest-${role === 'cv' ? 'forecast' : 'plan'}`,
          variableId, groupId, kind: role === 'cv' ? 'prediction' : 'plan', forecastScope: 'latest',
          label: `${variable.name} · ${role === 'cv' ? '最新完整預測' : '最新累積 MV 計畫（非實際）'}`,
          method: session.method ?? manifest.trials.find(item => item.id === session.trial_id)?.method,
          source: `原生求解 sample ${forecast.sample} · 方法 ${session.method ?? manifest.trials.find(item => item.id === session.trial_id)?.method ?? '未記錄'} · P=${forecast.prediction_horizon} / M=${forecast.control_horizon} · controller SHA-256 ${session.controller_sha256}`,
          interpolation: role === 'cv' ? 'linear' : 'step', available: true, points,
        });
      }
      for (const [kind, value] of [['operating_low', variable.low], ['operating_high', variable.high], ['spec_low', variable.spec_low], ['spec_high', variable.spec_high]] as const) {
        if (value === undefined || !Number.isFinite(value)) continue;
        series.push({ id: `${groupId}:${variableId}:${kind}`, variableId, groupId, kind, label: `${variable.name} · ${kind}`, source: session.variables ? '工作階段凍結工程設定' : '研究工程設定（歷史工作階段未封存獨立設定）', interpolation: 'step', available: true,
          points: [...new Set([...trace.time_seconds.map(seconds => originTime + seconds * 1000), rangeEnd])].map(time => ({ time, value, quality: 'good' as const })) });
      }
    });
    qualityOffset += settings[role].length;
  }
  return {
    schemaVersion: 1, originTime, timeBasis: session.time_basis, variables, series, events: [], sampleTimeMs, currentTime,
    range: { start: originTime + trace.time_seconds[0] * 1000, end: rangeEnd },
    rawSamples: trace.time_seconds.length, displaySamples: trace.time_seconds.length,
    groups: [{ id: groupId, label: `${session.trial_id} · ${session.id}`, variant: 'recent' }],
    warnings: ['僅最近最多 256 個真實完成掃描；不是完整工作階段統計。歷史一步預測有效時間為發出時間 + Ts；最新完整預測與累積 MV 計畫是原生求解結果，不是未來實際值。',
      ...(session.latest_forecast && !forecast ? ['最新預測與工作階段身分、形狀或掃描時間不相容；已拒絕顯示。'] : [])],
    provenance: `隔離模擬 · ${session.id} · controller SHA-256 ${session.controller_sha256}`,
  };
}

/** One archived native solve, never a new simulation or browser-computed prediction. */
export function archivedTwinTrend(manifest: TwinManifest, archive: TwinArchiveFrame, sourceId: string, trialId: string, settingsVersionId?: string | null): ProcessTrendData | null {
  const frame = archive.frame;
  return recentTwinTrend(manifest, {
    id: sourceId, run_id: manifest.run_id, trial_id: trialId, samples: archive.sample + 1,
    sample_time: archive.sample_time, start: archive.start, time_basis: archive.time_basis,
    controller_sha256: archive.controller_sha256, variables: archive.variables, settings_version_id: settingsVersionId,
    frame, latest_forecast: archive.latest_forecast,
    recent_trace: { time_seconds: [frame.time_seconds], cv: [frame.cv], mv: [frame.mv], dv: [frame.dv],
      targets: [frame.targets], command_mv: [frame.command_mv], predicted_cv: [frame.predicted_cv],
      quality: [frame.quality], solver_status: [frame.solver_status] },
  });
}
