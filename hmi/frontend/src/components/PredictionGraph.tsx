import React, { useState } from 'react';
import { LineChart, Line, XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer } from 'recharts';
import { useTagStore } from '../store/tagStore';
import { forecastPoints, runtimeNodeId } from '../lib/modelBinding';
import type { RuntimeVariable } from '../lib/modelBinding';

interface PredictionGraphProps {
  cvList: RuntimeVariable[];
  mvList: RuntimeVariable[];
  sampleTime: number;
}

function VariableForecast({ variable, sampleTime }: { variable: RuntimeVariable; sampleTime: number }) {
  const nodeId = runtimeNodeId(variable, variable.role === 'CV' ? 'Prediction' : 'FuturePlan');
  const tag = useTagStore(state => nodeId ? state.tags[nodeId] : undefined);
  const good = tag && Number.isInteger(tag.status) && tag.status >= 0 && tag.status <= 0xffffffff && (tag.status >>> 30) === 0;
  const points = forecastPoints(good ? tag.value : undefined, sampleTime, variable.role === 'MV' ? 0 : 1);
  const unit = variable.units || 'Units unknown';
  return <section className="min-w-0 rounded border border-slate-700 p-3">
    <h4 className="text-sm text-slate-200">{variable.name} · {variable.role} · {unit}</h4>
    <p className="mb-2 text-xs text-slate-400">{variable.description} · {variable.role === 'CV' ? 'Prediction' : 'Future command plan'} · independent axis</p>
    {points.some(point => point.value !== null) ? <div className="h-56"><ResponsiveContainer width="100%" height="100%">
      <LineChart data={points} syncId="model-forecast">
        <CartesianGrid strokeDasharray="3 3" stroke="#334155" />
        <XAxis dataKey="seconds" type="number" domain={['dataMin', 'dataMax']} tick={{ fontSize: 11 }} label={{ value: 'Forecast offset (seconds)', position: 'insideBottom', offset: -3, fill: '#94a3b8' }} height={42} />
        <YAxis domain={['auto', 'auto']} tick={{ fontSize: 11 }} width={85} label={{ value: unit, angle: -90, position: 'insideLeft', fill: '#94a3b8' }} />
        <Tooltip contentStyle={{ backgroundColor: '#0f172a', borderColor: '#334155' }} />
        <Line name={`${variable.name} (${unit})`} type={variable.role === 'MV' ? 'stepAfter' : 'linear'} dataKey="value" stroke={variable.role === 'CV' ? '#818cf8' : '#fbbf24'} dot={false} connectNulls={false} isAnimationActive={false} />
      </LineChart>
    </ResponsiveContainer></div> : <p className="p-5 text-sm text-amber-200">Forecast unavailable: no valid vector or sampling metadata.</p>}
    <p className="text-xs text-slate-500">Latest received vector: {tag?.timestamp || 'unavailable'}. Offsets use the model sample time; this is not a historical forecast or proof of an accepted command.</p>
  </section>;
}

export const PredictionGraph: React.FC<PredictionGraphProps> = ({ cvList, mvList, sampleTime }) => {
  const variables = [...cvList, ...mvList];
  const identity = variables.map(variable => `${variable.role}:${variable.name}:${runtimeNodeId(variable, variable.role === 'CV' ? 'Prediction' : 'FuturePlan')}`).join('|');
  const [hidden, setHidden] = useState<Set<string>>(() => new Set());
  const [selectionIdentity, setSelectionIdentity] = useState(identity);
  if (selectionIdentity !== identity) {
    setSelectionIdentity(identity);
    setHidden(new Set());
  }
  return <div className="space-y-3 rounded-xl border border-slate-800 bg-slate-900/50 p-4">
    <h3 className="font-bold text-slate-200">Prediction & Control Horizon</h3>
    <p className="text-xs text-slate-400">Model sampling: {Number.isFinite(sampleTime) && sampleTime > 0 ? `${sampleTime} seconds` : 'unavailable'}. Each variable retains its own engineering scale.</p>
    <div className="flex flex-wrap gap-2">{variables.map(variable => <button key={variable.name} aria-pressed={!hidden.has(variable.name)} className="rounded border border-slate-700 px-2 py-1 text-xs" onClick={() => setHidden(previous => { const next = new Set(previous); if (next.has(variable.name)) next.delete(variable.name); else next.add(variable.name); return next; })}>{variable.role} · {variable.name}</button>)}</div>
    {variables.filter(variable => !hidden.has(variable.name)).map(variable => <VariableForecast key={variable.name} variable={variable} sampleTime={sampleTime} />)}
  </div>;
};
