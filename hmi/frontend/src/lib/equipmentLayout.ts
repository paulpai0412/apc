import type { EquipmentConnection, EquipmentItem, EquipmentKind, EquipmentLayout, EquipmentView } from '../types/EquipmentView';

/** Illustrative topology only: never infer a mill type or a binding from model names. */
export function createEquipmentTemplate(layout: EquipmentLayout): EquipmentView {
  const equipment: EquipmentItem[] = [];
  const connections: EquipmentConnection[] = [];
  const add = (id: string, name: string, kind: EquipmentKind, position: EquipmentItem['position'], size: EquipmentItem['size']) => {
    equipment.push({ id, name, kind, position, size, rotation: [0, 0, 0] });
  };
  const link = (id: string, from: string, to: string, kind: EquipmentConnection['kind'] = 'material') => {
    connections.push({ id, from, to, kind });
  };
  add('raw-silo', 'Raw material silo', 'silo', [-13, 0, 0], [3, 7, 3]);
  add('feed-meter', 'Weigh feeder', 'feeder', [-9, 0, 0], [3, 2, 2]);
  add('feed-conveyor', 'Feed conveyor', 'conveyor', [-5, 0, 0], [4, 1.5, 1.5]);
  if (layout === 'closed_ball_mill') {
    add('mill', 'Ball mill', 'ball_mill', [0, 0, 0], [6, 3.8, 3.8]);
    add('elevator', 'Bucket elevator', 'elevator', [5, 0, -1], [1.8, 9, 1.8]);
    add('separator', 'Dynamic separator', 'separator', [8, 5, -4], [3.3, 4.5, 3.3]);
    add('return-conveyor', 'Coarse return conveyor', 'conveyor', [1, 0, -6], [7, 1.5, 1.5]);
    link('mill-discharge', 'mill', 'elevator');
    link('elevator-discharge', 'elevator', 'separator');
    link('coarse-reject', 'separator', 'return-conveyor');
    link('coarse-return', 'return-conveyor', 'mill');
  } else {
    add('mill', 'Vertical roller mill', 'vertical_mill', [0, 0, 0], [5, 7, 5]);
    add('separator', 'Mill top separator', 'separator', [0, 7.2, 0], [3.7, 3, 3.7]);
    add('reject-elevator', 'External reject elevator', 'elevator', [5, 0, -4], [1.8, 8, 1.8]);
    add('return-conveyor', 'Reject return conveyor', 'conveyor', [-2, 0, -6], [6, 1.5, 1.5]);
    link('mill-separation', 'mill', 'separator');
    link('external-reject', 'mill', 'reject-elevator');
    link('reject-return', 'reject-elevator', 'return-conveyor');
    link('return-feed', 'return-conveyor', 'mill');
  }
  add('collector', 'Product bag filter', 'collector', [11, 0, 3], [4.5, 7, 3.5]);
  add('fan', 'Process exhaust fan', 'fan', [15, 0, -3], [3, 3.5, 2.5]);
  add('product-conveyor', 'Product conveyor', 'conveyor', [15, 0, 4], [4, 1.5, 1.5]);
  add('product-silo', 'Finished cement silo', 'silo', [20, 0, 4], [3.5, 8, 3.5]);
  link('raw-feed', 'raw-silo', 'feed-meter');
  link('metered-feed', 'feed-meter', 'feed-conveyor');
  link('mill-feed', 'feed-conveyor', 'mill');
  link('fine-product', 'separator', 'collector');
  link('process-air', 'mill', 'collector', 'air');
  link('clean-air', 'collector', 'fan', 'air');
  link('collected-product', 'collector', 'product-conveyor');
  link('product-storage', 'product-conveyor', 'product-silo');
  return {
    schema_version: 1,
    label: layout === 'closed_ball_mill' ? 'Unconfirmed closed ball mill template' : 'Unconfirmed vertical roller mill template',
    layout,
    confirmed: false,
    equipment,
    connections,
    bindings: [],
  };
}
