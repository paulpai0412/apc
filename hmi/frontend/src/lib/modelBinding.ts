import type { ModelBinding } from '../types/ModelBinding';

export type RuntimeRole = 'CV' | 'MV' | 'DV';
export interface RuntimeVariable {
  name: string;
  description: string;
  units: string;
  role: RuntimeRole;
  nodes: Readonly<Record<string, string>>;
}

const object = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === 'object' && !Array.isArray(value);
const identifier = (value: unknown): value is string => typeof value === 'string' && value.trim().length > 0;

/** Decode only explicit metadata. Physical identifiers are never normalized or inferred. */
export function decodeModelBinding(value: unknown): ModelBinding {
  const fail = (detail: string): never => { throw new Error(`Model binding unavailable: ${detail}`); };
  if (!object(value) || value.schema_version !== 1) return fail('expected model_binding schema_version 1');
  if (!identifier(value.controller_name)) return fail('controller_name is required');
  if (typeof value.sample_time !== 'number' || !Number.isFinite(value.sample_time) || value.sample_time <= 0) return fail('sample_time must be positive');
  for (const key of ['prediction_horizon', 'control_horizon']) {
    if (typeof value[key] !== 'number' || !Number.isSafeInteger(value[key]) || value[key] <= 0) return fail(`${key} must be a positive integer`);
  }
  if ((value.control_horizon as number) > (value.prediction_horizon as number)) return fail('control horizon exceeds prediction horizon');
  const names = new Set<string>(), ids = new Set<string>();
  const node = (id: unknown, label: string) => {
    if (!identifier(id)) return fail(`${label} is required`);
    if (ids.has(id)) return fail(`ambiguous node ID ${id}`);
    ids.add(id);
  };
  for (const group of ['cvs', 'mvs', 'dvs'] as const) {
    const variables = value[group];
    if (!Array.isArray(variables) || (group !== 'dvs' && variables.length === 0)) return fail(`${group} must be an explicit variable array`);
    for (const variable of variables) {
      if (!object(variable) || !identifier(variable.name) || typeof variable.description !== 'string' || typeof variable.units !== 'string' || !object(variable.node_ids)) return fail(`invalid ${group} variable metadata`);
      if (names.has(variable.name)) return fail(`ambiguous variable name ${variable.name}`);
      names.add(variable.name);
      const nodes = variable.node_ids;
      const required = group === 'cvs' ? ['pv', 'target', 'prediction'] : group === 'mvs' ? ['pv', 'sp', 'op', 'mode', 'mode_target', 'future_plan'] : ['pv'];
      for (const key of required) node(nodes[key], `${variable.name}.${key}`);
      if (group === 'mvs') for (const key of ['applied_sp', 'target']) if (nodes[key] !== undefined) node(nodes[key], `${variable.name}.${key}`);
      if (!object(nodes.limits)) return fail(`${variable.name}.limits is required`);
      for (const key of ['low', 'high']) node(nodes.limits[key], `${variable.name}.limits.${key}`);
      for (const key of ['ll', 'hh']) if (nodes.limits[key] !== undefined) node(nodes.limits[key], `${variable.name}.limits.${key}`);
    }
  }
  // Engine-owned diagnostics cannot also be configured as a measurement or writable target.
  for (const suffix of ['OperatingMode', 'Heartbeat', 'AcquisitionSequence', 'ControlObservation', 'ConfigurationStatus', 'NextRun', 'SolverStatus', 'ExecutionTimeMs', 'ObjectiveFunction', 'SaveConfiguration']) {
    if (ids.has(`${value.controller_name}:${suffix}`)) return fail(`configured node aliases engine ${suffix}`);
  }
  for (const group of ['cvs', 'mvs'] as const) {
    const variables = value[group] as { name: string }[];
    const suffixes = group === 'cvs' ? ['Weight', 'Alpha', 'Bias', 'SteadyState'] : ['Weight', 'LastMove', 'SteadyState'];
    for (const variable of variables) for (const suffix of suffixes) {
      if (ids.has(`${variable.name}:${suffix}`)) return fail(`configured node aliases engine ${variable.name}:${suffix}`);
    }
  }
  // All consumed fields have been validated above; preserve exact identifiers and metadata.
  const binding = value as unknown as ModelBinding;
  return binding;
}

export function runtimeVariables(binding: ModelBinding): RuntimeVariable[] {
  const groups = [['CV', binding.cvs], ['MV', binding.mvs], ['DV', binding.dvs]] as const;
  return groups.flatMap(([role, variables]) => variables.map(variable => {
    const source = variable.node_ids;
    const nodes: Record<string, string> = { PV: source.pv, LowLimit: source.limits.low, HighLimit: source.limits.high };
    if (source.limits.ll) nodes.LowLowLimit = source.limits.ll;
    if (source.limits.hh) nodes.HighHighLimit = source.limits.hh;
    if ('prediction' in source) { nodes.Target = source.target; nodes.Prediction = source.prediction; }
    if ('sp' in source) {
      Object.assign(nodes, { SP: source.sp, OP: source.op, Mode: source.mode, ModeTarget: source.mode_target, FuturePlan: source.future_plan });
      if (source.applied_sp) nodes.AppliedSP = source.applied_sp;
      if (source.target) nodes.Target = source.target;
    }
    return { name: variable.name, description: variable.description, units: variable.units, role, nodes };
  }));
}

/** Semantic UI fields are not suffixes. Only engine-owned diagnostics have conventional IDs. */
export function runtimeNodeId(variable: RuntimeVariable, field: string): string | undefined {
  if (Object.hasOwn(variable.nodes, field)) return variable.nodes[field];
  const diagnostics = variable.role === 'CV' ? ['Weight', 'Alpha', 'Bias', 'SteadyState'] : variable.role === 'MV' ? ['Weight', 'LastMove', 'SteadyState'] : [];
  if (diagnostics.includes(field)) return `${variable.name}:${field}`;
  return undefined;
}

export function forecastPoints(values: unknown, sampleTime: number | undefined, firstStep: 0 | 1 = 1): { seconds: number; value: number | null }[] {
  if (!Array.isArray(values) || typeof sampleTime !== 'number' || !Number.isFinite(sampleTime) || sampleTime <= 0) return [];
  return values.map((value, index) => ({ seconds: (index + firstStep) * sampleTime, value: typeof value === 'number' && Number.isFinite(value) ? value : null }));
}

/** Identification uses the model's manipulated quantity, never the unrelated PID output. */
export function identificationInput(variable: RuntimeVariable): { nodeId: string; feedback: 'applied-sp' | 'sp-register' | 'measurement' } | undefined {
  if (variable.role === 'DV') return { nodeId: variable.nodes.PV, feedback: 'measurement' };
  if (variable.role !== 'MV') return undefined;
  if (variable.nodes.AppliedSP) return { nodeId: variable.nodes.AppliedSP, feedback: 'applied-sp' };
  return { nodeId: variable.nodes.SP, feedback: 'sp-register' };
}
