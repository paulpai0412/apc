import type { Campaign, ResearchValue, Trial } from './Research';
import type { EquipmentViewVersion, TwinControllerVariables } from './EquipmentView';

export type TwinTrial = Trial & { controller_sha256?: string; diagnostic_only?: boolean };
export type TwinVariableFocus = { role: 'CV' | 'MV' | 'DV'; name: string; unit: string };
export type TwinPath = { gain: number; tau1: number; tau2: number; delay_seconds: number };
export type TwinForecast = {
  schema_version: 1; sample: number; issued_at_seconds: number; sample_time: number;
  cv_count: number; mv_count: number; prediction_horizon: number; control_horizon: number;
  layout: 'step-major'; cv_first_offset_steps: 1; mv_first_offset_steps: 0;
  cv: number[]; mv_base: number[]; mv_delta: number[];
};
export type TwinFrame = {
  time_seconds: number; cv: number[]; mv: number[]; dv: number[]; targets: number[];
  command_mv: number[]; predicted_cv: number[]; quality: number[]; solver_status: string;
};
export type TwinTrace = {
  time_seconds: number[]; cv: number[][]; mv: number[][]; dv: number[][]; targets: number[][];
  command_mv: number[][]; predicted_cv: number[][]; quality: number[][]; solver_status: string[];
};
export type TwinOperation = {
  id: string; command: 'start' | 'pause' | 'step' | 'close';
  status: 'accepted' | 'completed' | 'unknown' | 'rejected'; reason?: string;
};
export type TwinSession = {
  id: string; run_id: string; trial_id: string; scenario: 'selection' | 'full'; transport: 'direct' | 'opc-da';
  status: 'starting' | 'running' | 'paused' | 'completed' | 'failed' | 'closed' | 'interrupted'; samples: number; requested_samples: number;
  sequence?: number; native_sequence?: number;
  execution_status?: 'starting' | 'paused' | 'running' | 'completed' | 'closed' | 'failed' | 'interrupted';
  operation?: TwinOperation | null;
  method?: string;
  sample_time: number; start: string; created_at: string; reason?: string | null; error?: string;
  time_basis: 'utc' | 'relative'; source_offset_seconds?: number;
  frame?: TwinFrame | null; recent_trace?: TwinTrace;
  latest_forecast?: TwinForecast | null;
  equipment_view?: EquipmentViewVersion | null; settings_version_id?: string | null;
  variables?: Campaign['variables']; controller_variables?: TwinControllerVariables;
  controller_sha256: string; plant_sha256: string; scenario_sha256: string; binary_sha256: string;
};
export type TwinManifest = {
  run_id: string; scope: string; run_status: string; stage: string;
  variables: Campaign['variables']; sample_time: number; start: string;
  plant_source: ResearchValue; plant_paths: TwinPath[][]; qualification: ResearchValue; identification: ResearchValue;
  time_basis: 'utc' | 'relative'; source_offset_seconds?: number;
  trials: TwinTrial[]; selected_trials: Record<string, string>; sessions: TwinSession[];
};

export type TwinArchiveFrame = {
  sample: number; total_samples: number; frame: TwinFrame; latest_forecast: TwinForecast | null;
  sample_time: number; start: string; time_basis: 'utc' | 'relative'; source_offset_seconds: number;
  controller_sha256: string; variables: Campaign['variables'];
};
