import React from 'react';
import { ProcessTable } from './ProcessTable';
import type { RuntimeVariable } from '../lib/modelBinding';
import type { UserRole } from '../lib/api';

interface TuningTabProps {
  cvList: RuntimeVariable[];
  mvList: RuntimeVariable[];
  wsRef: React.RefObject<WebSocket | null>;
  systemPrefix: string;
  currentRole: UserRole;
}

export const TuningTab: React.FC<TuningTabProps> = ({ cvList, mvList, wsRef, systemPrefix, currentRole }) => <div className="space-y-6">
  <ProcessTable title="Controlled Variables (CVs) - Tuning Parameters" rowHeaders={cvList} wsRef={wsRef} currentRole={currentRole} columns={[
    { header: 'Description', suffix: 'Description' }, { header: 'Weight', suffix: 'Weight' }, { header: 'Alpha', suffix: 'Alpha' },
    { header: 'Low Limit', suffix: 'LowLimit' }, { header: 'High Limit', suffix: 'HighLimit' },
    { header: 'Low Low', suffix: 'LowLowLimit' }, { header: 'High High', suffix: 'HighHighLimit' },
  ]} />
  <ProcessTable title="Manipulated Variables (MVs) - Tuning Parameters" rowHeaders={mvList} wsRef={wsRef} currentRole={currentRole} columns={[
    { header: 'Description', suffix: 'Description' },
    { header: 'Low Limit', suffix: 'LowLimit' }, { header: 'High Limit', suffix: 'HighLimit' }, { header: 'Weight', suffix: 'Weight' },
  ]} />
  <p className="rounded border border-slate-700 p-3 text-xs text-slate-400">{systemPrefix} 的 P／M／取樣時間由模型版本決定，不支援線上修改。變更模型後重新啟動觀察，再明確要求接管。未提供的限制不會產生推測節點。</p>
</div>;
