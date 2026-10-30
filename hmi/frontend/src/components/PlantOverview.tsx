import React, { useEffect, useState } from 'react';
import { useTagStore } from '../store/tagStore';
import { ToggleSwitch } from './ToggleSwitch';
import { NextRunDisplay } from './NextRunDisplay';
import { writeOpcNode } from '../lib/opcWrite';
import { decodeModelBinding } from '../lib/modelBinding';

interface ModelInfo {
  name: string;
  prefix: string;
  error?: string;
}

interface PlantOverviewProps {
  wsRef: React.RefObject<WebSocket | null>;
  onSelectModel: (name: string) => void;
  canOperate: boolean;
}

export const PlantOverview: React.FC<PlantOverviewProps> = ({ wsRef, onSelectModel, canOperate }) => {
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [loading, setLoading] = useState(true);

  // 1. Fetch Models and Determine Prefixes
  useEffect(() => {
    const fetchAllModels = async () => {
      try {
        const res = await fetch('/api/models');
        if (!res.ok) throw new Error(`Model list unavailable (${res.status})`);
        const modelNames: string[] = await res.json();
        
        const infoPromises = modelNames.map(async (name) => {
           try {
             const configRes = await fetch(`/api/model?file=${encodeURIComponent(name)}`);
             if (!configRes.ok) throw new Error(`Configuration unavailable (${configRes.status})`);
             const configData = await configRes.json();
             const binding = decodeModelBinding(configData.model_binding);
             return { name, prefix: binding.controller_name };
           } catch (e) {
             console.error(`Failed to load config for ${name}`, e);
             return { name, prefix: "", error: e instanceof Error ? e.message : 'Model binding unavailable' };
           }
        });

        const loadedModels = await Promise.all(infoPromises);
        setModels(loadedModels);
        setLoading(false);
      } catch (err) {
        console.error("Failed to load plant overview", err);
        setLoading(false);
      }
    };

    fetchAllModels();
  }, []);

  const handleWrite = (nodeId: string, value: number) => {
    if (!canOperate || !models.some(model => model.prefix && nodeId === `${model.prefix}:OperatingMode`)) return;
    if (!wsRef.current || wsRef.current.readyState !== WebSocket.OPEN) return;
    void writeOpcNode(nodeId, value).catch(error => console.error('OPC write not confirmed', error));
  };

  const handleToggle = (prefix: string, currentMode: number, isControlToggle: boolean, isCalcToggle: boolean) => {
      // Determine new mode based on toggle clicked
      // Mode 0: Idle
      // Mode 1: Minitor (Calc On, Control Off)
      // Mode 2: Engage (Calc On, Control On)
      
      let newMode = currentMode;
      const calcOn = currentMode >= 1;
      const controlOn = currentMode === 2;

      if (isCalcToggle) {
          // User clicked "Calculator"
          // If turning ON: Go to Monitor (1)
          // If turning OFF: Go to Idle (0)
          if (!calcOn) newMode = 1; // 0 -> 1
          else newMode = 0;         // 1/2 -> 0
      } else if (isControlToggle) {
           // User clicked "Control Action"
           // If turning ON: Go to Engage (2) (Must have calc on implicit)
           // If turning OFF: Go to Monitor (1)
           if (!controlOn) newMode = 2; // 1 -> 2
           else newMode = 1;            // 2 -> 1
      }

      handleWrite(`${prefix}:OperatingMode`, newMode);
  };

  if (loading) return <div className="p-8 text-slate-500">Scanning Plant Network...</div>;

  return (
    <div className="p-8 animate-in fade-in duration-500">
      <h2 className="text-2xl font-bold text-slate-100 mb-6">Plant Overview</h2>
      
      <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-6">
        {models.map((model) => (
          <ModelCard 
            key={model.name} 
            model={model} 
            onToggle={handleToggle}
            onSelect={() => onSelectModel(model.name)}
            canOperate={canOperate}
          />
        ))}
      </div>
    </div>
  );
};

// --- SUB-COMPONENT FOR INDIVIDUAL CARD ---
const ModelCard = ({ model, onToggle, onSelect, canOperate }: {
    model: ModelInfo, 
    onToggle: (p: string, m: number, c: boolean, k: boolean) => void,
    onSelect: () => void,
    canOperate: boolean
}) => {
    const modeTag = useTagStore(state => state.tags[`${model.prefix}:OperatingMode`]);
    const configurationTag = useTagStore(state => state.tags[`${model.prefix}:ConfigurationStatus`]);
    const validMode = model.prefix && modeTag && Number.isInteger(modeTag.status) && modeTag.status >= 0 && modeTag.status <= 0xffffffff
        && (modeTag.status >>> 30) === 0 && typeof modeTag.value === 'number' && [0, 1, 2].includes(modeTag.value);
    const mode = validMode ? modeTag.value as number : null;
    const calcOn = mode === 1 || mode === 2;
    const controlOn = mode === 2;
    const configurationGood = configurationTag && Number.isInteger(configurationTag.status)
        && configurationTag.status >= 0 && configurationTag.status <= 0xffffffff && (configurationTag.status >>> 30) === 0;
    const configurationStatus = configurationGood && typeof configurationTag.value === 'string'
        ? configurationTag.value : '未收到可信的引擎狀態；不可據此判定已接管。';

    return (
        <div className="bg-slate-900 border border-slate-800 rounded-xl p-5 shadow-lg hover:border-indigo-500/30 transition-colors">
            <div className="flex justify-between items-start mb-4">
                <div>
                    <h3 className="text-lg font-bold text-white cursor-pointer hover:text-indigo-400" onClick={onSelect}>
                        {model.name}
                    </h3>
                    <div className="text-xs text-slate-500 font-mono mt-1">{model.prefix || 'Binding unavailable'}</div>
                </div>
                {/* Next Run Display */}
                <div className="bg-slate-950 rounded-lg p-2 border border-slate-800">
                     <NextRunDisplay systemPrefix={model.prefix} />
                </div>
            </div>

            <div className="h-px bg-slate-800 my-4"></div>
            {model.error && <p role="alert" className="mb-3 text-xs text-amber-200">{model.error}. Controls unavailable.</p>}

            <div className="flex items-center justify-between gap-4">
                {/* Calculator Toggle */}
                <div className="flex flex-col items-center">
                    <ToggleSwitch 
                        label="要求計算"
                        enabled={calcOn}
                        disabled={!canOperate || mode === null}
                        onToggle={() => { if (mode !== null) onToggle(model.prefix, mode, false, true); }}
                        colorOn="bg-blue-500"
                    />
                    <span className={`text-[10px] mt-2 font-bold uppercase tracking-wider ${calcOn ? 'text-blue-400' : 'text-slate-600'}`}>
                        {mode === null ? "Unavailable" : calcOn ? "Requested" : "Idle"}
                    </span>
                </div>

                {/* Status Indicator */}
                <div className="flex-1 flex flex-col items-center justify-center">
                    <div className={`w-3 h-3 rounded-full mb-1 ${
                        mode === 2 ? "bg-emerald-500 shadow-[0_0_10px_rgba(16,185,129,0.5)]" : 
                        mode === 1 ? "bg-blue-500 shadow-[0_0_10px_rgba(59,130,246,0.5)]" : 
                        "bg-slate-700"
                    }`}></div>
                    <span className="text-[9px] text-slate-500 font-mono">
                         {mode === 2 ? "REQUESTED ENGAGE" : mode === 1 ? "MONITOR" : mode === 0 ? "IDLE" : "UNKNOWN"}
                    </span>
                </div>

                {/* Control Toggle */}
                <div className="flex flex-col items-center">
                    <ToggleSwitch 
                        label="要求接管"
                        enabled={controlOn}
                        disabled={!canOperate || !calcOn || mode === null}
                        onToggle={() => { if (mode !== null) onToggle(model.prefix, mode, true, false); }}
                        colorOn="bg-emerald-500"
                    />
                     <span className={`text-[10px] mt-2 font-bold uppercase tracking-wider ${controlOn ? 'text-emerald-400' : 'text-slate-600'}`}>
                        {mode === null ? "Unavailable" : controlOn ? "Requested" : "Not requested"}
                    </span>
                </div>
            </div>
            <div className="mt-4 border-t border-slate-800 pt-3 text-xs"><p className="break-words text-slate-300">{configurationStatus}</p><p className="mt-1 text-slate-500">模式是操作要求，不是 MV 已被接受的證據。最後引擎狀態：{configurationTag?.timestamp || '未取得'}</p></div>
        </div>
    );
};
