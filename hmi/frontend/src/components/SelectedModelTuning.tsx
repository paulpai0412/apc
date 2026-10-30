import React, { useEffect, useState } from 'react';
import { apiFetch } from '../lib/api';
import { useTagStore } from '../store/tagStore';
import type { OpcUpdate } from '../store/tagStore';

export interface SelectedModelTuningProps {
    controllerId: string;
    modelFilename: string;
    liveTuning: boolean;
}

type TuningField = { label: 'Weight' | 'Alpha'; nodeId: string; value: number; schemaDefault: boolean };
type TuningVariable = { kind: 'CV' | 'MV'; name: string; fields: TuningField[] };
type LoadState =
    | { status: 'loading' }
    | { status: 'error'; message: string }
    | { status: 'loaded'; variables: TuningVariable[]; horizons: { prediction: number; control: number; sampleSeconds: number } };
type Comparison = 'matching' | 'mismatching' | 'unavailable';

function record(value: unknown, path: string): Record<string, unknown> {
    if (!value || typeof value !== 'object' || Array.isArray(value)) {
        throw new Error(`${path} 必須是物件。`);
    }
    return value as Record<string, unknown>;
}

function hasControlCharacter(value: string): boolean {
    for (let index = 0; index < value.length; index += 1) {
        const code = value.charCodeAt(index);
        if (code <= 0x1f || (code >= 0x7f && code <= 0x9f)) return true;
    }
    return false;
}

// Decode only the fields displayed here; this is not whole-model validation.
function decodeSelectedModelTuning(value: unknown): TuningVariable[] {
    const variables = record(record(value, '模型').variables, 'variables');
    const identities = new Set<string>();
    const result: TuningVariable[] = [];
    for (const [key, kind] of [['cvs', 'CV'], ['mvs', 'MV']] as const) {
        const entries = variables[key];
        if (!Array.isArray(entries) || entries.length === 0) {
            throw new Error(`variables.${key} 必須是非空陣列。`);
        }
        entries.forEach((entry: unknown, index: number) => {
            const path = `variables.${key}[${index}]`;
            const item = record(entry, path);
            const name = item.name;
            if (typeof name !== 'string' || !name.trim() || name !== name.trim() || hasControlCharacter(name)) {
                throw new Error(`${path}.name 必須是非空且無頭尾空白或控制字元的節點名稱。`);
            }
            const fields: TuningField[] = [];
            const specs = kind === 'CV'
                ? [['weight', 'Weight'], ['alpha', 'Alpha']] as const
                : [['weight_r', 'Weight']] as const;
            for (const [property, label] of specs) {
                const schemaDefault = property === 'alpha' && !Object.hasOwn(item, property);
                const numeric = schemaDefault ? 0 : item[property];
                if (typeof numeric !== 'number' || !Number.isFinite(numeric) || numeric < 0 || (label === 'Alpha' && numeric > 1)) {
                    throw new Error(`${path}.${property} 必須是${label === 'Alpha' ? ' 0 至 1 之間的' : '非負'}有限數值。`);
                }
                const nodeId = `${name}:${label}`;
                if (identities.has(nodeId)) {
                    throw new Error(`模型含重複的調參節點：${nodeId}。`);
                }
                identities.add(nodeId);
                fields.push({ label, nodeId, value: numeric, schemaDefault });
            }
            result.push({ kind, name, fields });
        });
    }
    return result;
}

function quality(tag: OpcUpdate | undefined): { good: boolean; label: string } {
    if (!tag) return { good: false, label: '未收到' };
    if (!Number.isInteger(tag.status) || tag.status < 0 || tag.status > 0xffffffff) {
        return { good: false, label: '品質碼無效或缺少' };
    }
    const severity = tag.status >>> 30;
    const label = severity === 0 ? 'Good' : severity === 1 ? 'Uncertain' : 'Bad';
    return { good: severity === 0, label: `${label} (0x${tag.status.toString(16).padStart(8, '0')})` };
}

function compare(field: TuningField, tag: OpcUpdate | undefined): Comparison {
    if (!tag || tag.node_id !== field.nodeId || !quality(tag).good || typeof tag.value !== 'number' || !Number.isFinite(tag.value)) {
        return 'unavailable';
    }
    // Match the engine's exact numeric equality, not rounded display values.
    return tag.value === field.value ? 'matching' : 'mismatching';
}

const comparisonLabels: Record<Comparison, string> = {
    matching: '相符',
    mismatching: '不相符',
    unavailable: '無法比對',
};

const SelectedModelTuningContent: React.FC<SelectedModelTuningProps> = ({ controllerId, modelFilename, liveTuning }) => {
    const [state, setState] = useState<LoadState>({ status: 'loading' });
    const tags = useTagStore(store => store.tags);

    useEffect(() => {
        const abort = new AbortController();
        const load = async () => {
            try {
                const response = await apiFetch(`/api/controller/${encodeURIComponent(controllerId)}/models/${encodeURIComponent(modelFilename)}`, {
                    signal: abort.signal,
                    cache: 'no-store',
                });
                if (!response.ok) throw new Error(`無法讀取所選模型（HTTP ${response.status}）。`);
                const model: unknown = await response.json();
                const variables = decodeSelectedModelTuning(model);
                const tuning = record(record(model, '模型').tuning, 'tuning');
                const prediction = tuning.prediction_horizon;
                const control = tuning.control_horizon;
                const sampleSeconds = tuning.sample_time;
                if (typeof prediction !== 'number' || !Number.isSafeInteger(prediction) || prediction < 1
                    || typeof control !== 'number' || !Number.isSafeInteger(control) || control < 1 || control > prediction
                    || typeof sampleSeconds !== 'number' || !Number.isFinite(sampleSeconds) || sampleSeconds <= 0) {
                    throw new Error('所選模型的 P／M／取樣時間無效。');
                }
                if (!abort.signal.aborted) setState({ status: 'loaded', variables, horizons: { prediction, control, sampleSeconds } });
            } catch (cause) {
                if (!abort.signal.aborted) {
                    setState({ status: 'error', message: cause instanceof SyntaxError
                        ? '所選模型不是有效的 JSON。'
                        : cause instanceof Error ? cause.message : '無法讀取所選模型。' });
                }
            }
        };
        void load();
        return () => abort.abort();
    }, [controllerId, modelFilename]);

    const counts = { matching: 0, mismatching: 0, unavailable: 0 };
    if (state.status === 'loaded') {
        for (const variable of state.variables) {
            for (const field of variable.fields) counts[compare(field, tags[field.nodeId])] += 1;
        }
    }

    return (
        <section aria-label="所選模型調參對照" className="min-w-0 space-y-2 rounded border border-slate-700 bg-slate-900/50 p-3 text-xs text-slate-300">
            <h4 className="font-semibold text-indigo-200">所選模型調參 · {liveTuning ? '手動線上調參：僅作基準' : '凍結調參：需與模型一致'}</h4>
            <p className="break-all text-slate-400">控制器：{controllerId} ／ 模型：{modelFilename}</p>
            {state.status === 'loading' && <p role="status">正在讀取所選模型…</p>}
            {state.status === 'error' && <p role="alert" className="break-words text-rose-300">{state.message} 無法建立對照；本面板不會代寫節點。</p>}
            {state.status === 'loaded' && (
                <>
                    <p className="text-slate-300">所選模型：P {state.horizons.prediction} ／ M {state.horizons.control} ／ 取樣 {state.horizons.sampleSeconds} 秒</p>
                    <p className="font-medium text-slate-200">
                        快取對照：相符 {counts.matching} ／ 不相符 {counts.mismatching} ／ 無法比對 {counts.unavailable}
                    </p>
                    <p className="text-slate-400">快取對照不是接管就緒判定；引擎會重新驗證實際值。</p>
                    <details className="rounded border border-slate-700/70 p-2">
                        <summary className="cursor-pointer text-indigo-200 focus-visible:outline focus-visible:outline-2 focus-visible:outline-indigo-400">逐項查看模型值、快取值與品質</summary>
                        <div className="mt-3 space-y-3">
                            {state.variables.map(variable => (
                                <div key={`${variable.kind}:${variable.name}`} className="min-w-0 overflow-x-auto">
                                    <table className="w-full table-fixed border-collapse text-left text-xs">
                                        <caption className="pb-2 text-left font-semibold text-slate-200 break-all">{variable.kind} · {variable.name}</caption>
                                        <thead className="text-slate-400">
                                            <tr>
                                                <th scope="col" className="w-1/3 p-1 align-top">節點（ns=2）</th>
                                                <th scope="col" className="w-1/4 p-1 align-top">{liveTuning ? '模型基準值' : '模型必需值'}</th>
                                                <th scope="col" className="p-1 align-top">最後收到的快取</th>
                                            </tr>
                                        </thead>
                                        <tbody>
                                            {variable.fields.map(field => {
                                                const tag = tags[field.nodeId];
                                                const comparison = compare(field, tag);
                                                const numeric = tag && typeof tag.value === 'number' && Number.isFinite(tag.value);
                                                return (
                                                    <tr key={field.nodeId} className="border-t border-slate-700/70">
                                                        <th scope="row" className="break-all p-1 align-top font-mono font-normal">{field.nodeId}</th>
                                                        <td className="break-all p-1 align-top font-mono text-indigo-200">{String(field.value)}{field.schemaDefault && <span className="block font-sans text-slate-400">模型省略；依引擎 Alpha 預設值</span>}</td>
                                                        <td className="space-y-1 break-all p-1 align-top">
                                                            <div className="font-mono">{numeric ? String(tag.value) : tag ? '非有限數值／非數值' : '—'}</div>
                                                            <div className={comparison === 'mismatching' ? 'text-amber-300' : 'text-slate-300'}>{comparisonLabels[comparison]}</div>
                                                            <div className="text-slate-400">品質：{quality(tag).label}</div>
                                                            {tag && tag.node_id !== field.nodeId && <div className="text-amber-300">快取節點身分不符</div>}
                                                            <div className="text-slate-400">快取 OPC 標籤時間戳：{tag?.timestamp || '—'}</div>
                                                        </td>
                                                    </tr>
                                                );
                                            })}
                                        </tbody>
                                    </table>
                                </div>
                            ))}
                        </div>
                    </details>
                </>
            )}
            <details><summary className="cursor-pointer text-indigo-200">調參一致性與設定方式</summary><p className="mt-2 leading-relaxed">
                {liveTuning
                    ? '已明確選擇手動線上調參；模型值僅作基準，相等不是此模式的門檻。'
                    : '凍結模式要求 CV Weight／Alpha 與 MV Weight 精確符合所選模型；請勿依四捨五入顯示值判定。'}
                引擎啟動不會套用調參，會重新讀取並驗證。
                {liveTuning ? 'Engage 前請人工確認所需調參。' : 'Engage 前請人工將所需值核對一致。'}
                可在既有 Tuning 頁的 CV Weight／Alpha、MV Weight 控制項設定；請輸入完整精度。若該節點未列出或無寫入權限，請由授權工程人員透過既有設定流程處理，不要繞過驗證。
                本面板唯讀，不會修改調參、目標、限制或模式。
            </p></details>
        </section>
    );
};

export const SelectedModelTuning: React.FC<SelectedModelTuningProps> = props => {
    if (!props.controllerId || !props.modelFilename) {
        return <p className="text-xs text-slate-400">請先選擇控制器與模型，再查看必需調參。</p>;
    }
    // A selection-keyed child prevents even one render of the previous model's values.
    return <SelectedModelTuningContent key={JSON.stringify([props.controllerId, props.modelFilename])} {...props} />;
};
