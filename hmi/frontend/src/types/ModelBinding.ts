export interface NodeLimitBindings {
    low: string;
    high: string;
    ll?: string;
    hh?: string;
}

export interface CvNodeBindings {
    pv: string;
    target: string;
    prediction: string;
    limits: NodeLimitBindings;
}

export interface MvNodeBindings {
    pv: string;
    sp: string;
    op: string;
    mode: string;
    mode_target: string;
    future_plan: string;
    applied_sp?: string;
    target?: string;
    limits: NodeLimitBindings;
}

export interface DvNodeBindings {
    pv: string;
    limits: NodeLimitBindings;
}

export interface BoundVariable<Nodes> {
    name: string;
    description: string;
    units: string;
    node_ids: Nodes;
}

export type BoundCv = BoundVariable<CvNodeBindings>;
export type BoundMv = BoundVariable<MvNodeBindings>;
export type BoundDv = BoundVariable<DvNodeBindings>;

export interface ModelBinding {
    schema_version: 1;
    controller_name: string;
    sample_time: number;
    prediction_horizon: number;
    control_horizon: number;
    cvs: BoundCv[];
    mvs: BoundMv[];
    dvs: BoundDv[];
}
