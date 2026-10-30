import type { FullModel, VarConfig, CvOptimizationMode, MvOptimizationMode, VariableNodeIds } from '../types/ModelConfig.ts';
import type { ModelBinding, CvNodeBindings, MvNodeBindings, DvNodeBindings, NodeLimitBindings } from '../types/ModelBinding.ts';

type ObjectValue = Record<string, unknown>;
const object = (value: unknown): ObjectValue => value !== null && typeof value === 'object' && !Array.isArray(value) ? value as ObjectValue : {};
const mode = (value: unknown): CvOptimizationMode => {
    const raw = typeof value === 'string' ? value : object(value).type;
    const name = typeof raw === 'string' ? raw.toLowerCase() : 'target';
    if (name !== 'target' && name !== 'zone' && name !== 'maximize' && name !== 'minimize') throw new Error(`Unsupported optimization mode: ${String(raw)}`);
    return name;
};
const number = (value: unknown, fallback: number): number => typeof value === 'number' ? value : fallback;

/** Both file upload and controller loading enter through this same lossless editor adapter. */
export function importControllerModel(input: unknown): FullModel {
    const raw = structuredClone(object(input));
    const metadata = object(raw.metadata), tuning = object(raw.tuning), physics = object(raw.physics), variables = object(raw.variables);
    if (typeof metadata.name !== 'string' || !metadata.name.trim()) throw new Error('Model metadata.name is required');
    if (!Array.isArray(variables.cvs) || !Array.isArray(variables.mvs)) throw new Error('Model CV/MV arrays are required');
    const sampleTime = number(raw.sample_time ?? tuning.sample_time, NaN);
    const predictionHorizon = number(tuning.prediction_horizon, NaN);
    if (!Number.isFinite(sampleTime) || sampleTime <= 0) throw new Error('Imported model sample_time must be positive');
    if (!Number.isSafeInteger(predictionHorizon) || predictionHorizon <= 0) throw new Error('Imported model prediction_horizon is required');
    if (typeof tuning.control_horizon !== 'number' || !Number.isSafeInteger(tuning.control_horizon) || tuning.control_horizon <= 0) throw new Error('Imported model control_horizon is required');
    const parseVars = (role: 'cv' | 'mv' | 'dv'): VarConfig[] => {
        const list = variables[`${role}s`] ?? [];
        if (!Array.isArray(list)) throw new Error(`${role} variables must be an array`);
        return list.map((item, index) => {
            const v = object(item), limits = object(v.limits);
            if (typeof v.name !== 'string' || !v.name.trim()) throw new Error(`${role} ${index + 1}: name is required`);
            const optimization = mode(v.optimization_mode);
            if (role === 'mv' && optimization === 'zone') throw new Error(`${v.name}: Zone is not an MV optimization mode`);
            return {
                id: `${role}_${index}`, name: v.name, desc: typeof v.description === 'string' ? v.description : '', units: typeof v.units === 'string' ? v.units : '',
                source: v, nodeIds: structuredClone(object(v.node_ids)) as VariableNodeIds,
                weight: number(v.weight ?? v.weight_r, 1), alpha: v.alpha as number | undefined, ece_factor: v.ece_factor as number | undefined,
                optimizationMode: role === 'cv' ? optimization : undefined, mvOptimizationMode: role === 'mv' ? optimization as MvOptimizationMode : undefined,
                maxMove: v.max_move as number | undefined, target: v.target as number | undefined, targetWeight: v.target_weight as number | undefined,
                limits: { lowLow: number(limits.low_low, 0), low: number(limits.low, 0), high: number(limits.high, 100), highHigh: number(limits.high_high, 100), target: limits.target as number | undefined },
            };
        });
    };
    const modelType = metadata.model_type;
    if (modelType !== 'parametric' && modelType !== 'step_response' && modelType !== 'step_response_import') throw new Error('Unsupported model_type');
    return {
        source: raw, meta: { name: metadata.name, desc: typeof metadata.description === 'string' ? metadata.description : '' },
        mode: modelType === 'parametric' ? 'parametric' : 'step_response', cvs: parseVars('cv'), mvs: parseVars('mv'), dvs: parseVars('dv'),
        tuning: { sampleTime, predictionHorizon, tssMin: predictionHorizon * sampleTime / 60, controlHorizon: number(tuning.control_horizon, 10), solverTol: number(tuning.solver_tolerance, 0.0001), maxIter: number(tuning.max_iterations, 50), terminalWeightFactor: number(tuning.terminal_weight_factor, 1) },
        physics: { gain: (physics.gain ?? []) as number[][], tau: (physics.tau ?? []) as number[][], deadTime: (physics.dead_time ?? []) as number[][], gainDv: (physics.gain_dv ?? []) as number[][], tauDv: (physics.tau_dv ?? []) as number[][], deadTimeDv: (physics.dead_time_dv ?? []) as number[][] },
        stepResponseData: physics.step_coefficients !== undefined || physics.dv_coefficients !== undefined ? {
            fileName: metadata.name, stepCoefficients: physics.step_coefficients as number[][][] | undefined, dvCoefficients: physics.dv_coefficients as number[][][] | undefined,
            coefficients: (physics.step_coefficients ?? []) as number[][][], parsedAt: new Date().toISOString(),
        } : undefined,
    };
}

export interface ArtifactNode {
    nodeId: string;
    nodeClass: 'Object' | 'Variable';
    dataType?: 'Double' | 'DoubleArray' | 'Int32' | 'String' | 'Boolean';
    initialValue?: number | number[] | string | boolean;
    description?: string;
}

export interface ModelArtifacts {
    json: string;
    yaml: string;
    binding: ModelBinding;
    nodes: ArtifactNode[];
}

export function buildModelArtifacts(model: FullModel): ModelArtifacts {
    const text = (value: unknown, path: string): string => {
        if (typeof value !== 'string' || !value.trim()) throw new Error(`${path}: node ID/name is required`);
        return value;
    };
    const controllerName = text(model.meta.name, 'Controller');
    const predictionHorizon = model.tuning.predictionHorizon ?? Math.ceil(model.tuning.tssMin * 60 / model.tuning.sampleTime);
    if (!Number.isFinite(model.tuning.sampleTime) || model.tuning.sampleTime <= 0) throw new Error('Sample time must be positive');
    for (const [label, value] of [['Prediction horizon', predictionHorizon], ['Control horizon', model.tuning.controlHorizon]] as const) {
        if (!Number.isSafeInteger(value) || value <= 0) throw new Error(`${label} must be a positive integer`);
    }
    if (model.tuning.controlHorizon > predictionHorizon) throw new Error('Control horizon must not exceed prediction horizon');
    if (!model.cvs.length || !model.mvs.length) throw new Error('At least one CV and MV are required');
    const nodes: ArtifactNode[] = [];
    const owners = new Map<string, string>();
    const add = (id: unknown, owner: string, dataType: ArtifactNode['dataType'] = 'Double', initialValue: ArtifactNode['initialValue'] = 0, description?: string) => {
        const nodeId = text(id, owner);
        if (owners.has(nodeId)) throw new Error(`Node ID collision: ${nodeId} (${owners.get(nodeId)} / ${owner})`);
        owners.set(nodeId, owner);
        if (typeof initialValue === 'number' && !Number.isFinite(initialValue)) throw new Error(`${owner}: initial value must be finite`);
        nodes.push({ nodeId, nodeClass: 'Variable', dataType, initialValue, ...(description ? { description } : {}) });
    };
    for (const suffix of ['OperatingMode', 'Heartbeat', 'NextRun', 'SolverStatus']) add(`${controllerName}:${suffix}`, `Controller ${suffix}`, 'Int32');
    for (const suffix of ['ExecutionTimeMs', 'ObjectiveFunction']) add(`${controllerName}:${suffix}`, `Controller ${suffix}`);
    add(`${controllerName}:AcquisitionSequence`, 'Controller AcquisitionSequence', 'Double', 0, 'Acquisition publisher owns this counter. Static zero is not readiness evidence.');
    for (const suffix of ['ConfigurationStatus', 'ControlObservation']) add(`${controllerName}:${suffix}`, `Controller ${suffix}`, 'String', '');
    add(`${controllerName}:SaveConfiguration`, 'Controller SaveConfiguration', 'Boolean', false);
    const variableNames = new Set<string>();
    const bind = (v: VarConfig, role: 'cv' | 'mv' | 'dv') => {
        const name = text(v.name, `${role} name`);
        if (variableNames.has(name)) throw new Error(`Variable name collision: ${name}`);
        variableNames.add(name);
        const ids = v.nodeIds ?? {};
        const limits: NodeLimitBindings = { low: text(ids.limits?.low, `${name}.limits.low`), high: text(ids.limits?.high, `${name}.limits.high`) };
        for (const key of ['ll', 'hh'] as const) if (ids.limits?.[key] !== undefined) limits[key] = text(ids.limits[key], `${name}.limits.${key}`);
        const required = role === 'cv' ? ['pv', 'target', 'prediction'] : role === 'mv' ? ['pv', 'sp', 'op', 'mode', 'mode_target', 'future_plan'] : ['pv'];
        const bound: Record<string, unknown> = { limits };
        for (const key of required) bound[key] = text(ids[key], `${name}.${key}`);
        if (role === 'mv') for (const key of ['applied_sp', 'target']) if (ids[key] !== undefined) bound[key] = text(ids[key], `${name}.${key}`);
        const values: Record<string, number | undefined> = { target: (role === 'cv' ? v.limits.target : v.target) ?? number(object(v.source?.optimization_mode).value, 0) };
        const limitValues: Record<string, number> = { low: v.limits.low, high: v.limits.high, ll: v.limits.lowLow, hh: v.limits.highHigh };
        // Retain even extra configured nodes from controller files; only role-specific bindings grant runtime meaning.
        for (const [key, id] of Object.entries(ids)) {
            if (id === undefined) continue;
            if (key === 'limits') {
                for (const [limit, limitId] of Object.entries(object(id))) {
                    if (!(limit in limitValues)) throw new Error(`${name}: unsupported limit mapping ${limit}`);
                    add(limitId, `${name}.limits.${limit}`, 'Double', limitValues[limit]);
                }
            } else {
                if (!['pv', 'target', 'prediction', 'sp', 'applied_sp', 'op', 'mode', 'mode_target', 'future_plan'].includes(key)) throw new Error(`${name}: unsupported node mapping ${key}; its OPC data type is unknown`);
                const arrayLength = key === 'prediction' ? predictionHorizon : key === 'future_plan' ? model.tuning.controlHorizon : undefined;
                add(id, `${name}.${key}`, arrayLength ? 'DoubleArray' : key === 'mode' || key === 'mode_target' ? 'Int32' : 'Double', arrayLength ? Array(arrayLength).fill(0) : values[key] ?? 0);
            }
        }
        const diagnostics = role === 'cv' ? { Weight: v.weight, Alpha: v.alpha ?? 0, Bias: 0, SteadyState: 0 } : role === 'mv' ? { Weight: v.weight, LastMove: 0, SteadyState: 0 } : { SteadyState: 0 };
        for (const [suffix, value] of Object.entries(diagnostics)) add(`${name}:${suffix}`, `${name} diagnostic ${suffix}`, 'Double', value);
        return { name, description: v.desc, units: v.units, node_ids: bound };
    };
    const binding: ModelBinding = {
        schema_version: 1, controller_name: controllerName, sample_time: model.tuning.sampleTime, prediction_horizon: predictionHorizon, control_horizon: model.tuning.controlHorizon,
        cvs: model.cvs.map(v => { const b = bind(v, 'cv'); return { ...b, node_ids: b.node_ids as unknown as CvNodeBindings }; }),
        mvs: model.mvs.map(v => { const b = bind(v, 'mv'); return { ...b, node_ids: b.node_ids as unknown as MvNodeBindings }; }),
        dvs: model.dvs.map(v => { const b = bind(v, 'dv'); return { ...b, node_ids: b.node_ids as unknown as DvNodeBindings }; }),
    };
    const exportVariable = (v: VarConfig, role: 'cv' | 'mv' | 'dv') => {
        const source = v.source ?? {};
        const sourceLimits = object(source.limits);
        const result: ObjectValue = { ...source, name: v.name, description: v.desc, units: v.units, node_ids: v.nodeIds };
        const limits: ObjectValue = { ...sourceLimits, low: v.limits.low, high: v.limits.high };
        for (const [key, value, fallback] of [['low_low', v.limits.lowLow, 0], ['high_high', v.limits.highHigh, 100], ['target', v.limits.target, undefined]] as const) {
            if (!v.source || key in sourceLimits || value !== fallback) limits[key] = value;
        }
        result.limits = limits;
        if (role === 'cv') Object.assign(result, { weight: v.weight, alpha: v.alpha, ece_factor: v.ece_factor });
        if (role === 'mv') Object.assign(result, { weight_r: v.weight, max_move: v.maxMove, target: v.target, target_weight: v.targetWeight });
        if (role !== 'dv') {
            const selected = role === 'cv' ? v.optimizationMode ?? 'target' : v.mvOptimizationMode ?? 'target';
            const target = role === 'cv' ? v.limits.target : v.target;
            const originalTarget = role === 'cv' ? sourceLimits.target : source.target;
            if (!v.source || selected !== mode(source.optimization_mode) || target !== originalTarget) {
                const type = selected[0].toUpperCase() + selected.slice(1);
                result.optimization_mode = selected === 'target' ? { type, value: target ?? 0 } : { type };
            }
        }
        return result;
    };
    const source = model.source ?? {};
    const sourcePhysics = object(source.physics);
    const physics: ObjectValue = { ...sourcePhysics };
    for (const [key, value] of Object.entries({ gain: model.physics.gain, tau: model.physics.tau, dead_time: model.physics.deadTime, gain_dv: model.physics.gainDv, tau_dv: model.physics.tauDv, dead_time_dv: model.physics.deadTimeDv })) {
        if (!model.source || key in sourcePhysics || value.length) physics[key] = value;
    }
    if (model.stepResponseData) {
        if (model.stepResponseData.stepCoefficients !== undefined) physics.step_coefficients = model.stepResponseData.stepCoefficients;
        if (model.stepResponseData.dvCoefficients !== undefined) physics.dv_coefficients = model.stepResponseData.dvCoefficients;
    }
    const sourceTuning = object(source.tuning);
    const tuning: ObjectValue = { ...sourceTuning, prediction_horizon: predictionHorizon, control_horizon: model.tuning.controlHorizon };
    if (source.sample_time === undefined || 'sample_time' in sourceTuning) tuning.sample_time = model.tuning.sampleTime;
    for (const [key, value, fallback] of [['solver_tolerance', model.tuning.solverTol, 0.0001], ['max_iterations', model.tuning.maxIter, 50], ['terminal_weight_factor', model.tuning.terminalWeightFactor, 1]] as const) {
        if (!model.source || key in sourceTuning || value !== fallback) tuning[key] = value;
    }
    const exported = {
        ...source,
        ...(source.sample_time !== undefined ? { sample_time: model.tuning.sampleTime } : {}),
        metadata: { ...object(source.metadata), name: controllerName, description: model.meta.desc, ...(!model.source ? { version: '1.2' } : {}), model_type: model.mode },
        tuning,
        variables: { ...object(source.variables), cvs: model.cvs.map(v => exportVariable(v, 'cv')), mvs: model.mvs.map(v => exportVariable(v, 'mv')), dvs: model.dvs.map(v => exportVariable(v, 'dv')) },
        physics,
    };
    // JSON is YAML flow syntax: one serializer safely escapes every identifier and description.
    return { json: JSON.stringify(exported, null, 2), yaml: JSON.stringify({ model_binding: binding, nodes }, null, 2), binding, nodes };
}
