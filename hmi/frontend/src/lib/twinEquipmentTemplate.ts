import type { EquipmentLayout, EquipmentView } from '../types/EquipmentView';

/** Illustration only. Calling this never guesses a tag, role, unit or physical topology. */
export function unboundEquipmentTemplate(layout: EquipmentLayout = 'closed_ball_mill'): EquipmentView {
  return {
    schema_version: 1, label: '水泥磨設備草稿（未確認）', layout, confirmed: false,
    equipment: [
      { id: 'feed', name: '入料設備', kind: 'feeder', position: [-9, 0, 0], size: [3, 3, 3], rotation: [0, 0, 0] },
      { id: 'mill', name: '磨機（待工程確認）', kind: layout === 'vertical_mill' ? 'vertical_mill' : 'ball_mill', position: [0, 0, 0], size: [6, 4, 4], rotation: [0, 0, 0] },
      { id: 'separator', name: '選粉設備', kind: 'separator', position: [8, 0, 0], size: [3, 6, 3], rotation: [0, 0, 0] },
      { id: 'fan', name: '風機', kind: 'fan', position: [8, 0, 8], size: [3, 3, 3], rotation: [0, 0, 0] },
    ],
    connections: [], bindings: [],
  };
}
