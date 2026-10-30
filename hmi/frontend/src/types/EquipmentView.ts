import type { TrendQuality } from './ProcessTrend';
import type { Campaign } from './Research';
import type { VariableNodeIds } from './ModelConfig';

export type EquipmentLayout = 'closed_ball_mill' | 'vertical_mill';
export type EquipmentKind = 'silo' | 'feeder' | 'conveyor' | 'ball_mill' | 'vertical_mill' | 'elevator' | 'separator' | 'collector' | 'fan';
export type EquipmentRole = 'cv' | 'mv' | 'dv' | 'telemetry';
export type EquipmentChannel = 'actual' | 'target' | 'command' | 'op' | 'mode' | 'running' | 'fault';
export type EquipmentAnimation = 'none' | 'rotation' | 'flow';
export type EquipmentItem = {
  id: string; name: string; kind: EquipmentKind;
  position: [number, number, number]; size: [number, number, number]; rotation: [number, number, number];
};
export type EquipmentConnection = { id: string; from: string; to: string; kind: 'material' | 'air'; binding_id?: string };
export type EquipmentBinding = {
  id: string; equipment_id: string; role: EquipmentRole; variable_name: string; unit: string;
  channel: EquipmentChannel; node_id?: string; animation: EquipmentAnimation; reference_value?: number;
};
export type EquipmentView = {
  schema_version: 1; label: string; layout: EquipmentLayout; confirmed: boolean;
  equipment: EquipmentItem[]; connections: EquipmentConnection[]; bindings: EquipmentBinding[];
};
export type EquipmentViewVersion = {
  id: string; run_id: string; trial_id: string; controller_sha256: string; created_at: string; view: EquipmentView;
};
export type EquipmentPoint = {
  bindingId: string; equipmentId: string; role: EquipmentRole; name: string; unit: string;
  channel?: EquipmentChannel;
  value: number | null; quality: TrendQuality; time: number | null; sourceLabel: string; stale: boolean;
  animation: EquipmentAnimation; referenceValue: number | null;
};
export type EquipmentMotion = 'running' | 'paused' | 'stale' | 'unavailable';
export type TwinControllerVariable = {
  [key: string]: unknown;
  name: string; units: string; node_ids: VariableNodeIds;
  limits: { low: number; high: number; low_low?: number; high_high?: number; target?: number };
  weight?: number; alpha?: number; slack_weight?: number; is_integrating?: boolean;
  optimization_mode?: { type: 'Target' | 'Zone' | 'Maximize' | 'Minimize'; value?: number };
  weight_r?: number; max_move?: number; target?: number; target_weight?: number;
};
export type TwinControllerVariables = { cvs: TwinControllerVariable[]; mvs: TwinControllerVariable[]; dvs: TwinControllerVariable[] };
export type TwinController = {
  [key: string]: unknown;
  metadata: { name: string; model_type: string; [key: string]: unknown };
  tuning: { sample_time: number; prediction_horizon: number; control_horizon: number; [key: string]: unknown };
  variables: TwinControllerVariables;
};
export type TwinSettingsVersion = {
  id: string; run_id: string; trial_id: string; name: string; created_at: string;
  controller_sha256: string; controller: TwinController; variables: Campaign['variables']; diagnostic_only: true;
};
export type TwinConfiguration = {
  controller: TwinController; controller_sha256: string;
  equipment_views: EquipmentViewVersion[]; settings_versions: TwinSettingsVersion[];
};
