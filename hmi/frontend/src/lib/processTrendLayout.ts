import type { TrendVariable } from '../types/ProcessTrend';

export type TrendRoleBlock = {
  role: TrendVariable['role'];
  configured: number;
  lanes: TrendVariable[][];
};

// Keep missing roles distinct from configured roles whose variables are hidden.
export function trendRoleBlocks(variables: TrendVariable[], selected: string[], mobile: boolean): TrendRoleBlock[] {
  const roles: TrendVariable['role'][] = ['CV', 'MV', 'DV'];
  if (variables.some(variable => variable.role === 'OTHER')) roles.push('OTHER');
  return roles.map(role => {
    const configured = variables.filter(variable => variable.role === role);
    const visible = configured.filter(variable => selected.includes(variable.id));
    return { role, configured: configured.length,
      lanes: !mobile && visible.length <= 2 && visible.length > 0 ? [visible] : visible.map(variable => [variable]) };
  });
}

// Recharts adds visible Y-axis widths to chart margins. Reserve the unused
// right-axis slot for single-variable lanes so every timestamp has the same X.
export function trendRoleGeometry(variableCount: number, mobile: boolean) {
  const axisWidth = mobile ? 52 : 65;
  return { axisWidth, margin: { top: 12, right: 12 + (variableCount < 2 ? axisWidth : 0), bottom: 4, left: 0 } };
}
