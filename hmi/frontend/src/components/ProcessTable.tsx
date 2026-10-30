import React, { useState } from 'react';
import { ValueEditorModal } from './ValueEditorModal';
import { useTagStore } from '../store/tagStore';
import { useWriteNode } from '../hooks/useWriteNode';
import type { UserRole } from '../lib/api';
import { runtimeNodeId } from '../lib/modelBinding';
import type { RuntimeVariable } from '../lib/modelBinding';

export interface ColumnDef {
    header: string;
    suffix: string;
}

interface ProcessTableProps {
  title: string;
  rowHeaders: RuntimeVariable[];
  wsRef: React.RefObject<WebSocket | null>;
  columns: ColumnDef[];
    currentRole: UserRole;
}

interface EditingField {
    nodeId: string;
    title: string;
    currentValue: string;
    unit?: string;
}

export const ProcessTable: React.FC<ProcessTableProps> = ({ title, rowHeaders, wsRef, columns, currentRole }) => {
  const tags = useTagStore(s => s.tags);
  const [editingField, setEditingField] = useState<EditingField | null>(null);
    const [deniedReason, setDeniedReason] = useState<string | null>(null);

    const canRoleEditSuffix = (role: UserRole, suffix: string): boolean => {
            if (role === 'admin' || role === 'engineer') return true;
            if (role === 'operator') return ['Target', 'SP', 'ModeTarget', 'LowLimit', 'HighLimit'].includes(suffix);
            return false;
    };

    const deniedReasonForRole = (role: UserRole, suffix: string): string | null => {
            if (canRoleEditSuffix(role, suffix)) return null;
            if (role === 'viewer') return 'viewer role is read-only';
            if (role === 'operator' && (suffix === 'LowLowLimit' || suffix === 'HighHighLimit')) {
                    return 'operator cannot edit safety limits (LowLow/HighHigh)';
            }
            return 'insufficient permissions for this write';
    };
  
  const { isWriting, write } = useWriteNode(
    editingField?.nodeId || '',
    wsRef,
    {
            onSuccess: () => {
                setEditingField(null);
                setDeniedReason(null);
            }
    }
  );


  const EDITABLE_SUFFIXES = ["Target", "SP", "ModeTarget", "LowLimit", "HighLimit", "LowLowLimit", "HighHighLimit", "Weight", "Alpha"];

  const handleFieldClick = (row: RuntimeVariable, colSuffix: string, currentVal: unknown) => {
      const nodeId = runtimeNodeId(row, colSuffix);
      if (nodeId && typeof currentVal === 'number' && Number.isFinite(currentVal) && EDITABLE_SUFFIXES.includes(colSuffix)) {
          const denyReason = deniedReasonForRole(currentRole, colSuffix);
          const friendlyName = `${row.name} ${colSuffix}`;
          setDeniedReason(denyReason);
          setEditingField({
              nodeId,
              title: friendlyName,
              currentValue: String(currentVal),
              unit: ['Weight', 'Alpha', 'ModeTarget'].includes(colSuffix) ? undefined : row.units || 'Units unknown'
          });
      }
  };

  return (
    <div className="bg-slate-900/50 rounded-xl border-2 border-slate-700/50 overflow-hidden mb-4 shadow-lg">
      <div className="bg-gradient-to-r from-slate-900 to-slate-800 px-4 py-2 border-b-2 border-slate-700 flex justify-between items-center">
        <h3 className="font-extrabold text-slate-100 text-sm tracking-wide uppercase">{title}</h3>
        <span className="text-[10px] uppercase font-bold text-slate-400 bg-slate-900/80 px-2 py-1 rounded-md border border-slate-700">
            {rowHeaders.length} {rowHeaders.length === 1 ? 'Loop' : 'Loops'}
        </span>
      </div>

      <div className="overflow-x-auto" tabIndex={0} role="region" aria-label={`${title}；可橫向捲動查看各欄位`}>
        <table className="w-full min-w-max text-left text-xs">
          <thead>
            <tr className="border-b-2 border-slate-700 bg-slate-900/60 text-slate-300">
              <th className="px-3 py-2 font-extrabold uppercase tracking-wider text-[13px] w-24">Tag</th>
              {columns.map(c => (
                  <th key={c.header} className={`px-2 py-2 font-extrabold uppercase tracking-wider text-right text-[13px] ${c.suffix === 'Description' ? 'hidden xl:table-cell' : ''}`}>{c.header}</th>
              ))}
            </tr>
          </thead>
          <tbody className="divide-y divide-slate-800/50">
            {rowHeaders.map((rowTag) => (
              <tr key={rowTag.name} className="hover:bg-slate-800/40 transition-colors group">
                <td className="px-3 py-1 font-mono text-[11px] text-slate-400 group-hover:text-indigo-300 transition-colors truncate" title={rowTag.description}>
                    {rowTag.name}<span className="block text-slate-500">{rowTag.units || 'Units unknown'}</span>
                </td>

                {columns.map((col) => {
                    const nodeId = runtimeNodeId(rowTag, col.suffix);
                    const tagData = nodeId ? tags[nodeId] : undefined;
                    const value = (field: string) => {
                        const id = runtimeNodeId(rowTag, field);
                        const tag = id ? tags[id] : undefined;
                        return tag && Number.isInteger(tag.status) && tag.status >= 0 && tag.status <= 0xffffffff && (tag.status >>> 30) === 0
                            && typeof tag.value === 'number' && Number.isFinite(tag.value) ? tag.value : undefined;
                    };
                    
                    // Special handling for non-OPC columns
                    let val: number | string | undefined;
                    let displayText: string;
                    
                    if (col.suffix === "Description") {
                        val = rowTag.description || "-";
                        displayText = val;
                    } else if (col.suffix === "Status") {
                        const pv = value('PV');
                        const mode = value('Mode');
                        if (rowTag.role === 'MV') {
                            // PID mode is not proof that an engine command was accepted.
                            // 0 = Operator Control, 1 = Local Auto, 2 = Cascade, 3 = Remote Cascade
                            switch (mode) {
                                case 3:
                                    val = "Remote Cascade";
                                    displayText = "Remote Cascade";
                                    break;
                                case 2:
                                    val = "DCS Cascade";
                                    displayText = "DCS Cascade";
                                    break;
                                case 1:
                                    val = "Local Auto";
                                    displayText = "Local Auto";
                                    break;
                                case 0:
                                    val = "Operator";
                                    displayText = "Operator";
                                    break;
                                default:
                                    val = "Unknown Mode";
                                    displayText = "Unknown Mode";
                            }
                        } else {
                            const target = value('Target');
                            const low = value('LowLimit');
                            const high = value('HighLimit');
                            const lowlow = value('LowLowLimit');
                            const highhigh = value('HighHighLimit');
                            
                            if (pv !== undefined && low !== undefined && high !== undefined) {
                                if (lowlow !== undefined && pv <= lowlow) {
                                    val = "Low Alarm";
                                    displayText = "Low Alarm";
                                } else if (highhigh !== undefined && pv >= highhigh) {
                                    val = "High Alarm";
                                    displayText = "High Alarm";
                                } else if (pv <= low) {
                                    val = "Low Warning";
                                    displayText = "Low Warning";
                                } else if (pv >= high) {
                                    val = "High Warning";
                                    displayText = "High Warning";
                                } else if (target !== undefined && Math.abs(pv - target) < 0.5) {
                                    val = "On Target";
                                    displayText = "On Target";
                                } else {
                                    val = "Normal";
                                    displayText = "Normal";
                                }
                            } else {
                                val = "No Data";
                                displayText = "No Data";
                            }
                        }
                    } else {
                        val = value(col.suffix);
                        displayText = val === undefined ? (nodeId ? 'Unavailable' : 'Not configured') : val.toFixed(2);
                    }
                    
                    const isEditableCol = Boolean(nodeId && tagData && typeof val === 'number' && EDITABLE_SUFFIXES.includes(col.suffix));
                    
                    // --- Constraint Logic ---
                    // Fetch limits even if they are not columns for this table
                    const highLim = value('HighLimit');
                    const lowLim = value('LowLimit');

                    let constraintStyle = "";
                    const valNum = typeof val === 'number' ? val : parseFloat(String(val));
                    
                    // PV limits do not describe the independent PID output quantity.
                    if (col.suffix === "PV" && !isNaN(valNum)) {
                        if (highLim !== undefined && valNum >= highLim) {
                             constraintStyle = "bg-amber-900/40 text-amber-200 shadow-[0_0_10px_rgba(245,158,11,0.2)] font-black border border-amber-500/30 rounded px-1";
                        } else if (lowLim !== undefined && valNum <= lowLim) {
                             constraintStyle = "bg-amber-900/40 text-amber-200 shadow-[0_0_10px_rgba(245,158,11,0.2)] font-black border border-amber-500/30 rounded px-1";
                        }
                    }

                    return (
                        <td key={col.header} className={`px-2 py-1 text-right text-xs ${col.suffix === 'Description' ? 'hidden xl:table-cell' : ''}`}>
                            <span 
                                onClick={() => isEditableCol && handleFieldClick(rowTag, col.suffix, val)}
                                className={`
                                    inline-block cursor-default transition-all duration-150 px-1 rounded-sm
                                    ${constraintStyle}
                                    ${isEditableCol ? 'hover:text-emerald-400 cursor-pointer hover:scale-105' : ''}
                                    ${!constraintStyle && (col.suffix === "PV" || col.suffix === "OP") ? 'text-cyan-300 font-bold text-[13px]' : ''}
                                    ${!constraintStyle && col.suffix !== "PV" && col.suffix !== "OP" ? 'font-mono text-slate-300' : ''}
                                    ${col.suffix === "Status" ? (() => {
                                        // MV Status Colors
                                        if (displayText === "Remote Cascade") return 'text-emerald-400 font-bold text-[11px] uppercase tracking-wide';
                                        if (displayText === "DCS Cascade") return 'text-blue-400 font-semibold text-[11px] uppercase tracking-wide';
                                        if (displayText === "Local Auto") return 'text-yellow-400 font-semibold text-[11px] uppercase tracking-wide';
                                        if (displayText === "Operator") return 'text-orange-400 font-semibold text-[11px] uppercase tracking-wide';
                                        if (displayText === "Unknown Mode") return 'text-red-400 font-semibold text-[11px] uppercase tracking-wide';
                                        
                                        // CV Status Colors
                                        if (displayText === "Low Alarm" || displayText === "High Alarm") return 'text-red-400 font-bold text-[11px] uppercase tracking-wide';
                                        if (displayText === "Low Warning" || displayText === "High Warning") return 'text-yellow-400 font-semibold text-[11px] uppercase tracking-wide';
                                        if (displayText === "On Target") return 'text-emerald-400 font-semibold text-[11px] uppercase tracking-wide';
                                        if (displayText === "Normal") return 'text-slate-400 font-normal text-[11px] uppercase';
                                        return 'text-slate-500 font-normal text-[11px] uppercase';
                                    })() : ''}
                                    ${col.suffix === "Description" ? 'max-w-48 whitespace-normal text-slate-400 text-[11px] font-normal' : ''}
                                `}
                            >
                                {displayText}
                            </span>
                        </td>
                    );
                })}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      
      <ValueEditorModal
        isOpen={editingField !== null}
                onCancel={() => {
                    setEditingField(null);
                    setDeniedReason(null);
                }}
        title={editingField?.title || ""}
        unit={editingField?.unit}
        currentValue={editingField?.currentValue ? parseFloat(editingField.currentValue) : 0}
        onConfirm={write}
        isWriting={isWriting}
                deniedReason={deniedReason}
      />
    </div>
  );
};