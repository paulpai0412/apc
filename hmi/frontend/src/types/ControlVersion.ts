import type { EngineeringVariable, Metrics, ResearchResults, ResearchValue } from './Research';

export type ControlVersion = {
  schema_version: 1;
  id: string;
  created_at: string;
  selected_by: string;
  run_id: string;
  study_id: string;
  dataset_id: string | null;
  process_area: string;
  method: string;
  trial_id: string;
  model_sha256: string;
  authority: 'review_only';
  evidence_scope: string;
  parameters: Record<string, number>;
  variables: { cv: EngineeringVariable[]; mv: EngineeringVariable[]; dv: EngineeringVariable[] };
  metrics: { baseline: Metrics; selected: Metrics };
  assessment: ResearchValue;
  model_test: NonNullable<ResearchResults['model_test']>;
  source_hashes: Record<string, string>;
};
