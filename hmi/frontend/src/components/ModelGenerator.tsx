import React, { useState, useEffect } from 'react';
import { DEFAULT_MODEL, defaultNodeIds, type CvOptimizationMode, type FullModel, type MvOptimizationMode, type VarConfig } from '../types/ModelConfig';
import { Save, Upload, Download, Plus, Trash2, Activity, FileText, CheckCircle, ArrowDown, ArrowUp, Server, Network, Rocket, RefreshCw, AlertCircle } from 'lucide-react';
import { LineChart, Line, XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer } from 'recharts';
import { MatrixEditor } from './MatrixEditor';
import StepResponseTool from './StepResponseTool';
import { apiFetch } from '../lib/api';
import { buildModelArtifacts, importControllerModel, type ModelArtifacts } from '../lib/modelArtifacts';

// --- INFRASTRUCTURE TYPES ---
interface ServiceConfig {
    id: string;
    name: string;
    url: string;
    opc_endpoint?: string;
}

interface InfrastructureConfig {
    supervisors: ServiceConfig[];
    opc_servers: ServiceConfig[];
}

export const ModelGenerator = ({ onNodesDeployed }: { onNodesDeployed: () => void }) => {
    const [model, setModel] = useState<FullModel>(DEFAULT_MODEL);
    const [activeTab, setActiveTab] = useState<"Vars" | "Physics" | "Tuning" | "StepResponse" | "Models" | "Export">("Vars");
    const [selectedGraph, setSelectedGraph] = useState<{cvIdx: number, inputIdx: number, inputType: 'MV' | 'DV'} | null>(null);

    // --- DEPLOYMENT STATE ---
    const [infrastructure, setInfrastructure] = useState<InfrastructureConfig>({ supervisors: [], opc_servers: [] });
    const [selectedSup, setSelectedSup] = useState<string>("");
    const [selectedOpc, setSelectedOpc] = useState<string>("");
    const [deployStatus, setDeployStatus] = useState<string>("");
    const [isDeploying, setIsDeploying] = useState(false);
    
    // --- MODEL LOADING STATE ---
    const [showModelLoadModal, setShowModelLoadModal] = useState(false);
    const [controllers, setControllers] = useState<Array<{id: string, models: string[], active_model?: string, state: unknown}>>([]);
    const [loadingControllers, setLoadingControllers] = useState(false);
    const [showCvGuide, setShowCvGuide] = useState(false);
    const [showMvGuide, setShowMvGuide] = useState(false);
    const [showDvGuide, setShowDvGuide] = useState(false);

    const cvFieldHelp = [
        { key: "Tag", text: "Variable identity used by engine diagnostics. Physical IDs are edited separately in Node mapping." },
        { key: "Description", text: "Operator-friendly label shown in tables and exports." },
        { key: "Units", text: "Engineering units for display and interpretation." },
        { key: "Weight", text: "Higher value increases priority to reduce this CV error." },
        { key: "Alpha", text: "Reference trajectory factor, usually 0.0–1.0 (typical 0.7–0.95): lower is faster, higher is smoother." },
        { key: "Mode", text: "Target/Zone/Maximize/Minimize objective behavior for this CV." },
        { key: "ECE", text: "CV normalization factor. Default is 1; larger value lowers that CV's relative priority." },
        { key: "Limits", text: "Operating and safety envelope used by optimization constraints." },
    ];

    const mvFieldHelp = [
        { key: "R-Wt", text: "Move suppression cost; higher means less aggressive MV movement." },
        { key: "MaxMove", text: "Maximum MV change allowed per control interval." },
        { key: "Mode", text: "Target/Maximize/Minimize objective behavior for this MV." },
        { key: "Target", text: "Optional economic MV target value." },
        { key: "Tgt Wt", text: "MV target tracking weight; zero disables MV target pull." },
        { key: "Limits", text: "Allowed operating bounds enforced by the solver." },
    ];

    const dvFieldHelp = [
        { key: "Tag", text: "Measured disturbance identity; its physical PV is set in Node mapping." },
        { key: "Limits", text: "Expected range used for validation and warning checks." },
    ];


    // Fetch Infrastructure on Mount
    useEffect(() => {
        apiFetch('/api/infrastructure')
            .then(res => res.json())
            .then((data: InfrastructureConfig) => {
                setInfrastructure(data);
                if (data.supervisors.length > 0) setSelectedSup(data.supervisors[0].url);
                if (data.opc_servers.length > 0) setSelectedOpc(data.opc_servers[0].url);
            })
            .catch(err => console.error("Failed to load infrastructure:", err));
    }, []);

    // --- HELPER: RESIZE MATRICES ---
    const updateMatrixSize = (newCvs: VarConfig[], newMvs: VarConfig[], newDvs: VarConfig[]) => {
        const rows = newCvs.length;
        const mvCols = newMvs.length;
        const dvCols = newDvs.length;
        
        const resize = (oldMat: number[][] | undefined, cols: number, defaultVal: number) => {
            const safeMat = oldMat || [];
            return Array(rows).fill(0).map((_, r) => 
                Array(cols).fill(0).map((_, c) => (safeMat[r] && safeMat[r][c] !== undefined ? safeMat[r][c] : defaultVal))
            );
        };

        setModel((prev: FullModel) => ({
            ...prev,
            cvs: newCvs,
            mvs: newMvs,
            dvs: newDvs,
            physics: {
                gain: resize(prev.physics.gain, mvCols, 0),
                tau: resize(prev.physics.tau, mvCols, 60),
                deadTime: resize(prev.physics.deadTime, mvCols, 0),
                gainDv: resize(prev.physics.gainDv, dvCols, 0),
                tauDv: resize(prev.physics.tauDv, dvCols, 60),
                deadTimeDv: resize(prev.physics.deadTimeDv, dvCols, 0)
            }
        }));
    };

    // --- HELPER: MOVE MV TO DV ---
    const moveMvToDv = (mvIdx: number) => {
        const mvToMove = model.mvs[mvIdx];
        const newMv: VarConfig = {
            ...mvToMove,
            id: `dv_${Date.now()}`, // New ID for DV
            weight: 0, // DVs don't have weights in control
            maxMove: undefined // DVs don't have maxMove
        };
        
        const newMvs = model.mvs.filter((_, i) => i !== mvIdx);
        const newDvs = [...model.dvs, newMv];
        
        // Move physics column from MV to DV
        const newGain = model.physics.gain.map(row => row.filter((_, i) => i !== mvIdx));
        const newTau = model.physics.tau.map(row => row.filter((_, i) => i !== mvIdx));
        const newDeadTime = model.physics.deadTime.map(row => row.filter((_, i) => i !== mvIdx));
        
        const newGainDv = model.cvs.map((_, cvIdx) => [...(model.physics.gainDv[cvIdx] || []), model.physics.gain?.[cvIdx]?.[mvIdx] ?? 0]);
        const newTauDv = model.cvs.map((_, cvIdx) => [...(model.physics.tauDv[cvIdx] || []), model.physics.tau?.[cvIdx]?.[mvIdx] ?? 0]);
        const newDeadTimeDv = model.cvs.map((_, cvIdx) => [...(model.physics.deadTimeDv[cvIdx] || []), model.physics.deadTime?.[cvIdx]?.[mvIdx] ?? 0]);
        
        // Handle step response coefficients if in step_response mode
        let newStepCoefficients = model.stepResponseData?.stepCoefficients;
        let newDvCoefficients = model.stepResponseData?.dvCoefficients;
        
        if (model.mode === "step_response" && model.stepResponseData?.stepCoefficients) {
            // Remove MV column from stepCoefficients
            newStepCoefficients = model.stepResponseData.stepCoefficients.map(cvData => 
                cvData.filter((_, i) => i !== mvIdx)
            );
            // Add MV column to dvCoefficients
            newDvCoefficients = model.cvs.map((_, cvIdx) => [
                ...(model.stepResponseData?.dvCoefficients?.[cvIdx] || []),
                (model.stepResponseData?.stepCoefficients?.[cvIdx]?.[mvIdx] || [])
            ]);
        }
        
        setModel({
            ...model,
            mvs: newMvs,
            dvs: newDvs,
            physics: {
                gain: newGain,
                tau: newTau,
                deadTime: newDeadTime,
                gainDv: newGainDv,
                tauDv: newTauDv,
                deadTimeDv: newDeadTimeDv
            },
            stepResponseData: model.stepResponseData ? {
                ...model.stepResponseData,
                stepCoefficients: newStepCoefficients,
                dvCoefficients: newDvCoefficients
            } : undefined
        });
    };

    // --- HELPER: MOVE DV TO MV ---
    const moveDvToMv = (dvIdx: number) => {
        const dvToMove = model.dvs[dvIdx];
        const newMv: VarConfig = {
            ...dvToMove,
            id: `mv_${Date.now()}`,
            weight: 1,
            maxMove: 1
        };
        
        const newDvs = model.dvs.filter((_, i) => i !== dvIdx);
        const newMvs = [...model.mvs, newMv];
        
        // Move physics column from DV to MV
        const newGainDv = model.physics.gainDv.map(row => row.filter((_, i) => i !== dvIdx));
        const newTauDv = model.physics.tauDv.map(row => row.filter((_, i) => i !== dvIdx));
        const newDeadTimeDv = model.physics.deadTimeDv.map(row => row.filter((_, i) => i !== dvIdx));
        
        const newGain = model.cvs.map((_, cvIdx) => [...(model.physics.gain[cvIdx] || []), model.physics.gainDv?.[cvIdx]?.[dvIdx] ?? 0]);
        const newTau = model.cvs.map((_, cvIdx) => [...(model.physics.tau[cvIdx] || []), model.physics.tauDv?.[cvIdx]?.[dvIdx] ?? 0]);
        const newDeadTime = model.cvs.map((_, cvIdx) => [...(model.physics.deadTime[cvIdx] || []), model.physics.deadTimeDv?.[cvIdx]?.[dvIdx] ?? 0]);
        
        // Handle step response coefficients if in step_response mode
        let newStepCoefficients = model.stepResponseData?.stepCoefficients;
        let newDvCoefficients = model.stepResponseData?.dvCoefficients;
        
        if (model.mode === "step_response" && model.stepResponseData?.dvCoefficients) {
            // Remove DV column from dvCoefficients
            newDvCoefficients = model.stepResponseData.dvCoefficients.map(cvData => 
                cvData.filter((_, i) => i !== dvIdx)
            );
            // Add DV column to stepCoefficients
            newStepCoefficients = model.cvs.map((_, cvIdx) => [
                ...(model.stepResponseData?.stepCoefficients?.[cvIdx] || []),
                (model.stepResponseData?.dvCoefficients?.[cvIdx]?.[dvIdx] || [])
            ]);
        }
        
        setModel({
            ...model,
            mvs: newMvs,
            dvs: newDvs,
            physics: {
                gain: newGain,
                tau: newTau,
                deadTime: newDeadTime,
                gainDv: newGainDv,
                tauDv: newTauDv,
                deadTimeDv: newDeadTimeDv
            },
            stepResponseData: model.stepResponseData ? {
                ...model.stepResponseData,
                stepCoefficients: newStepCoefficients,
                dvCoefficients: newDvCoefficients
            } : undefined
        });
    };

    // --- HELPER: LOAD FROM CONTROLLER ---
    const handleLoadFromController = async () => {
        setShowModelLoadModal(true);
        setLoadingControllers(true);
        try {
            const res = await apiFetch('/api/prox/controllers');
            if (res.ok) {
                const data = await res.json();
                setControllers(data);
            }
        } catch (err) {
            console.error('Failed to fetch controllers:', err);
        } finally {
            setLoadingControllers(false);
        }
    };
    
    const handleLoadModelFromController = async (controllerId: string, modelFilename: string) => {
        try {
            // Fetch the specific model file
            const res = await apiFetch(`/api/controller/${controllerId}/models/${modelFilename}`);
            if (!res.ok) {
                alert(`Failed to load model: ${await res.text()}`);
                return;
            }
            
            const raw = await res.json();
            
            const loadedModel = importControllerModel(raw);
            setModel(loadedModel);
            setShowModelLoadModal(false);
            alert(`Loaded model: ${loadedModel.meta.name}\nReview the explicit node mapping before deployment. No controller was started.`);
        } catch (err) {
            console.error(err);
            alert(`Failed to load model from controller: ${err instanceof Error ? err.message : String(err)}`);
        }
    };

    // --- HELPER: JSON LOADER ---
    const handleFileUpload = (e: React.ChangeEvent<HTMLInputElement>) => {
        const file = e.target.files?.[0];
        if (!file) return;
        const reader = new FileReader();
        reader.onload = (event) => {
            try {
                const raw = JSON.parse(event.target?.result as string);
                
                const loadedModel = importControllerModel(raw);
                setModel(loadedModel);
                alert(`Successfully loaded model: ${loadedModel.meta.name}`);
            } catch (err) {
                console.error(err);
                alert(`Failed to import model: ${err instanceof Error ? err.message : String(err)}`);
            }
        };
        reader.readAsText(file);
    };
    // --- HELPER: STEP RESPONSE FILE PARSER ---
    const handleStepResponseUpload = (e: React.ChangeEvent<HTMLInputElement>) => {
        const file = e.target.files?.[0];
        if (!file) return;
        
        const reader = new FileReader();
        reader.onload = (event) => {
            try {
                const text = event.target?.result as string;
                const lines = text.split('\n');
                const nonEmpty = lines.map(l => l.trim()).filter(Boolean);

                // Read header strictly from the top: Last Run line, then numeric rows.
                let scan = 0;
                if (nonEmpty[0]?.toLowerCase().startsWith("last run:")) {
                    scan = 1;
                }

                const headerNumbers: number[] = [];
                while (scan < nonEmpty.length && headerNumbers.length < 7) {
                    const value = Number.parseFloat(nonEmpty[scan]);
                    if (Number.isNaN(value)) break;
                    headerNumbers.push(value);
                    scan++;
                }

                if (headerNumbers.length < 6) {
                    alert("Failed to parse legacy DMC file header.");
                    return;
                }

                const expectedMvs = Math.round(headerNumbers[1]);
                const expectedCvs = Math.round(headerNumbers[2]);
                const expectedHorizon = Math.round(headerNumbers[3]);
                const tssMin = headerNumbers[5];

                if (!expectedMvs || !expectedCvs || !expectedHorizon || !tssMin) {
                    alert("Failed to parse legacy DMC header values (MV/CV/Horizon/TSS).");
                    return;
                }

                const sampleTime = (tssMin * 60) / expectedHorizon;

                const mvDeclRegex = /^-999(?:\.0+)?\s+(\S+)(?:\s+(\S+))?$/i;
                const cvDeclRegex = /^0(?:\.0+)?\s+(\S+)\s+(\S+)\s+\d+\s+\d+$/i;
                const cvBlockHeaderRegex = /^(\S+)\s+(\S+)\s+\d+\s+[+-]?\d+\.\d+$/i;

                const mvNames: string[] = [];
                const cvNames: string[] = [];
                const mvUnits: string[] = [];
                const cvUnits: string[] = [];

                // Read declaration section line-by-line until we have the counts from header.
                for (let i = scan; i < nonEmpty.length; i++) {
                    const line = nonEmpty[i];
                    const firstToken = line.split(/\s+/)[0] || "";

                    const mvMatch = line.match(mvDeclRegex);
                    if (firstToken.startsWith("-999") && mvMatch && mvNames.length < expectedMvs) {
                        mvNames.push(mvMatch[1]);
                        mvUnits.push(mvMatch[2] || '');
                        continue;
                    }

                    const cvMatch = line.match(cvDeclRegex);
                    if (firstToken.startsWith("0") && cvMatch && cvNames.length < expectedCvs) {
                        cvNames.push(cvMatch[1]);
                        cvUnits.push(cvMatch[2]);
                    }

                    if (mvNames.length >= expectedMvs && cvNames.length >= expectedCvs) {
                        scan = i + 1;
                        break;
                    }
                }

                if (mvNames.length !== expectedMvs || cvNames.length !== expectedCvs) {
                    alert(`Failed to parse declarations: expected ${expectedCvs} CVs and ${expectedMvs} MVs, found ${cvNames.length} CVs and ${mvNames.length} MVs.`);
                    return;
                }

                // Find the first CV block start by exact CV name match.
                let idx = nonEmpty.findIndex((l, lineIdx) => {
                    if (lineIdx < scan) return false;
                    const m = l.match(cvBlockHeaderRegex);
                    return !!m && cvNames.includes(m[1]);
                });

                if (idx < 0) {
                    alert("Failed to locate CV response blocks in legacy DMC file.");
                    return;
                }

                const coefficients: number[][][] = [];
                const mvNameSet = new Set(mvNames);

                for (let cvIdx = 0; cvIdx < expectedCvs; cvIdx++) {
                    while (idx < nonEmpty.length) {
                        const m = nonEmpty[idx].match(cvBlockHeaderRegex);
                        if (m && m[1] === cvNames[cvIdx]) break;
                        idx++;
                    }

                    if (idx >= nonEmpty.length) {
                        alert(`Failed to find CV block for ${cvNames[cvIdx]}.`);
                        return;
                    }

                    idx++; // move past CV header

                    const cvResponses = new Map<string, number[]>();

                    while (idx < nonEmpty.length) {
                        const current = nonEmpty[idx];
                        const cvHeader = current.match(cvBlockHeaderRegex);
                        if (cvHeader && cvHeader[1] !== cvNames[cvIdx]) {
                            break;
                        }

                        const token = current.split(/\s+/)[0] || '';
                        if (!mvNameSet.has(token)) {
                            idx++;
                            continue;
                        }

                        const mvName = token;
                        idx++; // move to coefficient rows

                        const mvCoeffs: number[] = [];
                        while (idx < nonEmpty.length && mvCoeffs.length < expectedHorizon) {
                            const row = nonEmpty[idx];
                            const rowToken = row.split(/\s+/)[0] || '';
                            const nextCvHeader = row.match(cvBlockHeaderRegex);
                            if (mvNameSet.has(rowToken) || (nextCvHeader && nextCvHeader[1] !== cvNames[cvIdx])) {
                                break;
                            }

                            const values = row
                                .split(/\s+/)
                                .map(s => parseFloat(s))
                                .filter(v => !isNaN(v));

                            if (values.length > 0) {
                                mvCoeffs.push(...values);
                            }
                            idx++;
                        }

                        if (mvCoeffs.length > 0) {
                            cvResponses.set(mvName, mvCoeffs.slice(0, expectedHorizon));
                        }
                    }

                    const cvData: number[][] = mvNames.map((mvName) => {
                        const found = cvResponses.get(mvName);
                        if (!found) {
                            return Array(expectedHorizon).fill(0);
                        }
                        if (found.length >= expectedHorizon) {
                            return found.slice(0, expectedHorizon);
                        }
                        return [...found, ...Array(expectedHorizon - found.length).fill(0)];
                    });

                    coefficients.push(cvData);
                }
                
                console.log(`✅ Parsed: ${cvNames.length} CVs, ${mvNames.length} MVs`);
                
                // Validate
                if (cvNames.length === 0 || mvNames.length === 0) {
                    alert("Failed to parse file: No CVs or MVs found. Please check file format.");
                    return;
                }
                
                // Use header horizon or detect from coefficients
                const horizon = coefficients[0]?.[0]?.length || expectedHorizon;
                
                // Create VarConfigs
                const newCVs: VarConfig[] = cvNames.map((name, i) => {
                    const limits = { lowLow: 0, low: 10, target: 50, high: 90, highHigh: 100 };
                    return {
                        id: `cv_${i}_${Date.now()}`,
                        name,
                        nodeIds: defaultNodeIds(name, 'cv'),
                        desc: `Imported from ${file.name}`,
                        units: cvUnits[i] || '',
                        weight: name.includes('OP') ? 0 : 1, // FC1OP, FC2OP get weight 0
                        alpha: 0,
                        ece_factor: limits.high - limits.low, // Auto-calculate from span (80)
                        limits
                    };
                });
                
                const newMVs: VarConfig[] = mvNames.map((name, i) => ({
                    id: `mv_${i}_${Date.now()}`,
                    name,
                    nodeIds: defaultNodeIds(name, 'mv'),
                    desc: `Imported from ${file.name}`,
                    units: mvUnits[i] || '',
                    weight: 1,
                    maxMove: 1,
                    limits: { lowLow: 0, low: 2, high: 18, highHigh: 20 }
                }));
                
                // Update model
                setModel({
                    ...model,
                    mode: "step_response",
                    meta: { ...model.meta, name: file.name.replace(/\.[^/.]+$/, "") },
                    cvs: newCVs,
                    mvs: newMVs,
                    dvs: [], // Start with no DVs
                    tuning: {
                        ...model.tuning,
                        predictionHorizon: horizon,
                        sampleTime,
                        tssMin,
                        controlHorizon: Math.min(10, horizon) // Default to 10 or horizon
                    },
                    stepResponseData: {
                        fileName: file.name,
                        stepCoefficients: coefficients,
                        dvCoefficients: [], // Initially empty
                        coefficients, // Keep for backward compatibility
                        parsedAt: new Date().toISOString()
                    }
                });
                
                alert(`✅ Loaded ${file.name}\n${cvNames.length} CVs × ${mvNames.length} MVs × ${horizon} steps\n\nPlease review Variables tab and set limits/weights.`);
                
            } catch (err) {
                console.error(err);
                alert("Failed to parse step response file. Check console for details.");
            }
        };
        
        reader.readAsText(file);
    };

    // --- HELPER: IMPORT STEP RESPONSE JSON (from Step Response Tool) ---
    const handleStepResponseJsonImport = (e: React.ChangeEvent<HTMLInputElement>) => {
        const file = e.target.files?.[0];
        if (!file) return;
        
        const reader = new FileReader();
        reader.onload = (event) => {
            try {
                const json = JSON.parse(event.target?.result as string);
                
                // Validate structure
                if (!json.input_tag || !json.input_type || !json.responses) {
                    alert("Invalid step response JSON format");
                    return;
                }
                
                const inputType = json.input_type; // "MV" or "DV"
                const inputTag = json.input_tag;
                const responses = json.responses as Array<{cv_tag: string, baseline: number, coefficients: number[]}>;
                
                console.log(`📥 Importing ${inputType} step response: ${inputTag} → ${responses.length} CVs`);
                
                // Find or create the input variable
                const isMV = inputType === "MV";
                const existingInputIdx = isMV 
                    ? model.mvs.findIndex(mv => mv.name === inputTag)
                    : model.dvs.findIndex(dv => dv.name === inputTag);
                
                const updatedMVs = [...model.mvs];
                const updatedDVs = [...model.dvs];
                let inputIdx = existingInputIdx;
                
                // Add input if it doesn't exist
                if (existingInputIdx === -1) {
                    const newInput: VarConfig = {
                        id: `${isMV ? 'mv' : 'dv'}_${Date.now()}`,
                        name: inputTag,
                        nodeIds: defaultNodeIds(inputTag, isMV ? 'mv' : 'dv'),
                        desc: `Imported from ${file.name}`,
                        units: '',
                        weight: isMV ? 1 : 0,
                        maxMove: isMV ? 1 : undefined,
                        limits: { lowLow: 0, low: 10, target: 50, high: 90, highHigh: 100 }
                    };
                    
                    if (isMV) {
                        updatedMVs.push(newInput);
                        inputIdx = updatedMVs.length - 1;
                    } else {
                        updatedDVs.push(newInput);
                        inputIdx = updatedDVs.length - 1;
                    }
                }
                
                // Merge CVs and coefficients
                const updatedCVs = [...model.cvs];
                const horizon = responses[0]?.coefficients.length || 45;
                
                // Initialize coefficient arrays if needed
                const numCVs = Math.max(model.cvs.length, responses.length);
                const numMVs = updatedMVs.length;
                const numDVs = updatedDVs.length;
                
                let stepCoefficients = model.stepResponseData?.stepCoefficients || 
                    Array(numCVs).fill(null).map(() => Array(numMVs).fill(null).map(() => Array(horizon).fill(0)));
                let dvCoefficients = model.stepResponseData?.dvCoefficients || 
                    Array(numCVs).fill(null).map(() => Array(numDVs).fill(null).map(() => Array(horizon).fill(0)));
                
                // Ensure proper dimensions
                while (stepCoefficients.length < numCVs) {
                    stepCoefficients.push(Array(numMVs).fill(null).map(() => Array(horizon).fill(0)));
                }
                while (dvCoefficients.length < numCVs) {
                    dvCoefficients.push(Array(numDVs).fill(null).map(() => Array(horizon).fill(0)));
                }
                
                stepCoefficients = stepCoefficients.map(cvData => {
                    while (cvData.length < numMVs) cvData.push(Array(horizon).fill(0));
                    return cvData;
                });
                dvCoefficients = dvCoefficients.map(cvData => {
                    while (cvData.length < numDVs) cvData.push(Array(horizon).fill(0));
                    return cvData;
                });
                
                // Merge responses
                responses.forEach((resp) => {
                    const cvTag = resp.cv_tag;
                    let cvIdx = updatedCVs.findIndex(cv => cv.name === cvTag);
                    
                    // Add CV if it doesn't exist
                    if (cvIdx === -1) {
                        const newCV: VarConfig = {
                            id: `cv_${Date.now()}_${cvTag}`,
                            name: cvTag,
                            nodeIds: defaultNodeIds(cvTag, 'cv'),
                            desc: `Imported from ${file.name}`,
                            units: '',
                            weight: 1,
                            alpha: 0,
                            limits: { lowLow: 0, low: 10, target: resp.baseline, high: 90, highHigh: 100 }
                        };
                        updatedCVs.push(newCV);
                        cvIdx = updatedCVs.length - 1;
                        
                        // Add rows to coefficient matrices
                        stepCoefficients.push(Array(numMVs).fill(null).map(() => Array(horizon).fill(0)));
                        dvCoefficients.push(Array(numDVs).fill(null).map(() => Array(horizon).fill(0)));
                    }
                    
                    // Insert coefficients
                    if (isMV) {
                        stepCoefficients[cvIdx][inputIdx] = resp.coefficients;
                    } else {
                        dvCoefficients[cvIdx][inputIdx] = resp.coefficients;
                    }
                });
                
                // Update model
                setModel({
                    ...model,
                    mode: "step_response",
                    cvs: updatedCVs,
                    mvs: updatedMVs,
                    dvs: updatedDVs,
                    stepResponseData: {
                        fileName: `${model.stepResponseData?.fileName || 'imported'} + ${file.name}`,
                        stepCoefficients,
                        dvCoefficients,
                        coefficients: stepCoefficients,
                        parsedAt: new Date().toISOString()
                    }
                });
                
                alert(`✅ Imported ${inputType} step response for ${inputTag}\n${responses.length} CV responses added\nHorizon: ${horizon} points`);
                
            } catch (err) {
                console.error(err);
                alert("Failed to parse step response JSON. Check console for details.");
            }
        };
        
        reader.readAsText(file);
    };

    const downloadFile = (filename: string, content: string, type: string) => {
        const blob = new Blob([content], { type });
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = filename;
        a.click();
    };

    // JSON and node YAML share one validated physical-node mapping.
    let artifacts: ModelArtifacts | undefined;
    let artifactError = '';
    try {
        artifacts = buildModelArtifacts(model);
    } catch (error) {
        artifactError = error instanceof Error ? error.message : String(error);
    }


    // --- GENERATOR: DIRECT DEPLOY ---
    const handleDirectDeploy = async (mode: 'nodes' | 'controller') => {
        if (!artifacts) {
            setDeployStatus(`Configuration unavailable: ${artifactError}`);
            return;
        }
        if (mode === 'controller' && !selectedSup) {
            setDeployStatus("❌ Please select a target supervisor.");
            return;
        }
        if (mode === 'nodes' && !selectedOpc) {
            setDeployStatus("❌ Please select a target OPC server.");
            return;
        }

        setIsDeploying(true);
        const modeLabel = mode === 'controller' ? 'Controller' : 'Nodes';
        setDeployStatus(`⏳ Packing ${modeLabel}...`);

        const formData = new FormData();
        
        // 1. Targets
        formData.append("target_supervisor", selectedSup);
        formData.append("target_opc", selectedOpc);

        // Find TCP endpoint
        const opcObj = infrastructure.opc_servers.find(s => s.url === selectedOpc);
        if (opcObj && opcObj.opc_endpoint) {
             formData.append("target_opc_tcp", opcObj.opc_endpoint);
        }

        // 2. Payload
        if (mode === 'controller') {
            const jsonContent = artifacts.json;
            const jsonBlob = new Blob([jsonContent], { type: "application/json" });
            formData.append("model_json", jsonBlob, `${model.meta.name || "Model"}.json`);
        }
        
        if (mode === 'nodes') {
            const yamlContent = artifacts.yaml;
            const yamlBlob = new Blob([yamlContent], { type: "text/yaml" });
            formData.append("nodes_yaml", yamlBlob, `${model.meta.name || "Model"}_nodes.yaml`);
        }

        setDeployStatus(`🚀 Sending ${modeLabel}...`);

        try {
            const response = await apiFetch("/api/deploy", {
                method: "POST",
                body: formData,
            }, true);

            const result = await response.text();
            if (response.ok) {
                setDeployStatus(mode === 'nodes'
                    ? 'Node-only deployment confirmed by the server. No controller model was pushed; no controller was started or engaged.'
                    : 'Controller model push confirmed by the server. No nodes were deployed; no controller was started or engaged.');
                if (mode === 'nodes') onNodesDeployed();
            } else {
                setDeployStatus(`❌ Failed: ${result}`);
            }
        } catch (error) {
            setDeployStatus(`❌ Error: ${error}`);
        } finally {
            setIsDeploying(false);
        }
    };

    // Style for shared inputs (Weights, Limits, etc.)
    const numInputClass = "w-full bg-slate-950 border border-slate-800 rounded p-1 text-right outline-none focus:border-indigo-500 transition [appearance:textfield] [&::-webkit-outer-spin-button]:appearance-none [&::-webkit-inner-spin-button]:appearance-none";

    return (
        <div className="flex flex-col h-full bg-slate-950 text-slate-200 p-6 animate-in fade-in">
            {/* HEADER */}
            <div className="flex justify-between items-center mb-6 border-b border-slate-800 pb-4">
                <div className="flex items-center gap-3">
                    <Activity className="text-indigo-400" />
                    <input type="text" value={model.meta.name} onChange={e => setModel({...model, meta: {...model.meta, name: e.target.value}})} className="bg-transparent text-2xl font-bold text-white outline-none w-64" placeholder="Model Name..." />
                    <select 
                        value={model.mode} 
                        onChange={e => {
                            setModel({...model, mode: e.target.value as FullModel['mode']});
                            setActiveTab("Vars"); // Reset to first tab on mode change
                        }}
                        className="bg-slate-800 border border-slate-700 rounded-lg px-3 py-1 text-sm font-bold text-slate-300 outline-none hover:border-indigo-500 transition"
                    >
                        <option value="parametric">📐 Parametric (FOPDT)</option>
                        <option value="step_response">🔬 Step Response</option>
                    </select>
                </div>
                <div className="flex gap-2">
                    <button 
                        onClick={handleLoadFromController}
                        className="flex items-center gap-2 px-4 py-2 bg-indigo-700 hover:bg-indigo-600 rounded-lg cursor-pointer text-sm font-bold text-white transition"
                    >
                        <Download size={16} /> Load from Controller
                    </button>
                    {model.mode === "step_response" && (
                        <label className="flex items-center gap-2 px-4 py-2 bg-slate-800 hover:bg-slate-700 border border-slate-700 rounded-lg cursor-pointer text-sm font-bold text-slate-200">
                            <FileText size={16} /> Import DMC .mdl
                            <input type="file" className="hidden" accept=".txt,.mdl,text/plain" onChange={handleStepResponseUpload}/>
                        </label>
                    )}
                    {model.mode === "step_response" && (
                        <label className="flex items-center gap-2 px-4 py-2 bg-indigo-700 hover:bg-indigo-600 rounded-lg cursor-pointer text-sm font-bold text-white">
                            <Upload size={16} /> Import Step Response JSON
                            <input type="file" className="hidden" accept=".json" onChange={handleStepResponseJsonImport} multiple/>
                        </label>
                    )}
                    <label className="flex items-center gap-2 px-4 py-2 bg-slate-800 hover:bg-slate-700 rounded-lg cursor-pointer text-sm font-bold">
                        <Upload size={16} /> Load Model JSON 
                        <input type="file" className="hidden" accept=".json" onChange={handleFileUpload}/>
                    </label>
                </div>
            </div>

            {/* TABS */}
            <div className="flex gap-1 bg-slate-900 p-1 rounded-lg w-fit mb-6">
                {(() => {
                    // Define tabs based on model type
                    let tabs: (typeof activeTab)[] = [];
                    if (model.mode === "parametric") {
                        tabs = ["Vars", "Physics", "Tuning", "Export"];
                    } else if (model.mode === "step_response") {
                        tabs = ["Vars", "Physics", "Tuning", "StepResponse", "Models", "Export"];
                    }
                    
                    return tabs.map(t => (
                        <button key={t} onClick={() => setActiveTab(t)} className={`px-4 py-2 text-sm font-bold rounded-md transition ${activeTab === t ? 'bg-indigo-600 text-white shadow-lg' : 'text-slate-500 hover:text-slate-300'}`}>
                            {t === "StepResponse" ? "Step Response" : t}
                        </button>
                    ));
                })()}
            </div>

            <div className="flex-1 overflow-y-auto pr-2">
                {artifactError && <div role="alert" className="mb-4 rounded border border-red-800 bg-red-950/30 p-3 text-sm text-red-200">Configuration unavailable: {artifactError}. Export and deployment are disabled until the mapping is complete and unique.</div>}
                
                {/* --- TAB 1: VARIABLES --- */}
                {activeTab === "Vars" && (
                    <div className="space-y-8">
                        
                        {/* CVs TABLE */}
                        <div className="bg-slate-900/50 p-4 rounded-xl border border-slate-800 overflow-x-auto">
                            <div className="flex justify-between mb-4"><h3 className="font-bold text-indigo-300">Controlled Variables (CVs)</h3> <button onClick={() => {
                                const newCV = { 
                                    id: Date.now().toString(), 
                                    name: `CV${model.cvs.length+1}`, 
                                    nodeIds: defaultNodeIds(`CV${model.cvs.length+1}`, 'cv'),
                                    desc: "", 
                                    units: "", 
                                    weight: 1, 
                                    alpha: 0, 
                                    ece_factor: 100, // Default span
                                    optimizationMode: "target" as CvOptimizationMode,
                                    limits: {low:0, high:100, lowLow:0, highHigh:100, target:50} 
                                };
                                updateMatrixSize([...model.cvs, newCV], model.mvs, model.dvs);
                            }} className="p-1 bg-indigo-500/20 text-indigo-300 rounded hover:bg-indigo-500/30"><Plus size={16}/></button></div>
                            <div className="mb-4">
                                <button
                                    type="button"
                                    onClick={() => setShowCvGuide(prev => !prev)}
                                    className="w-full flex items-center justify-between px-3 py-2 bg-slate-950/60 border border-slate-800 rounded-lg text-left hover:border-slate-700 transition"
                                >
                                    <span className="text-[10px] uppercase text-slate-500 font-bold">Quick Field Guide (CV)</span>
                                    <span className="text-xs text-slate-400">{showCvGuide ? 'Hide' : 'Show'}</span>
                                </button>
                                {showCvGuide && (
                                    <div className="mt-2 p-3 bg-slate-950/60 border border-slate-800 rounded-lg">
                                        <div className="grid grid-cols-1 md:grid-cols-2 gap-1.5">
                                            {cvFieldHelp.map(item => (
                                                <p key={item.key} className="text-[11px] text-slate-400">
                                                    <span className="text-slate-300 font-semibold">{item.key}:</span> {item.text}
                                                </p>
                                            ))}
                                        </div>
                                    </div>
                                )}
                            </div>
                            <table className="w-full text-left text-xs">
                                <thead>
                                    <tr className="text-slate-500 border-b border-slate-800">
                                        <th className="p-2 w-24">Tag</th>
                                        <th className="p-2 w-32">Description</th>
                                        <th className="p-2 w-16">Units</th>
                                        <th className="p-2 w-16 text-right">Weight</th>
                                        <th className="p-2 w-16 text-right text-indigo-300">Alpha</th>
                                        <th className="p-2 w-24 text-right text-sky-300">Mode</th>
                                        <th className="p-2 w-16 text-right text-purple-400" title="Equal Concern Error - Normalization factor (defaults to span)">ECE</th>
                                        <th className="p-2 w-16 text-right text-red-400">LowLow Limit</th>
                                        <th className="p-2 w-16 text-right text-emerald-400">Low Limit</th>
                                        <th className="p-2 w-16 text-right text-blue-400">Target</th>
                                        <th className="p-2 w-16 text-right text-emerald-400">High Limit</th>
                                        <th className="p-2 w-16 text-right text-red-400">High High Limit</th>
                                        <th className="p-2 w-8"></th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {model.cvs.map((cv, idx) => {
                                        const span = cv.limits.high - cv.limits.low;
                                        const eceFactor = cv.ece_factor ?? span;
                                        return (
                                        <tr key={cv.id} className="border-b border-slate-800/50">
                                            <td className="p-1"><input value={cv.name} onChange={e => { const n = [...model.cvs]; n[idx].name = e.target.value; setModel({...model, cvs: n})}} className="w-full bg-slate-950 border border-slate-800 rounded p-1 text-white" /></td>
                                            <td className="p-1"><input value={cv.desc} onChange={e => { const n = [...model.cvs]; n[idx].desc = e.target.value; setModel({...model, cvs: n})}} className="w-full bg-slate-950 border border-slate-800 rounded p-1" /></td>
                                            <td className="p-1"><input value={cv.units} onChange={e => { const n = [...model.cvs]; n[idx].units = e.target.value; setModel({...model, cvs: n})}} className="w-full bg-slate-950 border border-slate-800 rounded p-1" /></td>
                                            <td className="p-1"><input type="number" value={cv.weight} onChange={e => { const n = [...model.cvs]; n[idx].weight = parseFloat(e.target.value); setModel({...model, cvs: n})}} className={numInputClass} /></td>
                                            <td className="p-1"><input type="number" step="0.1" value={cv.alpha} onChange={e => { const n = [...model.cvs]; n[idx].alpha = parseFloat(e.target.value); setModel({...model, cvs: n})}} className={`${numInputClass} text-indigo-300`} /></td>
                                            <td className="p-1">
                                                <select
                                                    value={cv.optimizationMode ?? "target"}
                                                    onChange={e => {
                                                        const n = [...model.cvs];
                                                        n[idx].optimizationMode = e.target.value as CvOptimizationMode;
                                                        setModel({...model, cvs: n});
                                                    }}
                                                    className="w-full bg-slate-950 border border-slate-800 rounded p-1 text-sky-300"
                                                >
                                                    <option value="target">Target</option>
                                                    <option value="zone">Zone</option>
                                                    <option value="maximize">Maximize</option>
                                                    <option value="minimize">Minimize</option>
                                                </select>
                                            </td>
                                            <td className="p-1"><input 
                                                type="number" 
                                                step="0.1"
                                                value={eceFactor} 
                                                onChange={e => { const n = [...model.cvs]; n[idx].ece_factor = parseFloat(e.target.value); setModel({...model, cvs: n})}} 
                                                className={`${numInputClass} text-purple-400`}
                                                title={`Auto-calculated span: ${span.toFixed(1)}`}
                                                placeholder={span.toFixed(1)}
                                            /></td>
                                            <td className="p-1"><input type="number" value={cv.limits.lowLow} onChange={e => { const n = [...model.cvs]; n[idx].limits.lowLow = parseFloat(e.target.value); setModel({...model, cvs: n})}} className={`${numInputClass} text-red-400`} /></td>
                                            <td className="p-1"><input type="number" value={cv.limits.low} onChange={e => { const n = [...model.cvs]; n[idx].limits.low = parseFloat(e.target.value); setModel({...model, cvs: n})}} className={`${numInputClass} text-emerald-400`} /></td>
                                            <td className="p-1"><input type="number" value={cv.limits.target} onChange={e => { const n = [...model.cvs]; n[idx].limits.target = parseFloat(e.target.value); setModel({...model, cvs: n})}} className={`${numInputClass} text-blue-400`} /></td>
                                            <td className="p-1"><input type="number" value={cv.limits.high} onChange={e => { const n = [...model.cvs]; n[idx].limits.high = parseFloat(e.target.value); setModel({...model, cvs: n})}} className={`${numInputClass} text-emerald-400`} /></td>
                                            <td className="p-1"><input type="number" value={cv.limits.highHigh} onChange={e => { const n = [...model.cvs]; n[idx].limits.highHigh = parseFloat(e.target.value); setModel({...model, cvs: n})}} className={`${numInputClass} text-red-400`} /></td>
                                            <td className="p-1 text-center"><button onClick={() => updateMatrixSize(model.cvs.filter((_, i) => i !== idx), model.mvs, model.dvs)} className="text-red-500 hover:text-red-400"><Trash2 size={14}/></button></td>
                                        </tr>
                                    )})}
                                </tbody>
                            </table>
                        </div>

                        {/* MVs TABLE */}
                        <div className="bg-slate-900/50 p-4 rounded-xl border border-slate-800 overflow-x-auto">
                            <div className="flex justify-between mb-4"><h3 className="font-bold text-indigo-300">Manipulated Variables (MVs)</h3> <button onClick={() => updateMatrixSize(model.cvs, [...model.mvs, { id: Date.now().toString(), name: `MV${model.mvs.length+1}`, nodeIds: defaultNodeIds(`MV${model.mvs.length+1}`, 'mv'), desc: "", units: "", weight: 1, maxMove: 5, mvOptimizationMode: "target", limits: {low:0, high:100, lowLow:0, highHigh:100}, target: undefined, targetWeight: 0 }], model.dvs)} className="p-1 bg-indigo-500/20 text-indigo-300 rounded hover:bg-indigo-500/30"><Plus size={16}/></button></div>
                            <div className="mb-4">
                                <button
                                    type="button"
                                    onClick={() => setShowMvGuide(prev => !prev)}
                                    className="w-full flex items-center justify-between px-3 py-2 bg-slate-950/60 border border-slate-800 rounded-lg text-left hover:border-slate-700 transition"
                                >
                                    <span className="text-[10px] uppercase text-slate-500 font-bold">Quick Field Guide (MV)</span>
                                    <span className="text-xs text-slate-400">{showMvGuide ? 'Hide' : 'Show'}</span>
                                </button>
                                {showMvGuide && (
                                    <div className="mt-2 p-3 bg-slate-950/60 border border-slate-800 rounded-lg">
                                        <div className="grid grid-cols-1 md:grid-cols-2 gap-1.5">
                                            {mvFieldHelp.map(item => (
                                                <p key={item.key} className="text-[11px] text-slate-400">
                                                    <span className="text-slate-300 font-semibold">{item.key}:</span> {item.text}
                                                </p>
                                            ))}
                                        </div>
                                    </div>
                                )}
                            </div>
                            <table className="w-full text-left text-xs">
                                <thead>
                                    <tr className="text-slate-500 border-b border-slate-800">
                                        <th className="p-2 w-24">Tag</th>
                                        <th className="p-2 w-32">Description</th>
                                        <th className="p-2 w-16">Units</th>
                                        <th className="p-2 w-16 text-right">R-Wt</th>
                                        <th className="p-2 w-16 text-right text-red-400">Low Low Limit</th>
                                        <th className="p-2 w-16 text-right text-emerald-400">Low Limit</th>
                                        <th className="p-2 w-16 text-right text-amber-400">MaxMove</th>
                                        <th className="p-2 w-16 text-right text-emerald-400">High Limit</th>
                                        <th className="p-2 w-16 text-right text-red-400">High High Limit</th>
                                        <th className="p-2 w-24 text-right text-sky-300">Mode</th>
                                        <th className="p-2 w-16 text-right text-cyan-400">Target</th>
                                        <th className="p-2 w-16 text-right text-cyan-400">Tgt Wt</th>
                                        <th className="p-2 w-8"></th>
                                        <th className="p-2 w-8" title="Move to DV">→DV</th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {model.mvs.map((mv, idx) => (
                                        <tr key={mv.id} className="border-b border-slate-800/50">
                                            <td className="p-1"><input value={mv.name} onChange={e => { const n = [...model.mvs]; n[idx].name = e.target.value; setModel({...model, mvs: n})}} className="w-full bg-slate-950 border border-slate-800 rounded p-1 text-white" /></td>
                                            <td className="p-1"><input value={mv.desc} onChange={e => { const n = [...model.mvs]; n[idx].desc = e.target.value; setModel({...model, mvs: n})}} className="w-full bg-slate-950 border border-slate-800 rounded p-1" /></td>
                                            <td className="p-1"><input value={mv.units} onChange={e => { const n = [...model.mvs]; n[idx].units = e.target.value; setModel({...model, mvs: n})}} className="w-full bg-slate-950 border border-slate-800 rounded p-1" /></td>
                                            <td className="p-1"><input type="number" value={mv.weight} onChange={e => { const n = [...model.mvs]; n[idx].weight = parseFloat(e.target.value); setModel({...model, mvs: n})}} className={numInputClass} /></td>
                                            <td className="p-1"><input type="number" value={mv.limits.lowLow} onChange={e => { const n = [...model.mvs]; n[idx].limits.lowLow = parseFloat(e.target.value); setModel({...model, mvs: n})}} className={`${numInputClass} text-red-400`} /></td>
                                            <td className="p-1"><input type="number" value={mv.limits.low} onChange={e => { const n = [...model.mvs]; n[idx].limits.low = parseFloat(e.target.value); setModel({...model, mvs: n})}} className={`${numInputClass} text-emerald-400`} /></td>
                                            <td className="p-1"><input type="number" value={mv.maxMove} onChange={e => { const n = [...model.mvs]; n[idx].maxMove = parseFloat(e.target.value); setModel({...model, mvs: n})}} className={`${numInputClass} text-amber-400`} /></td>
                                            <td className="p-1"><input type="number" value={mv.limits.high} onChange={e => { const n = [...model.mvs]; n[idx].limits.high = parseFloat(e.target.value); setModel({...model, mvs: n})}} className={`${numInputClass} text-emerald-400`} /></td>
                                            <td className="p-1"><input type="number" value={mv.limits.highHigh} onChange={e => { const n = [...model.mvs]; n[idx].limits.highHigh = parseFloat(e.target.value); setModel({...model, mvs: n})}} className={`${numInputClass} text-red-400`} /></td>
                                            <td className="p-1">
                                                <select
                                                    value={mv.mvOptimizationMode ?? "target"}
                                                    onChange={e => {
                                                        const n = [...model.mvs];
                                                        n[idx].mvOptimizationMode = e.target.value as MvOptimizationMode;
                                                        setModel({...model, mvs: n});
                                                    }}
                                                    className="w-full bg-slate-950 border border-slate-800 rounded p-1 text-sky-300"
                                                >
                                                    <option value="target">Target</option>
                                                    <option value="maximize">Maximize</option>
                                                    <option value="minimize">Minimize</option>
                                                </select>
                                            </td>
                                            <td className="p-1"><input type="number" value={mv.target ?? ""} onChange={e => { const n = [...model.mvs]; n[idx].target = e.target.value ? parseFloat(e.target.value) : undefined; setModel({...model, mvs: n})}} placeholder="Optional" className={`${numInputClass} text-cyan-400`} /></td>
                                            <td className="p-1"><input type="number" value={mv.targetWeight ?? 0} onChange={e => { const n = [...model.mvs]; n[idx].targetWeight = parseFloat(e.target.value) || 0; setModel({...model, mvs: n})}} className={`${numInputClass} text-cyan-400`} /></td>
                                            <td className="p-1 text-center"><button onClick={() => updateMatrixSize(model.cvs, model.mvs.filter((_, i) => i !== idx), model.dvs)} className="text-red-500 hover:text-red-400"><Trash2 size={14}/></button></td>
                                            <td className="p-1 text-center"><button onClick={() => moveMvToDv(idx)} className="text-pink-400 hover:text-pink-300" title="Move to DV"><ArrowDown size={14}/></button></td>
                                        </tr>
                                    ))}
                                </tbody>
                            </table>
                        </div>

                         {/* DVs TABLE */}
                         <div className="bg-slate-900/50 p-4 rounded-xl border border-slate-800 overflow-x-auto">
                            <div className="flex justify-between mb-4">
                                <h3 className="font-bold text-indigo-300">Disturbance Variables (DVs)</h3> 
                                <button onClick={() => updateMatrixSize(model.cvs, model.mvs, [...model.dvs, { id: Date.now().toString(), name: `DV${model.dvs.length+1}`, nodeIds: defaultNodeIds(`DV${model.dvs.length+1}`, 'dv'), desc: "", units: "", weight: 0, limits: {low:0, high:100, lowLow:0, highHigh:100} }])} className="p-1 bg-indigo-500/20 text-indigo-300 rounded hover:bg-indigo-500/30"><Plus size={16}/></button>
                            </div>
                            <div className="mb-4">
                                <button
                                    type="button"
                                    onClick={() => setShowDvGuide(prev => !prev)}
                                    className="w-full flex items-center justify-between px-3 py-2 bg-slate-950/60 border border-slate-800 rounded-lg text-left hover:border-slate-700 transition"
                                >
                                    <span className="text-[10px] uppercase text-slate-500 font-bold">Quick Field Guide (DV)</span>
                                    <span className="text-xs text-slate-400">{showDvGuide ? 'Hide' : 'Show'}</span>
                                </button>
                                {showDvGuide && (
                                    <div className="mt-2 p-3 bg-slate-950/60 border border-slate-800 rounded-lg">
                                        <div className="grid grid-cols-1 md:grid-cols-2 gap-1.5">
                                            {dvFieldHelp.map(item => (
                                                <p key={item.key} className="text-[11px] text-slate-400">
                                                    <span className="text-slate-300 font-semibold">{item.key}:</span> {item.text}
                                                </p>
                                            ))}
                                        </div>
                                    </div>
                                )}
                            </div>
                            <table className="w-full text-left text-xs">
                                <thead>
                                    <tr className="text-slate-500 border-b border-slate-800">
                                        <th className="p-2 w-24">Tag</th>
                                        <th className="p-2 w-32">Description</th>
                                        <th className="p-2 w-16">Units</th>
                                        <th className="p-2 w-16 text-right text-emerald-400">Low Limit</th>
                                        <th className="p-2 w-16 text-right text-emerald-400">High Limit</th>
                                        <th className="p-2 w-8" title="Move to MV">→MV</th>
                                        <th className="p-2 w-8"></th>
                                    </tr>
                                </thead>
                                <tbody>
                                    {model.dvs.map((dv, idx) => (
                                        <tr key={dv.id} className="border-b border-slate-800/50">
                                            <td className="p-1"><input value={dv.name} onChange={e => { const n = [...model.dvs]; n[idx].name = e.target.value; setModel({...model, dvs: n})}} className="w-full bg-slate-950 border border-slate-800 rounded p-1 text-white" /></td>
                                            <td className="p-1"><input value={dv.desc} onChange={e => { const n = [...model.dvs]; n[idx].desc = e.target.value; setModel({...model, dvs: n})}} className="w-full bg-slate-950 border border-slate-800 rounded p-1" /></td>
                                            <td className="p-1"><input value={dv.units} onChange={e => { const n = [...model.dvs]; n[idx].units = e.target.value; setModel({...model, dvs: n})}} className="w-full bg-slate-950 border border-slate-800 rounded p-1" /></td>
                                            
                                            <td className="p-1"><input type="number" value={dv.limits.low} onChange={e => { const n = [...model.dvs]; n[idx].limits.low = parseFloat(e.target.value); setModel({...model, dvs: n})}} className={`${numInputClass} text-emerald-400`} /></td>
                                            <td className="p-1"><input type="number" value={dv.limits.high} onChange={e => { const n = [...model.dvs]; n[idx].limits.high = parseFloat(e.target.value); setModel({...model, dvs: n})}} className={`${numInputClass} text-emerald-400`} /></td>
                                            
                                            <td className="p-1 text-center"><button onClick={() => moveDvToMv(idx)} className="text-pink-400 hover:text-pink-300" title="Move to MV"><ArrowUp size={14}/></button></td>
                                            <td className="p-1 text-center"><button onClick={() => updateMatrixSize(model.cvs, model.mvs, model.dvs.filter((_, i) => i !== idx))} className="text-red-500 hover:text-red-400"><Trash2 size={14}/></button></td>
                                        </tr>
                                    ))}
                                    {model.dvs.length === 0 && <tr><td colSpan={7} className="p-4 text-center text-slate-600 italic">No DVs configured. Add one to enable Feedforward models.</td></tr>}
                                </tbody>
                            </table>
                        </div>
                        <details className="rounded-xl border border-slate-700 bg-slate-900/50 p-4">
                            <summary className="cursor-pointer font-bold text-indigo-300">Node mapping — review / edit exact OPC IDs</summary>
                            <p className="my-3 text-xs text-slate-400">New variables receive conventional defaults. Imported mappings are never regenerated when labels change. Blank required IDs and collisions block export. Optional applied_sp is measured applied-SP feedback; absent feedback remains SP-register-only. OP is a separate PID diagnostic. Clearing an optional ID removes that mapping.</p>
                            {(['cvs', 'mvs', 'dvs'] as const).map(group => model[group].map((variable, index) => {
                                const required = group === 'cvs' ? ['pv', 'target', 'prediction'] : group === 'mvs' ? ['pv', 'sp', 'op', 'mode', 'mode_target', 'future_plan'] : ['pv'];
                                const optional = group === 'mvs' ? ['applied_sp', 'target'] : [];
                                const fields = [...new Set([...required, ...optional, ...Object.keys(variable.nodeIds).filter(key => key !== 'limits')])];
                                const limitFields = [...new Set(['low', 'high', 'll', 'hh', ...Object.keys(variable.nodeIds.limits ?? {})])];
                                const editMapping = (key: string, value: string, isLimit: boolean) => {
                                    const nodeIds: VarConfig['nodeIds'] & { limits: Record<string, string> } = { ...variable.nodeIds, limits: { ...variable.nodeIds.limits } };
                                    if (isLimit) {
                                        if (!value && key !== 'low' && key !== 'high') delete nodeIds.limits[key];
                                        else nodeIds.limits[key] = value;
                                    } else if (!value && !required.includes(key)) delete nodeIds[key];
                                    else nodeIds[key] = value;
                                    setModel({ ...model, [group]: model[group].map((entry, i) => i === index ? { ...entry, nodeIds } : entry) });
                                };
                                return <fieldset key={`${group}:${variable.id}`} className="mb-4 border-t border-slate-800 pt-3">
                                    <legend className="text-sm font-semibold">{group.slice(0, -1).toUpperCase()}: {variable.name}</legend>
                                    <div className="grid grid-cols-1 gap-2 md:grid-cols-2">
                                        {[...fields.map(key => ({ key, isLimit: false })), ...limitFields.map(key => ({ key, isLimit: true }))].map(({ key, isLimit }) => {
                                            const value = isLimit ? variable.nodeIds.limits?.[key] : variable.nodeIds[key];
                                            const needed = isLimit ? key === 'low' || key === 'high' : required.includes(key);
                                            return <label key={`${isLimit}:${key}`} className="flex items-center gap-2 text-xs">
                                                <span className="w-36 shrink-0">{isLimit ? 'limits.' : ''}{key}{needed ? ' (required)' : ' (optional)'}</span>
                                                <input aria-label={`${variable.name} ${isLimit ? 'limits.' : ''}${key} node ID`} value={typeof value === 'string' ? value : ''} onChange={event => editMapping(key, event.target.value, isLimit)} className="min-w-0 flex-1 rounded border border-slate-700 bg-slate-950 p-1 font-mono" />
                                            </label>;
                                        })}
                                    </div>
                                </fieldset>;
                            }))}
                        </details>
                    </div>
                )}

                {/* --- TAB 2: PHYSICS --- */}
                {activeTab === "Physics" && (
                    <>
                        {model.mode === "parametric" ? (
                            <div className="space-y-6">
                                {/* Gain Matrix (MV) */}
                                <MatrixEditor
                                    title="Gain Matrix (K) - Control Models (MV → CV)"
                                    rowLabels={model.cvs.map(cv => cv.name)}
                                    colLabels={model.mvs.map(mv => mv.name)}
                                    matrix={model.physics.gain}
                                    onChange={(row, col, value) => {
                                        const newMat = model.physics.gain.map(r => [...r]);
                                        newMat[row][col] = value;
                                        setModel({...model, physics: { ...model.physics, gain: newMat }});
                                    }}
                                    matrixType="gain"
                                />

                                {/* Time Constant Matrix (MV) */}
                                <MatrixEditor
                                    title="Time Constant (Tau) - Control Models (MV → CV)"
                                    rowLabels={model.cvs.map(cv => cv.name)}
                                    colLabels={model.mvs.map(mv => mv.name)}
                                    matrix={model.physics.tau}
                                    onChange={(row, col, value) => {
                                        const newMat = model.physics.tau.map(r => [...r]);
                                        newMat[row][col] = value;
                                        setModel({...model, physics: { ...model.physics, tau: newMat }});
                                    }}
                                />

                                {/* Dead Time Matrix (MV) */}
                                <MatrixEditor
                                    title="Dead Time (Theta) - Control Models (MV → CV)"
                                    rowLabels={model.cvs.map(cv => cv.name)}
                                    colLabels={model.mvs.map(mv => mv.name)}
                                    matrix={model.physics.deadTime}
                                    onChange={(row, col, value) => {
                                        const newMat = model.physics.deadTime.map(r => [...r]);
                                        newMat[row][col] = value;
                                        setModel({...model, physics: { ...model.physics, deadTime: newMat }});
                                    }}
                                />

                                {/* Disturbance Variable Matrices */}
                                {model.dvs.length > 0 && (
                                    <>
                                        <MatrixEditor
                                            title="Gain Matrix (K) - Feedforward Models (DV → CV)"
                                            rowLabels={model.cvs.map(cv => cv.name)}
                                            colLabels={model.dvs.map(dv => dv.name)}
                                            matrix={model.physics.gainDv}
                                            onChange={(row, col, value) => {
                                                const newMat = model.physics.gainDv.map(r => [...r]);
                                                newMat[row][col] = value;
                                                setModel({...model, physics: { ...model.physics, gainDv: newMat }});
                                            }}
                                            matrixType="gain"
                                        />

                                        <MatrixEditor
                                            title="Time Constant (Tau) - Feedforward Models (DV → CV)"
                                            rowLabels={model.cvs.map(cv => cv.name)}
                                            colLabels={model.dvs.map(dv => dv.name)}
                                            matrix={model.physics.tauDv}
                                            onChange={(row, col, value) => {
                                                const newMat = model.physics.tauDv.map(r => [...r]);
                                                newMat[row][col] = value;
                                                setModel({...model, physics: { ...model.physics, tauDv: newMat }});
                                            }}
                                        />

                                        <MatrixEditor
                                            title="Dead Time (Theta) - Feedforward Models (DV → CV)"
                                            rowLabels={model.cvs.map(cv => cv.name)}
                                            colLabels={model.dvs.map(dv => dv.name)}
                                            matrix={model.physics.deadTimeDv}
                                            onChange={(row, col, value) => {
                                                const newMat = model.physics.deadTimeDv.map(r => [...r]);
                                                newMat[row][col] = value;
                                                setModel({...model, physics: { ...model.physics, deadTimeDv: newMat }});
                                            }}
                                        />
                                    </>
                                )}
                            </div>
                        ) : (
                            // Step Response Mode
                            <div className="max-w-3xl space-y-6">
                                <div className="bg-slate-900/50 p-6 rounded-xl border border-slate-800">
                                    <h3 className="font-bold text-purple-300 text-lg mb-4 flex items-center gap-2">
                                        <FileText size={20} />
                                        Step Response Model
                                    </h3>
                                    
                                    {model.stepResponseData ? (
                                        <div className="space-y-4">
                                            <div className="bg-emerald-900/20 border border-emerald-500/30 rounded-lg p-4 flex items-start gap-3">
                                                <CheckCircle className="text-emerald-400 flex-shrink-0 mt-0.5" size={20} />
                                                <div className="flex-1">
                                                    <p className="font-bold text-emerald-300">Model Loaded Successfully</p>
                                                    <p className="text-sm text-slate-400 mt-1">File: {model.stepResponseData.fileName}</p>
                                                    <p className="text-xs text-slate-500 mt-1">
                                                        Imported: {new Date(model.stepResponseData.parsedAt).toLocaleString()}
                                                    </p>
                                                </div>
                                            </div>
                                            
                                            <div className="grid grid-cols-3 gap-4">
                                                <div className="bg-slate-800/50 p-3 rounded border border-slate-700">
                                                    <p className="text-xs text-slate-500 uppercase font-bold">CVs</p>
                                                    <p className="text-2xl font-bold text-indigo-400">{model.cvs.length}</p>
                                                </div>
                                                <div className="bg-slate-800/50 p-3 rounded border border-slate-700">
                                                    <p className="text-xs text-slate-500 uppercase font-bold">MVs</p>
                                                    <p className="text-2xl font-bold text-amber-400">{model.mvs.length}</p>
                                                </div>
                                                <div className="bg-slate-800/50 p-3 rounded border border-slate-700">
                                                    <p className="text-xs text-slate-500 uppercase font-bold">Steps</p>
                                                    <p className="text-2xl font-bold text-purple-400">
                                                        {model.stepResponseData.coefficients[0]?.[0]?.length || 0}
                                                    </p>
                                                </div>
                                            </div>
                                            
                                            <div className="bg-slate-800/30 rounded p-4 border border-slate-700">
                                                <p className="text-xs text-slate-500 font-bold mb-2">CV Names:</p>
                                                <p className="text-sm text-slate-300 font-mono">
                                                    {model.cvs.map(cv => cv.name).join(', ')}
                                                </p>
                                            </div>
                                            
                                            <div className="bg-slate-800/30 rounded p-4 border border-slate-700">
                                                <p className="text-xs text-slate-500 font-bold mb-2">MV Names:</p>
                                                <p className="text-sm text-slate-300 font-mono">
                                                    {model.mvs.map(mv => mv.name).join(', ')}
                                                </p>
                                            </div>
                                            
                                            {model.dvs.length > 0 && (
                                                <div className="bg-slate-800/30 rounded p-4 border border-slate-700">
                                                    <p className="text-xs text-slate-500 font-bold mb-2">DV Names:</p>
                                                    <p className="text-sm text-pink-300 font-mono">
                                                        {model.dvs.map(dv => dv.name).join(', ')}
                                                    </p>
                                                </div>
                                            )}
                                            
                                            <div className="bg-blue-900/20 border border-blue-500/30 rounded p-4">
                                                <p className="text-sm text-blue-300">
                                                    <strong>📊 Step Response Coverage:</strong><br/>
                                                    MV responses: {model.stepResponseData.stepCoefficients?.filter(cvData => cvData.some(mvData => mvData.some(v => v !== 0))).length || 0}/{model.cvs.length} CVs<br/>
                                                    {model.dvs.length > 0 && model.stepResponseData.dvCoefficients && (
                                                        <>DV responses: {model.stepResponseData.dvCoefficients?.filter(cvData => cvData.some(dvData => dvData.some(v => v !== 0))).length || 0}/{model.cvs.length} CVs</>
                                                    )}
                                                </p>
                                            </div>
                                            
                                            <div className="bg-blue-900/20 border border-blue-500/30 rounded p-4">
                                                <p className="text-sm text-blue-300">
                                                    ℹ️ Step response coefficients are stored internally. 
                                                    Please verify Variables tab and set appropriate weights, limits, and descriptions.
                                                    Use "Import Step Response JSON" to add MV or DV responses from Step Response Tool.
                                                </p>
                                            </div>
                                            
                                            <label className="flex items-center justify-center gap-2 px-4 py-2 bg-indigo-700 hover:bg-indigo-600 rounded-lg cursor-pointer text-sm font-bold text-white w-full">
                                                <FileText size={16} /> Re-import Legacy Step Response File
                                                <input type="file" className="hidden" accept=".txt,.mdl,text/plain" onChange={handleStepResponseUpload}/>
                                            </label>
                                        </div>
                                    ) : (
                                        <div className="space-y-4">
                                            <div className="bg-amber-900/20 border border-amber-500/30 rounded p-4 flex items-start gap-3">
                                                <AlertCircle className="text-amber-400 flex-shrink-0 mt-0.5" size={20} />
                                                <div>
                                                    <p className="font-bold text-amber-300">No Step Response Model Loaded</p>
                                                    <p className="text-sm text-slate-400 mt-1">
                                                        Import a legacy DMC step response file (.txt format) to populate the model.
                                                    </p>
                                                </div>
                                            </div>
                                            
                                            <div className="border-2 border-dashed border-slate-700 rounded-lg p-8 text-center hover:border-indigo-500 transition">
                                                <FileText className="mx-auto text-slate-600 mb-3" size={48} />
                                                <label className="cursor-pointer">
                                                    <span className="text-slate-400 hover:text-indigo-300 transition">
                                                        Click to browse or drag & drop .mdl/.txt file
                                                    </span>
                                                    <input type="file" className="hidden" accept=".txt,.mdl,text/plain" onChange={handleStepResponseUpload}/>
                                                </label>
                                                <p className="text-xs text-slate-600 mt-2">Supported: Legacy DMC formats (.mdl, .txt)</p>
                                            </div>
                                            
                                            <div className="bg-slate-800/50 rounded p-4 border border-slate-700">
                                                <p className="text-xs text-slate-500 font-bold mb-2">Expected Format:</p>
                                                <ul className="text-xs text-slate-400 space-y-1 list-disc list-inside">
                                                    <li>Header line with: 0, num_MVs, horizon, tss_minutes</li>
                                                    <li>CV sections with MV relationships</li>
                                                    <li>Step response coefficients (typically 45 steps)</li>
                                                </ul>
                                            </div>
                                        </div>
                                    )}
                                </div>
                            </div>
                        )}
                    </>
                )}
                
                {activeTab === "Tuning" && (
                     <div className="bg-slate-900/50 p-6 rounded-xl border border-slate-800 max-w-2xl">
                        <div className="grid grid-cols-2 gap-6 mb-6">
                            <div><label className="block text-xs text-slate-500 font-bold uppercase mb-1">Sample Time (sec)</label><input type="number" value={model.tuning.sampleTime} onChange={e => setModel({...model, tuning: {...model.tuning, predictionHorizon: undefined, sampleTime: parseFloat(e.target.value)}})} className={numInputClass}/></div>
                            <div><label className="block text-xs text-slate-500 font-bold uppercase mb-1">Steady State Time (min)</label><input type="number" value={model.tuning.tssMin} onChange={e => setModel({...model, tuning: {...model.tuning, predictionHorizon: undefined, tssMin: parseFloat(e.target.value)}})} className={numInputClass}/></div>
                             <div><label className="block text-xs text-slate-500 font-bold uppercase mb-1">Control Horizon (Moves)</label><input type="number" value={model.tuning.controlHorizon} onChange={e => setModel({...model, tuning: {...model.tuning, controlHorizon: parseFloat(e.target.value)}})} className={numInputClass}/></div>
                            <div><div className="p-3 bg-indigo-500/10 rounded-lg border border-indigo-500/20 mt-4"><span className="text-indigo-300 text-xs font-bold block">Prediction Horizon (imported or derived from timing)</span><span className="text-2xl font-bold text-white">{model.tuning.predictionHorizon ?? Math.ceil((model.tuning.tssMin * 60) / model.tuning.sampleTime)} <span className="text-sm text-slate-500">steps</span></span></div></div>
                        </div>
                        <h3 className="font-bold text-indigo-300 mb-4 border-b border-slate-800 pb-2">Optimization Solver</h3>
                        <div className="grid grid-cols-2 gap-6">
                            <div><label className="block text-xs text-slate-500 font-bold uppercase mb-1">Solver Tolerance</label><p className="text-[10px] text-slate-600 mb-1">Stop when error &lt; value</p><input type="number" step="0.00001" value={model.tuning.solverTol} onChange={e => setModel({...model, tuning: {...model.tuning, solverTol: parseFloat(e.target.value)}})} className={numInputClass}/></div>
                            <div><label className="block text-xs text-slate-500 font-bold uppercase mb-1">Max Iterations</label><p className="text-[10px] text-slate-600 mb-1">Safety stop to prevent infinite loops</p><input type="number" value={model.tuning.maxIter} onChange={e => setModel({...model, tuning: {...model.tuning, maxIter: parseFloat(e.target.value)}})} className={numInputClass}/></div>
                            <div><label className="block text-xs text-slate-500 font-bold uppercase mb-1">Terminal Weight Factor</label><p className="text-[10px] text-slate-600 mb-1">Multiplier on last prediction step weight</p><input type="number" step="0.1" value={model.tuning.terminalWeightFactor} onChange={e => setModel({...model, tuning: {...model.tuning, terminalWeightFactor: parseFloat(e.target.value)}})} className={numInputClass}/></div>
                        </div>
                     </div>
                )}

                {activeTab === "Export" && (
                    <div className="space-y-6 max-w-2xl">

                        {/* 1. DIRECT DEPLOy */}
                            <div className="bg-slate-900/50 p-6 rounded-xl border-2 border-indigo-600/30 overflow-hidden shadow-lg relative">
                                <div className="absolute top-0 left-0 w-full h-1 bg-gradient-to-r from-indigo-500 to-slate-500"></div>
                             <div className="flex justify-between items-start mb-4">
                                <div>
                                    <h3 className="font-bold text-white text-xl flex items-center gap-2">
                                        <Rocket className="text-indigo-300" /> Direct Push
                                    </h3>
                                    <p className="text-sm text-slate-400 mt-1">
                                        Push files to controller host / OPC server. This does not start or stop controllers.
                                    </p>
                                </div>
                             </div>

                             <div className="grid grid-cols-2 gap-4 mb-4">
                                <div>
                                    <label className="flex items-center gap-2 text-[10px] font-bold text-indigo-300 uppercase mb-1">
                                        <Network size={12}/> Target OPC Server
                                    </label>
                                    <select 
                                        value={selectedOpc}
                                        onChange={(e) => setSelectedOpc(e.target.value)}
                                        className="w-full bg-slate-800 border border-slate-700 rounded text-xs px-2 py-2 text-slate-300 focus:outline-none focus:border-indigo-500"
                                    >
                                        {infrastructure.opc_servers.length === 0 && <option value="http://127.0.0.1:9090">Default (Localhost)</option>}
                                        {infrastructure.opc_servers.map(s => (
                                            <option key={s.id} value={s.url}>
                                                {s.name} ({s.opc_endpoint || s.url})
                                            </option>
                                        ))}
                                    </select>
                                </div>
                                <div>
                                    <label className="flex items-center gap-2 text-[10px] font-bold text-indigo-400 uppercase mb-1">
                                        <Server size={12}/> Target Supervisor
                                    </label>
                                    <select 
                                        value={selectedSup}
                                        onChange={(e) => setSelectedSup(e.target.value)}
                                        className="w-full bg-slate-800 border border-slate-700 rounded text-xs px-2 py-2 text-slate-300 focus:outline-none focus:border-indigo-500"
                                    >
                                        {infrastructure.supervisors.length === 0 && <option value="http://127.0.0.1:8080">Default (Localhost)</option>}
                                        {infrastructure.supervisors.map(s => <option key={s.id} value={s.url}>{s.name} ({s.url})</option>)}
                                    </select>
                                </div>
                             </div>

                             <div className="grid grid-cols-2 gap-4">
                                <button 
                                    onClick={() => handleDirectDeploy('nodes')}
                                    disabled={isDeploying || !selectedOpc || !artifacts}
                                    className="py-3 bg-slate-700 hover:bg-slate-600 disabled:bg-slate-700 disabled:text-slate-500 text-slate-100 font-bold rounded shadow-lg transition-all flex flex-col items-center justify-center gap-1 text-xs"
                                >
                                    {isDeploying && deployStatus.includes('Nodes') ? (
                                        <div className="w-4 h-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
                                    ) : (
                                        <Network size={18} />
                                    )}
                                    Deploy Nodes (.yaml)
                                </button>

                                <button 
                                    onClick={() => handleDirectDeploy('controller')}
                                    disabled={isDeploying || !selectedSup || !artifacts}
                                    className="py-3 bg-indigo-700 hover:bg-indigo-600 disabled:bg-slate-700 disabled:text-slate-500 text-white font-bold rounded shadow-lg transition-all flex flex-col items-center justify-center gap-1 text-xs"
                                >
                                    {isDeploying && deployStatus.includes('Controller') ? (
                                        <div className="w-4 h-4 border-2 border-white/30 border-t-white rounded-full animate-spin" />
                                    ) : (
                                        <Rocket size={18} />
                                    )}
                                    Push Model (.json)
                                </button>
                             </div>

                             {deployStatus && (
                                <div className={`mt-3 text-center text-xs font-bold ${deployStatus.includes('✅') ? 'text-emerald-400' : 'text-amber-400'}`}>
                                    {deployStatus}
                                </div>
                             )}
                        </div>

                        <div className="grid grid-cols-2 gap-6 mb-8">
                            <div className="bg-slate-900/50 p-6 rounded-xl border border-slate-800 text-center">
                                <h3 className="font-bold text-white text-lg mb-2">Save Configuration</h3>
                                <button disabled={!artifacts} onClick={() => artifacts && downloadFile(`${model.meta.name || "Model"}.json`, artifacts.json, "application/json")} className="px-6 py-3 bg-indigo-700 hover:bg-indigo-600 disabled:opacity-40 text-white font-bold rounded-lg flex items-center gap-2 mx-auto"><Save size={18}/> Download .json</button>
                            </div>
                            <div className="bg-slate-900/50 p-6 rounded-xl border border-slate-800 text-center">
                                <h3 className="font-bold text-white text-lg mb-2">Export OPC Nodes</h3>
                                <button disabled={!artifacts} onClick={() => artifacts && downloadFile(`${model.meta.name || "Model"}_nodes.yaml`, artifacts.yaml, "text/yaml")} className="px-6 py-3 bg-slate-700 hover:bg-slate-600 disabled:opacity-40 text-slate-100 font-bold rounded-lg flex items-center gap-2 mx-auto"><Download size={18}/> Download .yaml</button>
                            </div>
                        </div>
                    </div>
                )}

                {activeTab === "StepResponse" && (
                    <StepResponseTool 
                        modelBinding={artifacts?.binding ?? null}
                        tssMin={model.tuning.tssMin}
                    />
                )}

                {activeTab === "Models" && (
                    <div className="space-y-6">
                        <div className="bg-slate-900/50 p-6 rounded-xl border border-slate-800">
                            <h3 className="font-bold text-white text-lg mb-4">📊 Model Visualization</h3>
                            
                            {/* Model Summary */}
                            <div className="grid grid-cols-3 gap-4 mb-6">
                                <div className="bg-slate-800/50 p-4 rounded border border-indigo-600/30">
                                    <p className="text-xs text-slate-500 uppercase font-bold mb-1">Controlled Variables</p>
                                    <p className="text-3xl font-bold text-indigo-400">{model.cvs.length}</p>
                                </div>
                                <div className="bg-slate-800/50 p-4 rounded border border-amber-600/30">
                                    <p className="text-xs text-slate-500 uppercase font-bold mb-1">Manipulated Variables</p>
                                    <p className="text-3xl font-bold text-amber-400">{model.mvs.length}</p>
                                </div>
                                <div className="bg-slate-800/50 p-4 rounded border border-pink-600/30">
                                    <p className="text-xs text-slate-500 uppercase font-bold mb-1">Disturbance Variables</p>
                                    <p className="text-3xl font-bold text-pink-400">{model.dvs.length}</p>
                                </div>
                            </div>

                            {/* Step Response Coverage */}
                            {model.mode === "step_response" && model.stepResponseData && (
                                <div className="bg-blue-900/20 border border-blue-600/30 rounded-lg p-4 mb-6">
                                    <h4 className="font-bold text-blue-300 mb-3">Step Response Coverage</h4>
                                    <div className="grid grid-cols-2 gap-4">
                                        <div>
                                            <p className="text-sm text-slate-400">MV → CV Responses:</p>
                                            <p className="text-2xl font-bold text-blue-300">
                                                {model.stepResponseData.stepCoefficients?.filter(cvData => cvData.some(mvData => mvData.some(v => v !== 0))).length || 0} / {model.cvs.length} CVs
                                            </p>
                                        </div>
                                        {model.dvs.length > 0 && model.stepResponseData.dvCoefficients && (
                                            <div>
                                                <p className="text-sm text-slate-400">DV → CV Responses:</p>
                                                <p className="text-2xl font-bold text-pink-300">
                                                    {model.stepResponseData.dvCoefficients?.filter(cvData => cvData.some(dvData => dvData.some(v => v !== 0))).length || 0} / {model.cvs.length} CVs
                                                </p>
                                            </div>
                                        )}
                                    </div>
                                </div>
                            )}

                            {/* Model Matrix Visualization */}
                            {model.mode === "step_response" && model.stepResponseData && (
                                <div className="bg-slate-900/50 border border-slate-700 rounded-lg p-4">
                                    <h4 className="font-bold text-white mb-4">📈 Model Matrix - Step Response Curves</h4>
                                    <p className="text-xs text-slate-400 mb-4">Click any graph to view details. Rows = CVs, Columns = Inputs (MVs + DVs)</p>
                                    
                                    {/* Header Row */}
                                    <div className="grid gap-1 mb-1" style={{gridTemplateColumns: `120px repeat(${model.mvs.length + model.dvs.length}, 150px)`}}>
                                        <div className="text-xs font-bold text-slate-500 p-2"></div>
                                        {model.mvs.map(mv => (
                                            <div key={mv.id} className="text-xs font-bold text-amber-400 p-2 text-center bg-amber-900/10 rounded">
                                                {mv.name}
                                            </div>
                                        ))}
                                        {model.dvs.map(dv => (
                                            <div key={dv.id} className="text-xs font-bold text-pink-400 p-2 text-center bg-pink-900/10 rounded">
                                                {dv.name}
                                            </div>
                                        ))}
                                    </div>

                                    {/* Data Rows */}
                                    <div className="overflow-auto max-h-[600px]">
                                        {model.cvs.map((cv, cvIdx) => (
                                            <div key={cv.id} className="grid gap-1 mb-1" style={{gridTemplateColumns: `120px repeat(${model.mvs.length + model.dvs.length}, 150px)`}}>
                                                {/* CV Label */}
                                                <div className="text-xs font-bold text-indigo-400 p-2 flex items-center bg-indigo-900/10 rounded">
                                                    {cv.name}
                                                </div>
                                                
                                                {/* MV Responses */}
                                                {model.mvs.map((mv, mvIdx) => {
                                                    const coeffs = model.stepResponseData?.stepCoefficients?.[cvIdx]?.[mvIdx] || [];
                                                    const hasData = coeffs.some(v => v !== 0);
                                                    const finalGain = coeffs[coeffs.length - 1] ?? 0;
                                                    const curveColor = finalGain < 0 ? '#f43f5e' : '#fbbf24';
                                                    const chartData = coeffs.map((gain, idx) => ({
                                                        time: idx * model.tuning.sampleTime,
                                                        gain
                                                    }));
                                                    
                                                    return (
                                                        <div 
                                                            key={mv.id}
                                                            onClick={() => hasData && setSelectedGraph({cvIdx, inputIdx: mvIdx, inputType: 'MV'})}
                                                            className={`border rounded ${hasData ? 'border-amber-600/30 bg-slate-800/50 cursor-pointer hover:border-amber-500 hover:shadow-lg' : 'border-slate-700/20 bg-slate-900/20'} transition`}
                                                            style={{height: '120px'}}
                                                        >
                                                            {hasData ? (
                                                                <ResponsiveContainer width="100%" height="100%">
                                                                    <LineChart data={chartData} margin={{top: 5, right: 5, bottom: 5, left: 5}}>
                                                                        <Line type="monotone" dataKey="gain" stroke={curveColor} strokeWidth={1.5} dot={false} />
                                                                    </LineChart>
                                                                </ResponsiveContainer>
                                                            ) : (
                                                                <div className="flex items-center justify-center h-full text-xs text-slate-600">No data</div>
                                                            )}
                                                        </div>
                                                    );
                                                })}
                                                
                                                {/* DV Responses */}
                                                {model.dvs.map((dv, dvIdx) => {
                                                    const coeffs = model.stepResponseData?.dvCoefficients?.[cvIdx]?.[dvIdx] || [];
                                                    const hasData = coeffs.some(v => v !== 0);
                                                    const finalGain = coeffs[coeffs.length - 1] ?? 0;
                                                    const curveColor = finalGain < 0 ? '#f43f5e' : '#a78bfa';
                                                    const chartData = coeffs.map((gain, idx) => ({
                                                        time: idx * model.tuning.sampleTime,
                                                        gain
                                                    }));
                                                    
                                                    return (
                                                        <div 
                                                            key={dv.id}
                                                            onClick={() => hasData && setSelectedGraph({cvIdx, inputIdx: dvIdx, inputType: 'DV'})}
                                                            className={`border rounded ${hasData ? 'border-pink-600/30 bg-slate-800/50 cursor-pointer hover:border-pink-500 hover:shadow-lg' : 'border-slate-700/20 bg-slate-900/20'} transition`}
                                                            style={{height: '120px'}}
                                                        >
                                                            {hasData ? (
                                                                <ResponsiveContainer width="100%" height="100%">
                                                                    <LineChart data={chartData} margin={{top: 5, right: 5, bottom: 5, left: 5}}>
                                                                        <Line type="monotone" dataKey="gain" stroke={curveColor} strokeWidth={1.5} dot={false} />
                                                                    </LineChart>
                                                                </ResponsiveContainer>
                                                            ) : (
                                                                <div className="flex items-center justify-center h-full text-xs text-slate-600">No data</div>
                                                            )}
                                                        </div>
                                                    );
                                                })}
                                            </div>
                                        ))}
                                    </div>
                                </div>
                            )}

                            {/* Parametric Mode Placeholder */}
                            {model.mode === "parametric" && (
                                <div className="bg-slate-800/30 border border-slate-700 rounded-lg p-6 text-center">
                                    <Activity className="mx-auto text-slate-600 mb-3" size={48} />
                                    <p className="text-slate-400 mb-2">Parametric Model Visualization</p>
                                    <p className="text-xs text-slate-600">Coming soon: FOPDT parameter visualization, time constant heatmap</p>
                                </div>
                            )}
                        </div>
                    </div>
                )}

                {/* Modal for Large Graph View */}
                {selectedGraph && (
                    <div 
                        className="fixed inset-0 bg-black/80 flex items-center justify-center z-50 p-8"
                        onClick={() => setSelectedGraph(null)}
                    >
                        <div 
                            className="bg-slate-900 rounded-xl border-2 border-indigo-600 p-6 max-w-4xl w-full"
                            onClick={(e) => e.stopPropagation()}
                        >
                            {(() => {
                                const cv = model.cvs[selectedGraph.cvIdx];
                                const input = selectedGraph.inputType === 'MV' 
                                    ? model.mvs[selectedGraph.inputIdx] 
                                    : model.dvs[selectedGraph.inputIdx];
                                const coeffs = selectedGraph.inputType === 'MV'
                                    ? model.stepResponseData?.stepCoefficients?.[selectedGraph.cvIdx]?.[selectedGraph.inputIdx] || []
                                    : model.stepResponseData?.dvCoefficients?.[selectedGraph.cvIdx]?.[selectedGraph.inputIdx] || [];
                                const chartData = coeffs.map((gain, idx) => ({
                                    time: idx * model.tuning.sampleTime,
                                    gain
                                }));
                                const finalGain = coeffs[coeffs.length - 1] ?? 0;
                                const color = finalGain < 0
                                    ? '#f43f5e'
                                    : (selectedGraph.inputType === 'MV' ? '#fbbf24' : '#a78bfa');

                                return (
                                    <>
                                        <div className="flex justify-between items-start mb-4">
                                            <div>
                                                <h3 className="text-2xl font-bold text-white mb-2">
                                                    {input.name} → {cv.name}
                                                </h3>
                                                <p className="text-sm text-slate-400">
                                                    {selectedGraph.inputType} step response | {coeffs.length} points @ {model.tuning.sampleTime}s intervals
                                                </p>
                                            </div>
                                            <button 
                                                onClick={() => setSelectedGraph(null)}
                                                className="text-slate-400 hover:text-white text-2xl font-bold px-3 py-1"
                                            >
                                                ×
                                            </button>
                                        </div>
                                        
                                        <ResponsiveContainer width="100%" height={400}>
                                            <LineChart data={chartData}>
                                                <CartesianGrid strokeDasharray="3 3" stroke="#475569" />
                                                <XAxis 
                                                    dataKey="time" 
                                                    stroke="#94a3b8"
                                                    label={{value: 'Time (s)', position: 'insideBottom', offset: -5}}
                                                />
                                                <YAxis 
                                                    stroke="#94a3b8"
                                                    label={{value: 'Gain (ΔCV/ΔInput)', angle: -90, position: 'insideLeft'}}
                                                />
                                                <Tooltip 
                                                    contentStyle={{backgroundColor: '#1e293b', border: '1px solid #475569'}}
                                                    labelFormatter={(val) => `Time: ${val}s`}
                                                />
                                                <Line 
                                                    type="monotone" 
                                                    dataKey="gain" 
                                                    stroke={color} 
                                                    strokeWidth={3} 
                                                    dot={{r: 3}}
                                                    name="Gain"
                                                />
                                            </LineChart>
                                        </ResponsiveContainer>

                                        <div className="grid grid-cols-3 gap-4 mt-4 text-sm">
                                            <div className="bg-slate-800/50 rounded p-3">
                                                <p className="text-slate-500 mb-1">Initial Gain</p>
                                                <p className="text-xl font-bold" style={{color}}>{coeffs[0]?.toFixed(4) || 0}</p>
                                            </div>
                                            <div className="bg-slate-800/50 rounded p-3">
                                                <p className="text-slate-500 mb-1">Final Gain</p>
                                                <p className="text-xl font-bold" style={{color}}>{coeffs[coeffs.length - 1]?.toFixed(4) || 0}</p>
                                            </div>
                                            <div className="bg-slate-800/50 rounded p-3">
                                                <p className="text-slate-500 mb-1">Steady State Time</p>
                                                <p className="text-xl font-bold text-slate-300">{model.tuning.tssMin} min</p>
                                            </div>
                                        </div>
                                    </>
                                );
                            })()}
                        </div>
                    </div>
                )}
            </div>
            
            {/* MODEL LOAD MODAL */}
            {showModelLoadModal && (
                <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-50 p-4">
                    <div className="bg-slate-900 border-2 border-slate-700 rounded-xl w-full max-w-3xl max-h-[80vh] overflow-hidden flex flex-col">
                        <div className="bg-gradient-to-r from-emerald-900 to-emerald-800 px-6 py-4 border-b-2 border-slate-700 flex justify-between items-center">
                            <h2 className="text-xl font-bold text-white flex items-center gap-2">
                                <Server size={24} />
                                Load Model from Controller
                            </h2>
                            <button 
                                onClick={() => setShowModelLoadModal(false)}
                                className="text-slate-300 hover:text-white transition"
                            >
                                ✕
                            </button>
                        </div>
                        
                        <div className="flex-1 overflow-y-auto p-6">
                            {loadingControllers ? (
                                <div className="text-center py-12 text-slate-400">
                                    <RefreshCw size={32} className="animate-spin mx-auto mb-4" />
                                    Loading controllers...
                                </div>
                            ) : controllers.length === 0 ? (
                                <div className="text-center py-12">
                                    <AlertCircle size={48} className="text-amber-500 mx-auto mb-4" />
                                    <p className="text-slate-400 text-lg">No controllers found</p>
                                    <p className="text-slate-500 text-sm mt-2">Make sure controller_host is running</p>
                                </div>
                            ) : (
                                <div className="space-y-4">
                                    {controllers.map(controller => {
                                        const isRunning = controller.state !== null && typeof controller.state === 'object' && 'Running' in controller.state && controller.state.Running !== undefined;
                                        return (
                                            <div key={controller.id} className="bg-slate-800 rounded-lg border-2 border-slate-700 overflow-hidden">
                                                <div className={`px-4 py-3 flex items-center justify-between ${isRunning ? 'bg-emerald-900/30 border-b-2 border-emerald-700/50' : 'border-b border-slate-700'}`}>
                                                    <div className="flex items-center gap-3">
                                                        <div className={`w-3 h-3 rounded-full ${isRunning ? 'bg-emerald-400 animate-pulse' : 'bg-slate-600'}`}></div>
                                                        <h3 className="font-bold text-white text-lg">{controller.id}</h3>
                                                        {isRunning && controller.active_model && (
                                                            <span className="px-2 py-1 bg-emerald-600 text-white text-xs font-bold rounded">
                                                                Running: {controller.active_model}
                                                            </span>
                                                        )}
                                                    </div>
                                                    <span className="text-slate-400 text-sm">
                                                        {controller.models.length} model{controller.models.length !== 1 ? 's' : ''}
                                                    </span>
                                                </div>
                                                
                                                {controller.models.length > 0 ? (
                                                    <div className="p-4 space-y-2">
                                                        {controller.models.map(modelFile => {
                                                            const isActive = controller.active_model === modelFile;
                                                            return (
                                                                <button
                                                                    key={modelFile}
                                                                    onClick={() => handleLoadModelFromController(controller.id, modelFile)}
                                                                    className={`w-full text-left px-4 py-3 rounded-lg border-2 transition flex items-center justify-between group ${
                                                                        isActive 
                                                                        ? 'bg-emerald-900/20 border-emerald-600 hover:bg-emerald-900/30' 
                                                                        : 'bg-slate-900/50 border-slate-700 hover:border-indigo-500 hover:bg-slate-800'
                                                                    }`}
                                                                >
                                                                    <div className="flex items-center gap-3">
                                                                        <FileText size={20} className={isActive ? 'text-emerald-400' : 'text-slate-400 group-hover:text-indigo-400'} />
                                                                        <span className={`font-mono ${isActive ? 'text-emerald-300 font-bold' : 'text-slate-300'}`}>
                                                                            {modelFile}
                                                                        </span>
                                                                    </div>
                                                                    {isActive ? (
                                                                        <span className="px-2 py-1 bg-emerald-600 text-white text-xs font-bold rounded">
                                                                            ACTIVE
                                                                        </span>
                                                                    ) : (
                                                                        <Download size={16} className="text-slate-500 group-hover:text-indigo-400" />
                                                                    )}
                                                                </button>
                                                            );
                                                        })}
                                                    </div>
                                                ) : (
                                                    <div className="p-4 text-center text-slate-500 text-sm">
                                                        No models available
                                                    </div>
                                                )}
                                            </div>
                                        );
                                    })}
                                </div>
                            )}
                        </div>
                        
                        <div className="px-6 py-4 bg-slate-800/50 border-t-2 border-slate-700 flex justify-end">
                            <button 
                                onClick={() => setShowModelLoadModal(false)}
                                className="px-4 py-2 bg-slate-700 hover:bg-slate-600 rounded-lg text-white font-bold transition"
                            >
                                Cancel
                            </button>
                        </div>
                    </div>
                </div>
            )}
        </div>
    );
};