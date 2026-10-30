export interface LimitConfig {
    lowLow: number;
    low: number;
    high: number;
    highHigh: number;
    target?: number; // For CVs
}

export type CvOptimizationMode = "target" | "zone" | "maximize" | "minimize";
export type MvOptimizationMode = "target" | "maximize" | "minimize";

// Stored explicitly: editing a variable label must never rename plant nodes.
export interface VariableNodeIds {
    [role: string]: string | Record<string, string> | undefined;
    pv?: string;
    target?: string;
    prediction?: string;
    sp?: string;
    applied_sp?: string;
    op?: string;
    mode?: string;
    mode_target?: string;
    future_plan?: string;
    limits?: Record<string, string>;
}

export function defaultNodeIds(name: string, role: 'cv' | 'mv' | 'dv'): VariableNodeIds {
    const limits = { low: `${name}:LowLimit`, high: `${name}:HighLimit` };
    if (role === 'dv') return { pv: `${name}:PV`, limits };
    const safetyLimits = { ...limits, ll: `${name}:LowLowLimit`, hh: `${name}:HighHighLimit` };
    if (role === 'cv') return { pv: `${name}:PV`, target: `${name}:Target`, prediction: `${name}:Prediction`, limits: safetyLimits };
    return { pv: `${name}:PV`, sp: `${name}:SP`, op: `${name}:OP`, mode: `${name}:Mode`, mode_target: `${name}:ModeTarget`, future_plan: `${name}:FuturePlan`, limits: safetyLimits };
}

export interface VarConfig {
    id: string; // Internal ID for React keys
    name: string;
    desc: string;
    units: string;
    nodeIds: VariableNodeIds;
    source?: Record<string, unknown>;
    weight: number;
    alpha?: number; // For CVs
    ece_factor?: number; // For CVs - Equal Concern Error normalization (auto-calculated from span)
    optimizationMode?: CvOptimizationMode; // For CVs
    mvOptimizationMode?: MvOptimizationMode; // For MVs
    limits: LimitConfig;
    maxMove?: number; // Only for MVs
    target?: number; // Optional economic target for MVs
    targetWeight?: number; // Weight for MV target tracking (0 = no target)
}

export interface PhysicsMatrices {
    // CV x MV Models
    gain: number[][];      // K
    tau: number[][];       // Time Constants
    deadTime: number[][];  // Theta
    
    // CV x DV Models (Feedforward)
    gainDv: number[][];
    tauDv: number[][];
    deadTimeDv: number[][];
}

export interface TuningConfig {
    sampleTime: number;
    predictionHorizon?: number; // Exact imported P; derived from TSS only after explicit timing edits.
    tssMin: number; // Time to steady state
    controlHorizon: number; // Nu
    solverTol: number;
    maxIter: number;
    terminalWeightFactor: number;
}

export interface FullModel {
    source?: Record<string, unknown>; // Preserve model fields not exposed by this editor.
    meta: { name: string; desc: string };
    mode: "parametric" | "step_response";
    cvs: VarConfig[];
    mvs: VarConfig[];
    dvs: VarConfig[]; // New: Disturbance Variables
    physics: PhysicsMatrices;
    tuning: TuningConfig;
    stepResponseData?: {
        fileName: string;
        stepCoefficients?: number[][][]; // [cv][mv][time] - MV coefficients
        dvCoefficients?: number[][][]; // [cv][dv][time] - DV coefficients
        coefficients: number[][][]; // [cv][mv][time] - legacy field for backward compat
        parsedAt: string;
    };
}

// Default Empty Model
export const DEFAULT_MODEL: FullModel = {
    meta: { name: "New_Model", desc: "Created in AuTHRity" },
    mode: "parametric",
    cvs: [
        { id: "1", name: "TI1", nodeIds: defaultNodeIds("TI1", "cv"), desc: "Reactor Temp", units: "C", weight: 1.0, ece_factor: 80, optimizationMode: "target", limits: { lowLow: 0, low: 10, target: 50, high: 90, highHigh: 100 } }
    ],
    mvs: [
        { id: "1", name: "FC1", nodeIds: defaultNodeIds("FC1", "mv"), desc: "Steam Flow", units: "kg/h", weight: 1.0, maxMove: 5.0, mvOptimizationMode: "target", limits: { lowLow: 0, low: 0, high: 100, highHigh: 100 } }
    ],
    dvs: [], // Start empty
    physics: {
        // MV Models (1x1)
        gain: [[0]],
        tau: [[60]],
        deadTime: [[0]],
        
        // DV Models (Empty)
        gainDv: [],
        tauDv: [],
        deadTimeDv: []
    },
    tuning: {
        sampleTime: 20,
        tssMin: 20,
        controlHorizon: 10,
        solverTol: 0.0001,
        maxIter: 50,
        terminalWeightFactor: 10
    }
};