export type ResearchValue = null | boolean | number | string | ResearchValue[] | { [key: string]: ResearchValue };
export type SignalRole = 'cv' | 'mv' | 'dv';
export type DatasetSource = { kind: 'file' } | {
  kind: 'postgres'; source_id: string; schema: string; table: string; timestamp_column: string;
  columns: string[]; start: string; end: string; read_at: string; row_count: number;
} | {
  kind: 'captured_acquisition';
  manifest: { schema_version: 1; source_id: string; captured_at: string; start: string; end: string; format: 'csv' | 'history'; sha256: string; read_only: true };
} | {
  kind: 'generated_synthetic'; run_id: string; history_sha256: string; receipt_sha256: string; config_sha256: string;
};
export type ResearchSource = {
  id: string; name: string; description: string; timestamp_column: string; columns: string[]; max_rows: number;
};
export type Dataset = {
  id: string; name: string; kind: 'raw' | 'clean'; columns: string[]; row_count: number;
  suggested_signals?: SignalMapping[]; sample_time?: number;
  created_at: string; source_id?: string; usable?: boolean;
  source?: DatasetSource;
};
export type SignalMapping = {
  column: string; name: string; role: SignalRole; unit: string; scale: number; offset: number;
  quality_column?: string; quality_format?: 'da' | 'ua' | 'boolean'; min?: number; max?: number;
};
export type Cleaning = {
  time_column: string; time_format: 'iso' | 'seconds'; sample_time: number; train_end: number;
  validation_end: number; duplicate_policy: 'first' | 'last' | 'mean';
  missing_policy: 'reject' | 'forward_fill' | 'linear'; max_gap_seconds: number;
  outlier_policy: 'none' | 'bounds' | 'train_sigma'; sigma: number; signals: SignalMapping[];
  regimes?: { start: number; end: number; label: string; exclude: boolean; reason: string }[];
};
export type CleaningPreview = {
  dataset_id: string; dataset?: Dataset; summary: Record<string, ResearchValue>; warnings: string[];
  usable: boolean; series: { time: number; raw: Record<string, number | null>; clean: Record<string, number | null> }[];
  signals: { name: string; role: SignalRole; unit: string }[]; cleaning: Cleaning;
  exposure?: { independent: boolean; status: string; events: ResearchValue; reason: string };
};
export type ResearchNodeIds = {
  pv?: string; target?: string; prediction?: string; sp?: string; applied_sp?: string;
  op?: string; mode?: string; mode_target?: string; future_plan?: string;
  limits?: { low?: string; high?: string; ll?: string; hh?: string };
};
export type EngineeringVariable = {
  name: string; unit: string; low: number; high: number; target?: number; weight?: number;
  alpha?: number; slack_weight?: number; max_move?: number; weight_r?: number;
  spec_low?: number; spec_high?: number;
  hard_low?: number; hard_high?: number; source_column?: string;
  measurement_kind?: 'fast' | 'delayed_lab' | 'estimated'; measurement_delay_seconds?: number;
  node_ids?: ResearchNodeIds;
};
export type Campaign = {
  [key: string]: ResearchValue | { cv: EngineeringVariable[]; mv: EngineeringVariable[]; dv: EngineeringVariable[] } | undefined;
  variables: { cv: EngineeringVariable[]; mv: EngineeringVariable[]; dv: EngineeringVariable[] };
  sample_time: number; start: string; months?: number;
  data_window?: { samples: number; train_end: number; validation_end: number };
};
export type ResearchObjective = {
  schema_version: number; scenarios: string[];
  coefficients: { variance: number; bias: number; quality: number; movement: number };
  cv: {
    name: string; scale: number; max_abs_bias: number;
    max_operating_violation_fraction: number; max_spec_violation_fraction: number;
    max_variance_increase_fraction: number; min_variance_improvement_fraction: number;
    quality_spec: { low: number | null; high: number | null } | null;
  }[];
  mv: { name: string; scale: number; max_movement_increase_fraction: number }[];
};
export type ResearchScenario = { id: string; gain_multiplier: number };
export type ResearchSettings = {
  methods: string[]; rounds: number; max_trials: number; objective: ResearchObjective;
  scenarios: ResearchScenario[]; [key: string]: ResearchValue;
};
export type Study = { id: string; name: string; dataset_id: string | null; model_id?: string; config: Campaign; research: ResearchSettings };
export type ResearchDraft = Study & { revision: number; draft: true; updated_at: string };
export type ResearchRun = {
  id: string; study_id: string; name: string; status: 'queued' | 'running' | 'cancel_requested' | 'cancelled' | 'failed' | 'completed';
  stage: string; progress: { completed: number; total: number }; created_at: string; error?: string;
  results?: ResearchResults; trials: Trial[]; checks: ResearchValue[];
  development?: { objectives: string[]; vectors: Record<string, number[]>; nondominated_trial_ids: string[]; scope: string };
  policy_candidate?: {
    status: 'sealed' | 'retain_baseline' | 'manual_only_unbound' | 'failed_qualification' | 'run_not_completed';
    candidate_id?: string; reason?: string; authority: 'review_only'; run_id: string; policy_id: string;
  };
};
export type Metrics = { score: number; cv_rmse: number[]; cv_iae: number[]; cv_violation_seconds: number[]; mv_total_variation: number[]; mv_hard_violations: number[]; solver_failures: number; [key: string]: ResearchValue };
export type Trial = { id: string; method: string; round: number; parent_trial?: string; parameters: ResearchValue; effective_tuning?: ResearchValue; score: number | null; valid: boolean; eligible?: boolean; reason?: string; samples?: number };
export type ResearchResults = {
  methods: {
    method: string; baseline: { full: Metrics; test: Metrics }; optimized: { full: Metrics; test: Metrics };
    transport: { full: Metrics; test: Metrics }; selected_trial: string; parameters: ResearchValue;
    recommendation?: 'candidate_for_review' | 'do_not_promote';
    selection_recommendation?: 'candidate' | 'retain_baseline';
    assessment?: { eligible: boolean; reasons: string[] };
  }[];
  variables: Campaign['variables']; splits: { name: string; start: string; end: string; first: number; last: number; samples: number }[];
  identification: ResearchValue; scope: string;
  model_test?: { passed: boolean; normalized_rmse: number[]; used_for_selection: boolean };
};
export type ResearchBootstrap = {
  schema_version: number; enabled: boolean;
  capabilities: { methods: { id: string; label: string; description: string }[]; [key: string]: ResearchValue };
  defaults: { campaign: Campaign; research: ResearchSettings };
  datasets: Dataset[]; studies: Study[]; runs: ResearchRun[];
};

export type ResearchWorkspaceContext = {
  datasetId: string; studyId: string; runId: string;
  sourceLabel: string; versionLabel: string; dirty: boolean;
};
