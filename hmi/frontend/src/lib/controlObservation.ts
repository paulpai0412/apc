export interface ObservedCv {
  name: string;
  unit: string;
  pv: number;
  target: number;
  low: number;
  high: number;
  variance: number | null;
  tracking_error_variance: number | null;
  bias: number | null;
  rmse: number | null;
  operating_violation_samples: number;
}

export interface ControlObservation {
  schema_version: 1;
  source: 'engine_opc_snapshot';
  time_basis: 'engine_receipt';
  observed_at: string;
  engine_id: string;
  sample_time_seconds: number;
  requested_mode: 0 | 1 | 2;
  samples: number;
  window_samples: number;
  complete: boolean;
  cv: ObservedCv[];
  mv: { name: string; unit: string; applied_sp: number; total_variation: number }[];
  dv: { name: string; unit: string; pv: number }[];
}

type ParseResult =
  | { observation: ControlObservation; error: null }
  | { observation: null; error: string };

type Unparsed<T> = { [Key in keyof T]?: unknown };
const isFiniteNumber = (value: unknown): value is number =>
  typeof value === 'number' && Number.isFinite(value);
const isCount = (value: unknown): value is number =>
  isFiniteNumber(value) && Number.isSafeInteger(value) && value >= 0;
const isNullableNumber = (value: unknown): value is number | null =>
  value === null || isFiniteNumber(value);
const isNullableNonnegative = (value: unknown): value is number | null =>
  value === null || (isFiniteNumber(value) && value >= 0);
const hasIdentity = (value: Unparsed<{ name: string; unit: string }>) =>
  typeof value.name === 'string' && value.name.trim().length > 0 && typeof value.unit === 'string';

function isUtcTimestamp(value: unknown): value is string {
  if (typeof value !== 'string' || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|\+00:00)$/.test(value)) return false;
  const milliseconds = Date.parse(value);
  // Also reject dates that Date.parse silently normalizes (for example February 30).
  return Number.isFinite(milliseconds) && new Date(milliseconds).toISOString().slice(0, 19) === value.slice(0, 19);
}

function isObservation(input: unknown): input is ControlObservation {
  if (typeof input !== 'object' || input === null || Array.isArray(input)) return false;
  const value = input as Unparsed<ControlObservation>;
  if (value.schema_version !== 1
    || value.source !== 'engine_opc_snapshot'
    || value.time_basis !== 'engine_receipt'
    || !isUtcTimestamp(value.observed_at)
    || typeof value.engine_id !== 'string' || value.engine_id.length === 0
    || !isFiniteNumber(value.sample_time_seconds) || value.sample_time_seconds <= 0
    || !Number.isFinite(value.sample_time_seconds * 3000)
    || (value.requested_mode !== 0 && value.requested_mode !== 1 && value.requested_mode !== 2)
    || !isCount(value.samples) || !isCount(value.window_samples) || value.window_samples < 2
    || value.samples > value.window_samples
    || typeof value.complete !== 'boolean' || value.complete !== (value.samples === value.window_samples)
    || !Array.isArray(value.cv) || !Array.isArray(value.mv) || !Array.isArray(value.dv)) return false;

  const samples = value.samples;
  const cvsValid = value.cv.every((input: unknown) => {
    if (typeof input !== 'object' || input === null || Array.isArray(input)) return false;
    const cv = input as Unparsed<ObservedCv>;
    return hasIdentity(cv) && isFiniteNumber(cv.pv) && isFiniteNumber(cv.target)
      && isFiniteNumber(cv.low) && isFiniteNumber(cv.high) && cv.low <= cv.high
      && isNullableNonnegative(cv.variance) && isNullableNonnegative(cv.tracking_error_variance)
      && isNullableNumber(cv.bias) && isNullableNonnegative(cv.rmse)
      && isCount(cv.operating_violation_samples) && cv.operating_violation_samples <= samples;
  });
  const mvsValid = value.mv.every((input: unknown) => {
    if (typeof input !== 'object' || input === null || Array.isArray(input)) return false;
    const mv = input as Unparsed<ControlObservation['mv'][number]>;
    return hasIdentity(mv) && isFiniteNumber(mv.applied_sp)
      && isFiniteNumber(mv.total_variation) && mv.total_variation >= 0;
  });
  const dvsValid = value.dv.every((input: unknown) => {
    if (typeof input !== 'object' || input === null || Array.isArray(input)) return false;
    const dv = input as Unparsed<ControlObservation['dv'][number]>;
    return hasIdentity(dv) && isFiniteNumber(dv.pv);
  });
  return cvsValid && mvsValid && dvsValid;
}

export function parseControlObservation(value: unknown, engineId: string): ParseResult {
  if (typeof value !== 'string' || value.trim() === '') {
    return { observation: null, error: '尚未收到有效的觀測資料。' };
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    return { observation: null, error: '觀測資料不是有效的 JSON。' };
  }
  if (typeof parsed === 'object' && parsed !== null && !Array.isArray(parsed)
    && 'state' in parsed && parsed.state === 'unavailable') {
    const status = parsed as Record<string, unknown>;
    if (status.schema_version !== 1 || status.source !== 'engine_opc_snapshot'
      || status.time_basis !== 'engine_receipt' || !isUtcTimestamp(status.observed_at)
      || status.engine_id !== engineId || typeof status.reason !== 'string' || !status.reason.trim()) {
      return { observation: null, error: '觀測暫停狀態格式或控制器身分不符。' };
    }
    return { observation: null, error: `觀測暫停：${status.reason}` };
  }
  if (!isObservation(parsed)) {
    return { observation: null, error: '觀測格式、時間或數值無效。' };
  }
  if (parsed.engine_id !== engineId) {
    return { observation: null, error: '觀測資料的控制器身分不符。' };
  }
  return { observation: parsed, error: null };
}

export function hasGoodOpcQuality(status: number): boolean {
  return Number.isInteger(status) && status >= 0 && status <= 0xffffffff && (status & 0xc0000000) === 0;
}

// Display freshness only: this must never become a control/command permission check.
export const observationFreshnessMs = (sampleTimeSeconds: number): number =>
  Math.max(10_000, 3 * sampleTimeSeconds * 1000);
export const OBSERVATION_FUTURE_TOLERANCE_MS = 5_000;
