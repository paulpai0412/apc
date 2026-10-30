import type { ModelBinding } from './ModelBinding';
import type { EquipmentView } from './EquipmentView';
import type { UserRole } from '../lib/api';

export type RuntimeTarget = { plant: string; line: string; area: string; group: string };
export type RuntimeCapabilities = {
  profile_operations: boolean; native_forecast: boolean; automatic_handoff: boolean;
  engine_lifecycle: boolean; transport: string;
};
export type WorkspaceCapabilities = {
  runtime_enabled: boolean; research_enabled: boolean; capabilities: RuntimeCapabilities;
  deployment_kind: 'isolated_research' | 'local_test_runtime' | 'field_runtime';
  user: { id: number; username: string; role: UserRole };
  scopes: { profile_hash: string; target: RuntimeTarget; permissions: string[] }[];
  limitations: string[];
};
export type RuntimeVariable = {
  id: string; equipment_instance_id: string; role: 'CV' | 'MV' | 'DV'; unit: string;
  pv_tag: string; command_tag: string | null; applied_tag: string | null; op_tag: string | null;
  mode_tag: string | null; mode_target_tag: string | null; target_tag: string | null;
  low_tag: string | null; high_tag: string | null; hard_low: number; hard_high: number;
  max_move: number | null; command_coordinate: string | null; feedback_coordinate: string | null;
  remote_mode: number | null;
};
export type DeploymentProfile = {
  profile_hash: string; target: RuntimeTarget; equipment_instance_ids: string[];
  engine_id: string; model_sha256: string; endpoint: string; model_file: string;
  model_binding: ModelBinding; equipment_view: EquipmentView | null;
  forecast_tag: string; observation_tag: string; variables: RuntimeVariable[];
  approved_by: string; commissioning_evidence: string; operating_envelope: string;
  commissioning_kind: 'synthetic_local' | 'field'; application_tolerance: number;
  maximum_age_ms: number; maximum_skew_ms: number;
  acquisition_sequence_node: string; history_capacity_samples: number;
  handoff: { release_tag: string; release_value: number; acknowledgement_tag: string; acknowledgement_value: number; timeout_ms: number; watchdog_evidence: string };
};
export type RuntimeSignal = { node_id: string; value: unknown; timestamp: string; status: number; received_at: string };
export type RuntimeProfileSnapshot = {
  profile_hash: string; target: RuntimeTarget; equipment_instance_ids: string[]; epoch: number;
  version: string; health: 'unknown' | 'good' | 'stale'; blockers: string[];
  signals: RuntimeSignal[]; owner_group: string;
  instance_id: string | null; authority: string; arming: boolean; dcs_handoff: boolean | null;
  registry_epoch: number; active_model_binding: ModelBinding | null;
  native_status: Record<string, unknown> | null;
};
export type RuntimeOverview = { profiles: RuntimeProfileSnapshot[]; capabilities: RuntimeCapabilities; observed_at: string };
export type RuntimeOperationAction =
  | { kind: 'cv_target' | 'operating_low' | 'operating_high'; variable_id: string; value: number }
  | { kind: 'start' | 'recover_monitor' | 'monitor' | 'arm' | 'engage' | 'hold' | 'exit' | 'stop' }
  | { kind: 'switch_version' | 'start_candidate'; candidate_hash: string; approval_id: string };
export type RuntimeOperationRequest = {
  operation_id: string; profile_hash: string; expected_version: string; expected_epoch: number;
  expected_instance: string | null;
  expires_at: string; reason: string; action: RuntimeOperationAction;
};
export type RuntimeOperation = {
  operation_id: string; profile_hash: string; actor_id: number;
  state: 'executing' | 'accepted' | 'rejected' | 'unknown'; request: unknown;
  detail: string; created_at: string; updated_at: string;
  blocking: boolean; clearance_id: string | null; cleared_at: string | null; clearance_evidence: unknown;
};
export type RuntimeEvent = {
  id: number; profile_hash: string; source: string; severity: string; condition: string;
  first_seen: string; last_seen: string; read_at: string | null;
  condition_key: string | null; condition_active: boolean | null; cleared_at: string | null; last_evaluated: string | null;
};
export type RuntimeWorkspaceContext = { targetLabel: string; sourceLabel: string; versionLabel: string; health: string; observedAt: string | null };
