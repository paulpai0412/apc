import { useEffect, useMemo, useRef, useState } from 'react';
import { LineChart, Line, XAxis, YAxis, CartesianGrid, Tooltip, Legend, ResponsiveContainer, ReferenceLine, ReferenceArea } from 'recharts';
import { Download } from 'lucide-react';
import { apiFetch } from '../lib/api';
import { runtimeVariables, identificationInput } from '../lib/modelBinding';
import { opcQuality } from '../lib/historianTrends';
import type { HistoryEnvelope } from '../lib/historianTrends';
import type { ModelBinding } from '../types/ModelBinding';

interface StepResponseToolProps { modelBinding: ModelBinding | null; tssMin: number }
interface HistorySample { time: number; value: number | null }
interface CvResponse { cv_node: string; cv_baseline: number; coefficients: number[]; raw_response: [number, number][]; fitted_response: [number, number][] }
interface StepResponse { mv_baseline: number; step_size: number; step_time: string; responses: CvResponse[] }
const finite = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value);
const object = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === 'object' && !Array.isArray(value);
function decodeResponse(value: unknown): StepResponse {
  const pairs = (values: unknown) => Array.isArray(values) && values.every(pair => Array.isArray(pair) && pair.length === 2 && pair.every(finite));
  if (!object(value) || !finite(value.mv_baseline) || !finite(value.step_size) || typeof value.step_time !== 'string' || !Array.isArray(value.responses)
    || !value.responses.every(row => object(row) && typeof row.cv_node === 'string' && finite(row.cv_baseline) && Array.isArray(row.coefficients) && row.coefficients.every(finite) && pairs(row.raw_response) && pairs(row.fitted_response))) {
    throw new Error('Step-response result unavailable: invalid calculation response.');
  }
  // Every consumed response member was checked above.
  const result = value as unknown as StepResponse;
  return result;
}
const inputClass = 'w-full rounded border border-slate-700 bg-slate-900 px-3 py-2 text-sm';

export default function StepResponseTool({ modelBinding, tssMin }: StepResponseToolProps) {
  const variables = useMemo(() => modelBinding ? runtimeVariables(modelBinding) : [], [modelBinding]);
  const cvs = variables.filter(variable => variable.role === 'CV');
  const inputs = variables.filter(variable => variable.role !== 'CV');
  const [startTime, setStartTime] = useState('');
  const [endTime, setEndTime] = useState('');
  const [selectedInput, setSelectedInput] = useState('');
  const [selectedCVs, setSelectedCVs] = useState<string[]>([]);
  const [history, setHistory] = useState<Record<string, HistorySample[]>>({});
  const [stepTime, setStepTime] = useState('');
  const [baselineDuration, setBaselineDuration] = useState(300);
  const [resultState, setResult] = useState<{ context: string; data: StepResponse } | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const input = inputs.find(variable => variable.name === selectedInput);
  const inputSource = input ? identificationInput(input) : undefined;
  const outputs = cvs.filter(variable => selectedCVs.includes(variable.name));
  const sampleTime = modelBinding?.sample_time;
  const responseDuration = tssMin * 60;
  const timingValid = finite(sampleTime) && sampleTime > 0 && finite(responseDuration) && responseDuration > 0;
  const context = JSON.stringify([modelBinding, selectedInput, selectedCVs, startTime, endTime]);
  const calculationContext = JSON.stringify([context, stepTime, baselineDuration, tssMin]);
  const activeCalculation = useRef(calculationContext);
  activeCalculation.current = calculationContext;
  const result = resultState?.context === calculationContext ? resultState.data : null;
  const activeContext = useRef(context);
  activeContext.current = context;
  useEffect(() => { setHistory({}); setResult(null); setStepTime(''); setError(''); setLoading(false); }, [context]);
  const startMs = Date.parse(startTime), endMs = Date.parse(endTime), stepMs = Date.parse(stepTime);
  const baselineTruncated = Number.isFinite(stepMs) && Number.isFinite(startMs) && stepMs - baselineDuration * 1000 < startMs;
  const traces = [...(input && inputSource ? [{ variable: input, nodeId: inputSource.nodeId }] : []), ...outputs.map(variable => ({ variable, nodeId: variable.nodes.PV }))];

  async function loadHistoricalData() {
    if (!modelBinding || !timingValid || !inputSource || outputs.length === 0) { setError('A valid explicit binding, input, outputs and sample time are required.'); return; }
    if (!Number.isFinite(startMs) || !Number.isFinite(endMs) || endMs <= startMs) { setError('Select a valid increasing time range.'); return; }
    const requestedContext = context;
    setLoading(true); setError(''); setResult(null);
    try {
      const params = new URLSearchParams({ nodes: JSON.stringify(traces.map(trace => trace.nodeId)), settings: JSON.stringify(input?.role === 'MV' ? [inputSource.nodeId] : []), start: new Date(startMs).toISOString(), end: new Date(endMs).toISOString() });
      const response = await apiFetch(`/api/trends?${params}`);
      if (!response.ok) throw new Error(await response.text());
      const envelope: HistoryEnvelope = await response.json();
      if (!Array.isArray(envelope.points) || envelope.points.some(point => typeof point.node_id !== 'string')) throw new Error('Historian response lacks authoritative node_id values.');
      if (requestedContext !== activeContext.current) return;
      const grouped: Record<string, HistorySample[]> = Object.fromEntries(traces.map(trace => [trace.nodeId, []]));
      for (const point of envelope.points) {
        const id = point.node_id;
        if (!Object.hasOwn(grouped, id)) continue;
        const time = Date.parse(point.timestamp);
        if (!Number.isFinite(time)) continue;
        grouped[id].push({ time, value: finite(point.value) && opcQuality(point.status) === 'good' ? point.value : null });
      }
      for (const points of Object.values(grouped)) points.sort((a, b) => a.time - b.time);
      setHistory(grouped);
      const source = grouped[inputSource.nodeId];
      let biggest = 0, detected: number | undefined;
      for (let index = 1; index < source.length; index++) {
        const before = source[index - 1].value, after = source[index].value;
        if (before === null || after === null) continue;
        const change = Math.abs(after - before);
        if (change > biggest) { biggest = change; detected = source[index].time; }
      }
      if (detected !== undefined) setStepTime(new Date(detected).toISOString());
    } catch (cause) { if (requestedContext === activeContext.current) setError(cause instanceof Error ? cause.message : 'History request failed.'); }
    finally { if (requestedContext === activeContext.current) setLoading(false); }
  }

  async function calculateStepResponse() {
    if (!modelBinding || !timingValid || !inputSource || !Number.isFinite(stepMs) || outputs.length === 0) { setError('Valid binding, sample time and step time are required.'); return; }
    if (!finite(baselineDuration) || baselineDuration <= 0 || baselineTruncated) { setError('The complete positive baseline window must be included.'); return; }
    const samples = traces.map(trace => history[trace.nodeId]);
    if (samples.some(points => !points || points.length < 2 || points.some(point => point.value === null))) { setError('Calculation requires recorded good-quality samples for every bound signal; missing data will not be interpolated by the UI.'); return; }
    const requestedContext = context;
    const requestedCalculation = calculationContext;
    setLoading(true); setError(''); setResult(null);
    const pairs = (points: HistorySample[]) => points.map(point => [new Date(point.time).toISOString(), point.value]);
    try {
      const response = await apiFetch('/api/stepresponse/calculate', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({
        mv_node: inputSource.nodeId, cv_nodes: outputs.map(variable => variable.nodes.PV), mv_data: pairs(samples[0]), cv_data: samples.slice(1).map(pairs),
        step_time: new Date(stepMs).toISOString(), baseline_duration: baselineDuration, response_duration: responseDuration, sample_time: sampleTime,
      }) }, true);
      if (!response.ok) throw new Error(await response.text());
      const calculated = decodeResponse(await response.json());
      if (calculated.responses.length !== outputs.length || new Set(calculated.responses.map(row => row.cv_node)).size !== outputs.length
        || calculated.responses.some(row => !outputs.some(variable => variable.nodes.PV === row.cv_node))) throw new Error('Calculation returned different bound CVs.');
      if (requestedContext === activeContext.current && requestedCalculation === activeCalculation.current) setResult({ context: requestedCalculation, data: calculated });
    } catch (cause) { if (requestedContext === activeContext.current) setError(cause instanceof Error ? cause.message : 'Calculation failed.'); }
    finally { if (requestedContext === activeContext.current) setLoading(false); }
  }

  function exportToJSON() {
    if (!result || !input || !inputSource || !modelBinding) return;
    const data = { timestamp: new Date().toISOString(), input_tag: input.name, input_node: inputSource.nodeId, input_type: input.role,
      sample_time: sampleTime, step_size: result.step_size, step_time: result.step_time,
      responses: result.responses.map(response => ({ cv_tag: cvs.find(variable => variable.nodes.PV === response.cv_node)!.name, cv_node: response.cv_node, baseline: response.cv_baseline, coefficients: response.coefficients })) };
    const url = URL.createObjectURL(new Blob([JSON.stringify(data, null, 2)], { type: 'application/json' }));
    const anchor = document.createElement('a'); anchor.href = url; anchor.download = `model_${input.role}_${input.name}.json`; anchor.click(); URL.revokeObjectURL(url);
  }

  return <div className="space-y-4 p-6">
    <h2 className="text-xl font-semibold">Step Response Tool</h2>
    {!modelBinding && <p role="alert" className="text-amber-200">Model binding unavailable. Complete explicit node mappings and timing before identification.</p>}
    {error && <p role="alert" className="break-words text-rose-300">{error}</p>}
    <section className="space-y-3 rounded border border-slate-700 p-4">
      <div className="grid gap-3 md:grid-cols-2">
        <label className="text-xs">Start<input className={inputClass} type="datetime-local" value={startTime} onChange={event => setStartTime(event.target.value)} /></label>
        <label className="text-xs">End<input className={inputClass} type="datetime-local" value={endTime} onChange={event => setEndTime(event.target.value)} /></label>
        <label className="text-xs">Input (MV / DV)<select className={inputClass} value={selectedInput} disabled={!modelBinding} onChange={event => setSelectedInput(event.target.value)}><option value="">Select…</option>{inputs.map(variable => <option key={variable.name} value={variable.name}>{variable.role} · {variable.name} ({variable.units || 'units unknown'})</option>)}</select></label>
        <fieldset><legend className="text-xs">Outputs (CVs)</legend><div className="flex flex-wrap gap-3">{cvs.map(variable => <label key={variable.name} className="text-xs"><input type="checkbox" checked={selectedCVs.includes(variable.name)} onChange={() => setSelectedCVs(previous => previous.includes(variable.name) ? previous.filter(name => name !== variable.name) : [...previous, variable.name])} /> {variable.name} ({variable.units || 'units unknown'})</label>)}</div></fieldset>
      </div>
      {inputSource && <p className="break-all text-xs text-slate-400">Input node: {inputSource.nodeId}. {inputSource.feedback === 'sp-register' ? 'SP-register feedback only: not evidence that the PID applied the command.' : inputSource.feedback === 'applied-sp' ? 'Explicit applied-SP feedback.' : 'Explicit disturbance measurement.'} PID OP is a separate quantity and is not substituted for this model input.</p>}
      <p className="text-xs text-slate-400">Sample time: {timingValid ? `${sampleTime} s` : 'unavailable'} · Response window: {finite(tssMin) ? `${tssMin} min` : 'unavailable'}</p>
      <button onClick={() => void loadHistoricalData()} disabled={loading || !modelBinding || !timingValid} className="rounded bg-indigo-600 px-4 py-2 text-sm disabled:opacity-40">{loading ? 'Working…' : 'Load Data'}</button>
    </section>
    {Object.keys(history).length > 0 && <section className="space-y-4 rounded border border-slate-700 p-4">
      <p className="text-xs text-slate-400">Click any independent chart to set the step time. Missing or non-good samples remain gaps.</p>
      {traces.map(({ variable, nodeId }) => <div key={nodeId}><h3 className="text-sm">{variable.name} · {variable.units || 'units unknown'}</h3>{history[nodeId]?.length ? <div className="h-56"><ResponsiveContainer width="100%" height="100%"><LineChart data={history[nodeId]} syncId="identification-history" onClick={event => { const time = Number(event?.activeLabel); if (Number.isFinite(time)) { setStepTime(new Date(time).toISOString()); setResult(null); } }}>
        <CartesianGrid stroke="#334155" strokeDasharray="3 3" /><XAxis dataKey="time" type="number" domain={['dataMin', 'dataMax']} tickFormatter={time => new Date(time).toLocaleTimeString()} /><YAxis domain={['auto', 'auto']} width={85} label={{ value: variable.units || 'unknown', angle: -90, position: 'insideLeft' }} /><Tooltip labelFormatter={time => new Date(Number(time)).toLocaleString()} />
        {Number.isFinite(stepMs) && <><ReferenceArea x1={stepMs - baselineDuration * 1000} x2={stepMs} fill="#64748b" fillOpacity={0.15} /><ReferenceArea x1={stepMs} x2={stepMs + responseDuration * 1000} fill="#3b82f6" fillOpacity={0.1} /><ReferenceLine x={stepMs} stroke="#fbbf24" /></>}
        <Line dataKey="value" name={variable.name} type={variable.role === 'MV' ? 'stepAfter' : 'linear'} stroke={variable.role === 'CV' ? '#818cf8' : '#fbbf24'} dot={false} connectNulls={false} isAnimationActive={false} />
      </LineChart></ResponsiveContainer></div> : <p className="text-amber-200">No recorded data for {nodeId}.</p>}</div>)}
      {baselineTruncated && <p role="alert" className="text-amber-200">The requested baseline begins before the loaded history window.</p>}
      <div className="grid gap-3 md:grid-cols-3"><label className="text-xs">Step time<input className={inputClass} type="datetime-local" step="1" value={Number.isFinite(stepMs) ? new Date(stepMs - new Date(stepMs).getTimezoneOffset() * 60000).toISOString().slice(0, 19) : ''} onChange={event => { setStepTime(event.target.value); setResult(null); }} /></label><label className="text-xs">Baseline (seconds)<input className={inputClass} type="number" min="1" value={baselineDuration} onChange={event => { setBaselineDuration(Number(event.target.value)); setResult(null); }} /></label><button disabled={loading || !timingValid || !modelBinding} onClick={() => void calculateStepResponse()} className="rounded bg-emerald-700 px-4 py-2 text-sm disabled:opacity-40">Calculate</button></div>
    </section>}
    {result && <section className="space-y-4 rounded border border-slate-700 p-4">
      <h3 className="font-semibold">Model fit · each CV retains an independent gain axis</h3>
      <p className="text-sm">Input baseline: {result.mv_baseline} {input?.units || '(units unknown)'} · Step size: {result.step_size} {input?.units || '(units unknown)'}</p>
      {result.responses.map(response => {
        const variable = cvs.find(cv => cv.nodes.PV === response.cv_node)!;
        const gainUnit = variable.units && input?.units ? `${variable.units} / ${input.units}` : 'gain units unknown';
        return <div key={response.cv_node}><h4 className="text-sm">{variable.name} · {gainUnit} · Baseline {response.cv_baseline} {variable.units || '(units unknown)'}</h4><div className="h-64"><ResponsiveContainer width="100%" height="100%"><LineChart syncId="identification-fit">
          <CartesianGrid stroke="#334155" strokeDasharray="3 3" /><XAxis dataKey="time" type="number" domain={[0, responseDuration]} label={{ value: 'Offset (seconds)', position: 'insideBottom', offset: -4 }} /><YAxis width={90} domain={['auto', 'auto']} /><Tooltip /><Legend />
          <Line data={response.raw_response.map(([time, value]) => ({ time, value }))} dataKey="value" name="Recorded response" type="linear" stroke="#94a3b8" dot={false} connectNulls={false} isAnimationActive={false} />
          <Line data={response.fitted_response.map(([time, value]) => ({ time, value }))} dataKey="value" name="Fitted response" type="linear" stroke="#818cf8" dot={false} connectNulls={false} isAnimationActive={false} />
        </LineChart></ResponsiveContainer></div><p className="text-xs text-slate-400">{response.coefficients.length} coefficients · {response.cv_node}</p></div>;
      })}
      <button onClick={exportToJSON} className="flex items-center gap-2 rounded bg-violet-700 px-4 py-2"><Download className="h-4 w-4" />Export Model JSON</button>
    </section>}
  </div>;
}
