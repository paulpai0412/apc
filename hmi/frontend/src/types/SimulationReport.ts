export type ReportValue = string | number | boolean | null | ReportValue[] | { [key: string]: ReportValue };

export interface SimulationReportSummary {
  id: string;
  title: string;
  created_at: string;
  status: string;
  start: string;
  end: string;
  sample_time: number;
  samples: number;
  synthetic: boolean;
}

export interface ReportVariable {
  name: string;
  unit: string;
  low?: number | null;
  high?: number | null;
  target?: number | null;
  max_move?: number | null;
}

export interface ReportMetrics {
  score?: number | null;
  cv_rmse?: (number | null)[];
  cv_iae?: (number | null)[];
  cv_violation_seconds?: (number | null)[];
  mv_total_variation?: (number | null)[];
  mv_hard_violations?: (number | null)[];
  solver_calls?: number | null;
  solver_failures?: number | null;
  mean_solve_ms?: number | null;
  max_solve_ms?: number | null;
}

export interface ReportRun {
  id: string;
  label: string;
  kind: 'baseline' | 'optimized' | 'transport';
  transport: string;
  status: string;
  reason?: string;
  metrics?: ReportMetrics | null;
  test_metrics?: ReportMetrics | null;
  samples: number;
  trace_file?: string | null;
}

export interface SimulationReport {
  schema_version: 1;
  id: string;
  title: string;
  created_at: string;
  status: string;
  synthetic: boolean;
  description: string;
  period: { start: string; end: string; sample_time: number; samples: number };
  provenance: Record<string, ReportValue>;
  splits: { name: string; start: string; end: string; samples: number }[];
  variables: { cv: ReportVariable[]; mv: ReportVariable[]; dv: ReportVariable[] };
  identification: Record<string, ReportValue>;
  runs: ReportRun[];
  trials: { id: string; parameters: Record<string, ReportValue>; score: number | null; status: string; reason?: string }[];
  checks: { name: string; status: 'passed' | 'failed' | 'not_run'; detail: string }[];
  limitations: string[];
  artifacts: { name: string; path: string; sha256: string; bytes: number }[];
}

export interface ReportSeries {
  run_id: string;
  total_samples: number;
  returned_samples: number;
  downsampled: boolean;
  rows: {
    timestamp: string;
    time_seconds: number;
    cv: number[];
    mv: number[];
    dv: number[];
    targets: number[];
    command_mv?: number[];
  }[];
}
