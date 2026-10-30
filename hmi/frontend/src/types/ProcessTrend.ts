export type TrendKind = 'actual' | 'prediction' | 'residual' | 'movement' | 'target' | 'command' | 'plan' | 'spec_low' | 'spec_high' | 'operating_low' | 'operating_high' | 'soft_low' | 'soft_high' | 'valid_low' | 'valid_high';
export type TrendQuality = 'good' | 'bad' | 'uncertain' | 'unknown' | 'missing';
export type TrendPoint = { time: number; value: number | null; quality: TrendQuality; issuedAt?: number };
export type TrendVariable = { id: string; role: 'CV' | 'MV' | 'DV' | 'OTHER'; name: string; unit: string; description?: string };
export type TrendGroup = { id: string; label: string; method?: string; variant?: string };
export type TrendSeries = { id: string; variableId: string; label: string; kind: TrendKind; points: TrendPoint[]; source: string; method?: string; groupId?: string; interpolation: 'linear' | 'step'; available: boolean; unavailableReason?: string; leadSeconds?: number; forecastScope?: 'latest' | 'historical' };
export type TrendEvent = { time: number; kind: 'limit' | 'target' | 'mode' | 'gap' | 'violation' | 'quality'; label: string; variableId?: string; groupId?: string };
export type TrendRange = { start: number; end: number };
export type TrendOverview = {
  range: TrendRange;
  series: { seriesId: string; variableId: string; groupId?: string; label: string;
    points: { time: number; start: number; end: number; min: number | null; max: number | null; median: number | null; count: number; goodCount: number }[] }[];
  splits: { name: string; start: number; end: number }[];
};
export type TrendWindowSummary = {
  range: TrendRange; basis: string;
  rows: {
    seriesId: string; variableId: string; groupId?: string; samples: number; goodSamples: number;
    mean: number | null; variance: number | null; stddev: number | null;
    bias: number | null; biasSamples: number;
    specViolationSeconds: number | null; specObservedSeconds: number;
    operatingViolationSeconds: number | null; operatingObservedSeconds: number;
    commandMovement: number | null; commandPairs: number;
  }[];
};
export type ProcessTrendData = {
  schemaVersion: 1; originTime: number; timeBasis?: 'utc' | 'relative';
  variables: TrendVariable[]; series: TrendSeries[]; events: TrendEvent[]; range: TrendRange;
  sampleTimeMs: number | null; rawSamples: number; displaySamples: number; warnings: string[]; provenance: string;
  currentTime?: number;
  groups?: TrendGroup[]; overview?: TrendOverview; windowSummary?: TrendWindowSummary;
};
